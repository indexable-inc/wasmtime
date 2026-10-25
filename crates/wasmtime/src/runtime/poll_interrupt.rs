//! Poll interruption (ix fork); see [`Config::poll_interruption`].
//!
//! Each [`Store`](crate::Store) owns one *poll cell*: a word on its own
//! cache line, whose address is written into every instance's vmctx
//! `poll_ptr`. Compiled code reads the word at every function entry and loop
//! header with a `volatile` load, which Cranelift never merges, hoists or
//! deletes, so every poll reads memory afresh.
//!
//! [`InterruptHandle::interrupt`] is one atomic store into the word: no
//! syscall, no `mprotect`, no TLB shootdown. What the word means depends on
//! how the engine compiles polls (`Tunables::poll_variant`, A/B only):
//!
//! - *flag* (2): the word is 0 while armed and 1 when interrupted. A poll is
//!   `load; trapnz`, and the trap carries `Trap::Interrupt`. Nothing faults.
//! - *swap* (1): the word is a pointer. Armed, it points at the cell itself
//!   (readable). Interrupted, it points at one process-wide page that is
//!   mapped `PROT_NONE` once and never changed. A poll loads the word and then
//!   loads through it; that second load faults on the shared page, and the
//!   fault at a PC whose trap code is `Interrupt` becomes `Trap::Interrupt`
//!   (see `trap::from_runtime_box`).
//! - *page* (0): `ix-epoch-poll` 56c25a9's codegen (one `atomic_load` from
//!   the cell), kept only to measure the old hot path. It cannot be
//!   interrupted.
//!
//! # Visibility
//!
//! The interrupting thread's store is `SeqCst` (on x86 an `xchg`, which
//! drains its store buffer; on aarch64 `stlr`). The guest's poll is a plain
//! load with no fence. Both architectures keep caches coherent in hardware:
//! once the store has left the writer's store buffer, the line holding the
//! word is invalidated in every other core's cache, and the next load that
//! executes on the guest's core misses and reads the new value. The guest
//! needs no fence because it only has to see the store *eventually*, and
//! "eventually" is bounded by the next poll the guest executes (a load is
//! never satisfied from a stale copy once the invalidation has arrived)
//! plus one coherence round trip.
//!
//! [`Config::poll_interruption`]: crate::Config::poll_interruption

use alloc::sync::Arc;
use core::ptr::NonNull;
use core::sync::atomic::{AtomicUsize, Ordering};
use rustix::mm::{MapFlags, ProtFlags};
use std::sync::Mutex;

/// The address of the process-wide never-readable page (swap variant), or 0
/// before it is mapped. Mapped once, never unmapped, never made readable.
static REVOKED: AtomicUsize = AtomicUsize::new(0);
static REVOKED_INIT: Mutex<()> = Mutex::new(());

fn page_size() -> usize {
    crate::runtime::vm::host_page_size()
}

/// The never-readable page, mapping it on first use.
fn revoked_page() -> crate::Result<usize> {
    let addr = REVOKED.load(Ordering::Acquire);
    if addr != 0 {
        return Ok(addr);
    }
    let _g = REVOKED_INIT.lock().unwrap_or_else(|e| e.into_inner());
    let addr = REVOKED.load(Ordering::Acquire);
    if addr != 0 {
        return Ok(addr);
    }
    // SAFETY: a fresh anonymous mapping at an address the kernel picks. It is
    // never unmapped and its protection never changes, so a pointer to it
    // stays valid (and faulting) for the rest of the process.
    let base = unsafe {
        rustix::mm::mmap_anonymous(
            core::ptr::null_mut(),
            page_size(),
            ProtFlags::empty(),
            MapFlags::PRIVATE,
        )
    }
    .map_err(|e| crate::format_err!("mapping the poll interrupt page failed: {e}"))?;
    REVOKED.store(base as usize, Ordering::Release);
    Ok(base as usize)
}

/// Whether `addr` lies in the process-wide never-readable page. A fault there
/// can only come from a poll of a store that was interrupted (swap variant).
pub(crate) fn is_revoked_page(addr: usize) -> bool {
    let start = REVOKED.load(Ordering::Acquire);
    start != 0 && addr >= start && addr - start < page_size()
}

/// One store's poll word, alone on its cache line (and the adjacent-line
/// prefetch pair), so polls never share a line with data another core writes.
#[repr(C, align(128))]
struct Cell {
    word: AtomicUsize,
    /// The word's value while armed.
    armed: usize,
    /// The word's value while interrupted.
    revoked: usize,
}

/// Interrupts the wasm running in one [`Store`](crate::Store), from any
/// thread. Created by [`Store::interrupt_handle`](crate::Store::interrupt_handle)
/// when [`Config::poll_interruption`](crate::Config::poll_interruption) is on.
///
/// The handle keeps its store's poll cell alive, so interrupting through a
/// handle that outlived its store is harmless: no other store reads that cell.
#[derive(Clone)]
pub struct InterruptHandle(Arc<Cell>);

impl InterruptHandle {
    pub(crate) fn new(variant: u8) -> crate::Result<Self> {
        let swap = variant != 2;
        let revoked = if swap { revoked_page()? } else { 1 };
        let mut cell = Arc::new(Cell {
            word: AtomicUsize::new(0),
            armed: 0,
            revoked,
        });
        if swap {
            // Armed, the word points at itself: readable, and owned by this
            // store alone.
            let me = Arc::get_mut(&mut cell).expect("just created");
            me.armed = core::ptr::from_ref(&me.word) as usize;
            *me.word.get_mut() = me.armed;
        }
        Ok(InterruptHandle(cell))
    }

    /// The address compiled code polls (the cell's word).
    pub(crate) fn poll_ptr(&self) -> NonNull<core::sync::atomic::AtomicU64> {
        NonNull::from(&self.0.word).cast()
    }

    /// Interrupt this store's wasm: the next poll traps with
    /// [`Trap::Interrupt`](crate::Trap::Interrupt). Wasm that is not running
    /// yet traps at its first function entry. Idempotent, wait-free, and makes
    /// no syscall.
    ///
    /// Running wasm sees the interrupt at its next poll (the next function
    /// entry or loop iteration) once the store has reached its core's cache;
    /// see the module docs.
    pub fn interrupt(&self) {
        self.0.word.store(self.0.revoked, Ordering::SeqCst);
    }

    /// Make the store's wasm runnable again. Returns whether it had been
    /// interrupted.
    ///
    /// Call it on the thread that runs the store, between calls: an
    /// interrupt that lands after a call returned stays pending until this
    /// runs.
    pub fn rearm(&self) -> bool {
        self.0.word.swap(self.0.armed, Ordering::SeqCst) != self.0.armed
    }

    /// Whether [`InterruptHandle::interrupt`] ran and
    /// [`InterruptHandle::rearm`] has not run since.
    pub fn is_interrupted(&self) -> bool {
        self.0.word.load(Ordering::SeqCst) != self.0.armed
    }
}

impl core::fmt::Debug for InterruptHandle {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("InterruptHandle")
            .field("cell", &core::ptr::from_ref(&self.0.word))
            .field("interrupted", &self.is_interrupted())
            .finish()
    }
}
