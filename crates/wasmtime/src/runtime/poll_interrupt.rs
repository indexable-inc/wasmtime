//! Poll-page interruption (ix fork); see [`Config::poll_interruption`].
//!
//! Each [`Store`](crate::Store) owns one page, mapped read-only, whose address
//! is written into every instance's vmctx. Compiled code loads from it at each
//! function entry and loop header. [`InterruptHandle::interrupt`] maps it
//! `PROT_NONE`, so the next poll faults; the trap metadata at that PC says
//! `Trap::Interrupt`, and the trap path checks that the faulting address is
//! this store's page (see `trap::from_runtime_box`).
//!
//! # Pages come from an arena and are reused
//!
//! Pages are carved from process-wide arena chunks that are mapped once and
//! never unmapped. A chunk is one `PROT_NONE` reservation of
//! `2 * CHUNK_PAGES + 1` pages; every odd page is replaced by its own
//! `MAP_SHARED | MAP_ANONYMOUS` read-only mapping:
//!
//! ```text
//! | gap | poll | gap | poll | gap | ... | poll | gap |
//! ```
//!
//! Each poll page is its own VMA, and it can never merge with a neighbour:
//! the gaps are private anonymous memory and every shared anonymous mapping
//! has its own backing object. So `mprotect` of a poll page only rewrites the
//! flags of one whole VMA; it never splits or merges VMAs, needs no new VMA,
//! and doesn't change the process's map count. That is what keeps
//! `interrupt` and `rearm` from failing with `ENOMEM` (a split needs an
//! allocation and can hit `vm.max_map_count`). They still return `Result`,
//! and a page whose protection change ever failed is never handed out again.
//!
//! When a store and every handle to its page are gone, the page is re-armed
//! (if it was revoked) and goes back on a free list for the next store; it is
//! never unmapped. A handle is reference counted, so `mprotect` never lands on
//! a page that another store owns now.
//!
//! [`Config::poll_interruption`]: crate::Config::poll_interruption

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::ptr::NonNull;
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use rustix::mm::{MapFlags, MprotectFlags, ProtFlags};
use std::sync::Mutex;

/// Poll pages per arena chunk.
const CHUNK_PAGES: usize = 64;

/// Free poll pages (their addresses), readable and not owned by any store.
static FREE: Mutex<Vec<usize>> = Mutex::new(Vec::new());

/// Pages that were mapped into the arena, and pages withdrawn from reuse
/// because a protection change failed. For tests and diagnostics.
static MAPPED: AtomicUsize = AtomicUsize::new(0);
static QUARANTINED: AtomicUsize = AtomicUsize::new(0);

fn page_size() -> usize {
    crate::runtime::vm::host_page_size()
}

/// Take a free poll page, mapping a new arena chunk when there is none.
fn take_page() -> crate::Result<NonNull<AtomicU64>> {
    let mut free = FREE.lock().unwrap_or_else(|e| e.into_inner());
    if free.is_empty() {
        grow(&mut free)?;
    }
    let addr = free.pop().expect("grow added pages");
    Ok(NonNull::new(addr as *mut AtomicU64).expect("arena pages are not null"))
}

/// Map one arena chunk and put its poll pages on `free`.
fn grow(free: &mut Vec<usize>) -> crate::Result<()> {
    let page = page_size();
    let len = (2 * CHUNK_PAGES + 1) * page;
    // SAFETY: a fresh anonymous reservation at an address the kernel picks.
    // It is never unmapped: pages from it may be referenced by stores for the
    // rest of the process.
    let base = unsafe {
        rustix::mm::mmap_anonymous(core::ptr::null_mut(), len, ProtFlags::empty(), MapFlags::PRIVATE)
    }
    .map_err(|e| crate::format_err!("reserving the poll-page arena failed: {e}"))?;
    let mut added = 0;
    let mut err = None;
    for i in 0..CHUNK_PAGES {
        let addr = base as usize + (2 * i + 1) * page;
        // SAFETY: `addr` is one page inside the reservation above, which we
        // own and nothing else uses; MAP_FIXED replaces just that page.
        let r = unsafe {
            rustix::mm::mmap_anonymous(
                addr as *mut _,
                page,
                ProtFlags::READ,
                MapFlags::SHARED | MapFlags::FIXED,
            )
        };
        match r {
            Ok(p) if p as usize == addr => {
                // Fault it in now, so the first poll in a store doesn't.
                // SAFETY: the page is mapped readable and 8-aligned.
                unsafe { core::ptr::read_volatile(addr as *const u64) };
                free.push(addr);
                added += 1;
            }
            Ok(_) => {
                err = Some(crate::format_err!("MAP_FIXED poll page landed at the wrong address"));
                break;
            }
            Err(e) => {
                err = Some(crate::format_err!("mapping a poll page failed: {e}"));
                break;
            }
        }
    }
    MAPPED.fetch_add(added, Ordering::Relaxed);
    match (added, err) {
        // Some pages are usable; the rest of the chunk stays PROT_NONE.
        (1.., _) => Ok(()),
        (0, Some(e)) => Err(e),
        (0, None) => unreachable!("CHUNK_PAGES > 0"),
    }
}

