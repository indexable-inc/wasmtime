//! Poll-page interruption (ix fork); see [`Config::poll_interruption`].
//!
//! Each [`Store`](crate::Store) owns one page, mapped read-only, whose address
//! is written into every instance's vmctx. Compiled code loads from it at each
//! function entry and loop header. [`InterruptHandle::interrupt`] maps it
//! `PROT_NONE`, so the next poll faults; the trap metadata at that PC says
//! `Trap::Interrupt`, and the trap path checks that the faulting address is
//! this store's page (see `trap::from_runtime_box`).
//!
//! The page is reference counted: a handle held by another thread keeps the
//! mapping alive after its store is dropped, so `mprotect` never lands on an
//! address that has been unmapped and reused.
//!
//! [`Config::poll_interruption`]: crate::Config::poll_interruption

use alloc::sync::Arc;
use core::ptr::NonNull;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use rustix::mm::{MapFlags, MprotectFlags, ProtFlags};
use std::sync::Mutex;

struct PollPage {
    base: NonNull<AtomicU64>,
    len: usize,
    /// Set while the page is inaccessible. Read without the lock on the
    /// re-arm fast path; written only under `lock`.
    revoked: AtomicBool,
    /// Serializes `interrupt` and `rearm`, so the page's protection always
    /// matches `revoked` once the lock is released.
    lock: Mutex<()>,
}

// SAFETY: the page is never written through `base`; the pointer is only
// handed to compiled code and to `mprotect`/`munmap`.
unsafe impl Send for PollPage {}
// SAFETY: as above; all state changes go through `lock`.
unsafe impl Sync for PollPage {}

impl Drop for PollPage {
    fn drop(&mut self) {
        // SAFETY: we mapped exactly `base..base+len` and nothing else refers to
        // it once the last handle (and so the store) is gone.
        unsafe {
            let _ = rustix::mm::munmap(self.base.as_ptr().cast(), self.len);
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
        let len = crate::runtime::vm::host_page_size();
        // SAFETY: a fresh anonymous mapping at an address the kernel picks.
        let ptr = unsafe {
            rustix::mm::mmap_anonymous(core::ptr::null_mut(), len, ProtFlags::READ, MapFlags::PRIVATE)
        }
        .map_err(|e| crate::format_err!("mmap of the poll page failed: {e}"))?;
        let base = NonNull::new(ptr.cast::<AtomicU64>()).expect("mmap returned null");
        Ok(InterruptHandle(Arc::new(PollPage {
            base,
            len,
            revoked: AtomicBool::new(false),
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
        addr >= start && addr - start < self.0.len
    }

    fn protect(&self, flags: MprotectFlags) {
        // SAFETY: the page is our own mapping, alive as long as `self`.
        unsafe { rustix::mm::mprotect(self.0.base.as_ptr().cast(), self.0.len, flags) }
            .expect("mprotect of the poll page failed");
    }

    /// Make the poll page inaccessible: the next poll in this store's wasm
    /// traps with [`Trap::Interrupt`](crate::Trap::Interrupt). Wasm that is
    /// not running yet traps at its first function entry. Idempotent.
    ///
    /// When this returns, the kernel has changed the page tables and flushed
    /// every CPU's TLB for the page, so no later poll on any thread can read
    /// it.
    pub fn interrupt(&self) {
        let _g = self.0.lock.lock().unwrap_or_else(|e| e.into_inner());
        if !self.0.revoked.load(Ordering::Relaxed) {
            self.protect(MprotectFlags::empty());
            self.0.revoked.store(true, Ordering::Release);
        }
    }

    /// Make the poll page readable again, so wasm runs. Returns whether it had
    /// been interrupted. Costs one atomic load when it was not.
    pub fn rearm(&self) -> bool {
        if !self.0.revoked.load(Ordering::Acquire) {
            return false;
        }
        let _g = self.0.lock.lock().unwrap_or_else(|e| e.into_inner());
        if !self.0.revoked.load(Ordering::Relaxed) {
            return false;
        }
        self.protect(MprotectFlags::READ);
        self.0.revoked.store(false, Ordering::Release);
        true
    }

    /// Whether [`InterruptHandle::interrupt`] took effect and
    /// [`InterruptHandle::rearm`] has not run since.
    pub fn is_interrupted(&self) -> bool {
        self.0.revoked.load(Ordering::Acquire)
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
