//! Poll interruption (ix fork); see [`Config::poll_interruption`].
//!
//! Each [`Store`](crate::Store) owns one *poll cell*: a word on its own
//! cache line, whose address is written into every instance's vmctx
//! `poll_ptr`. Compiled code reads the word at every function entry and loop
//! header with a `volatile` load, which Cranelift never merges, hoists or
//! deletes, so every poll reads memory afresh, and traps with
//! `Trap::Interrupt` if it is nonzero. The trap is a trap instruction
//! (`ud2`/`udf`), never a memory fault: the word is always readable.
//!
//! [`InterruptHandle::interrupt`] is one atomic store of 1 into the word: no
//! syscall, no `mprotect`, no TLB shootdown. [`InterruptHandle::rearm`]
//! stores 0.
//!
//! The cell lives in the handle (an `Arc`), not in vmctx: vmctx belongs to an
//! instance slot that the pooling allocator hands to another tenant once the
//! store is gone, and a handle may outlive its store. Interrupting through a
//! stale handle writes only to its own cell, which nothing reads any more.
//!
//! # Visibility
//!
//! The interrupting thread's store is `SeqCst` (on x86 an `xchg`, which
//! drains its store buffer; on aarch64 `stlr`). The guest's poll is a plain
//! load with no fence. Both architectures keep caches coherent in hardware:
//! once the store has left the writer's store buffer, the line holding the
//! word is invalidated in every other core's cache, and the next load of it
//! on the guest's core misses and reads the new value. A load is never
//! satisfied from a stale copy after the invalidation has arrived, and a
//! volatile load is not hoisted out of the loop, so the guest sees the
//! interrupt at its next poll (the next function entry or loop iteration)
//! after one coherence round trip. It needs no fence because it only has to
//! see the store eventually, not in order with anything else.
//!
//! [`Config::poll_interruption`]: crate::Config::poll_interruption

use alloc::sync::Arc;
use core::ptr::NonNull;
use core::sync::atomic::{AtomicU64, Ordering};

/// One store's poll word, alone on its cache line (and the adjacent-line
/// prefetch pair), so polls never share a line with data another core writes.
#[repr(C, align(128))]
struct Cell {
    word: AtomicU64,
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
    pub(crate) fn new() -> Self {
        InterruptHandle(Arc::new(Cell {
            word: AtomicU64::new(0),
        }))
    }

    /// The address compiled code polls (the cell's word).
    pub(crate) fn poll_ptr(&self) -> NonNull<AtomicU64> {
        NonNull::from(&self.0.word)
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
        self.0.word.store(1, Ordering::SeqCst);
    }

    /// Make the store's wasm runnable again. Returns whether it had been
    /// interrupted.
    ///
    /// Hard requirement: call it only on the thread that runs the store,
    /// between calls into it, never while its wasm runs. An interrupt that
    /// lands after a call returned stays pending until this runs. A rearm
    /// from another thread can clear the word after a poll saw it set but
    /// before the trap is attributed; that call then fails with a distinct
    /// error, not [`Trap::Interrupt`](crate::Trap::Interrupt), so a stray
    /// trap never passes for an interrupt the host issued.
    pub fn rearm(&self) -> bool {
        self.0.word.swap(0, Ordering::SeqCst) != 0
    }

    /// Whether [`InterruptHandle::interrupt`] ran and
    /// [`InterruptHandle::rearm`] has not run since.
    pub fn is_interrupted(&self) -> bool {
        self.0.word.load(Ordering::SeqCst) != 0
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