struct PollPage {
    base: NonNull<AtomicU64>,
    /// Set while the page is inaccessible. Read without the lock on the
    /// re-arm fast path; written only under `lock`.
    revoked: AtomicBool,
    /// Set once a protection change of this page failed: its state is then
    /// unknown, so it never goes back to the free list.
    failed: AtomicBool,
    /// Serializes `interrupt` and `rearm`, so the page's protection always
    /// matches `revoked` once the lock is released.
    lock: Mutex<()>,
}

// SAFETY: the page is never written through `base`; the pointer is only
// handed to compiled code and to `mprotect`.
unsafe impl Send for PollPage {}
// SAFETY: as above; all state changes go through `lock`.
unsafe impl Sync for PollPage {}

impl PollPage {
    fn protect(&self, flags: MprotectFlags) -> crate::Result<()> {
        // SAFETY: the page is an arena page owned by this `PollPage` until it
        // is dropped; it is a whole VMA of its own (see the module docs).
        unsafe { rustix::mm::mprotect(self.base.as_ptr().cast(), page_size(), flags) }.map_err(|e| {
            self.failed.store(true, Ordering::Relaxed);
            crate::format_err!("mprotect of the poll page failed: {e}")
        })
    }
}

impl Drop for PollPage {
    fn drop(&mut self) {
        // The store and every handle are gone. Hand the page to the next
        // store only if it is readable again; otherwise keep it out of use.
        let ok = !self.failed.load(Ordering::Relaxed)
            && (!self.revoked.load(Ordering::Relaxed) || self.protect(MprotectFlags::READ).is_ok());
        if ok {
            FREE.lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(self.base.as_ptr() as usize);
        } else {
            QUARANTINED.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// Interrupts the wasm running in one [`Store`](crate::Store), from any
/// thread. Created by [`Store::interrupt_handle`](crate::Store::interrupt_handle)
/// when [`Config::poll_interruption`](crate::Config::poll_interruption) is on.
#[derive(Clone)]
pub struct InterruptHandle(Arc<PollPage>);

impl InterruptHandle {
    pub(crate) fn new() -> crate::Result<Self> {
        Ok(InterruptHandle(Arc::new(PollPage {
            base: take_page()?,
            revoked: AtomicBool::new(false),
            failed: AtomicBool::new(false),
            lock: Mutex::new(()),
        })))
    }

    /// The address compiled code polls.
    pub(crate) fn poll_ptr(&self) -> NonNull<AtomicU64> {
        self.0.base
    }

    /// Whether `addr` lies in this poll page.
    pub(crate) fn contains(&self, addr: usize) -> bool {
        let start = self.0.base.as_ptr() as usize;
        addr >= start && addr - start < page_size()
    }

    /// Make the poll page inaccessible: the next poll in this store's wasm
    /// traps with [`Trap::Interrupt`](crate::Trap::Interrupt). Wasm that is
    /// not running yet traps at its first function entry. Idempotent.
    ///
    /// When this returns `Ok`, the kernel has changed the page tables and
    /// flushed every CPU's TLB for the page, so no later poll on any thread
    /// can read it.
    ///
    /// # Errors
    ///
    /// When `mprotect` fails the page stays readable and the store's wasm is
    /// *not* interrupted. The caller may retry; the page is never reused by
    /// another store.
    pub fn interrupt(&self) -> crate::Result<()> {
        let _g = self.0.lock.lock().unwrap_or_else(|e| e.into_inner());
        if !self.0.revoked.load(Ordering::Relaxed) {
            self.0.protect(MprotectFlags::empty())?;
            self.0.revoked.store(true, Ordering::Release);
        }
        Ok(())
    }

    /// Make the poll page readable again, so wasm runs. Returns whether it had
    /// been interrupted. Costs one atomic load when it was not.
    ///
    /// # Errors
    ///
    /// When `mprotect` fails the page stays revoked: every call into the
    /// store traps with `Trap::Interrupt` at its first poll. Drop the store.
    pub fn rearm(&self) -> crate::Result<bool> {
        if !self.0.revoked.load(Ordering::Acquire) {
            return Ok(false);
        }
        let _g = self.0.lock.lock().unwrap_or_else(|e| e.into_inner());
        if !self.0.revoked.load(Ordering::Relaxed) {
            return Ok(false);
        }
        self.0.protect(MprotectFlags::READ)?;
        self.0.revoked.store(false, Ordering::Release);
        Ok(true)
    }

    /// Whether [`InterruptHandle::interrupt`] took effect and
    /// [`InterruptHandle::rearm`] has not run since.
    pub fn is_interrupted(&self) -> bool {
        self.0.revoked.load(Ordering::Acquire)
    }

    /// Process-wide poll-page counts: `(mapped, free, quarantined)`. Pages are
    /// never unmapped, so `mapped` only grows; it stays at the peak number of
    /// live stores, rounded up to a whole arena chunk.
    #[doc(hidden)]
    pub fn arena_stats() -> (usize, usize, usize) {
        let free = FREE.lock().unwrap_or_else(|e| e.into_inner()).len();
        (MAPPED.load(Ordering::Relaxed), free, QUARANTINED.load(Ordering::Relaxed))
    }
}

impl core::fmt::Debug for InterruptHandle {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("InterruptHandle")
            .field("page", &self.0.base)
            .field("interrupted", &self.is_interrupted())
            .finish()
    }
}
