//! Embedder-provided stacks for stack-switching continuations.
//!
//! By default each continuation gets its own `mmap`ed stack. An embedder that
//! must own every byte of guest-reachable state (to snapshot it, copy it to
//! another process, or diff it) installs a [`ContinuationStackCreator`] with
//! [`Config::with_continuation_stacks`](crate::Config::with_continuation_stacks)
//! and hands out stacks from a region it controls.

use crate::Result;
use core::ops::Range;

/// A stack handed out by a [`ContinuationStackCreator`].
#[derive(Clone, Copy, Debug)]
pub struct ContinuationStackSpan {
    /// The lowest usable address of the stack (stacks grow down from
    /// `base + len`).
    pub base: *mut u8,
    /// Bytes of guard below `base` that the creator left inaccessible.
    pub guard_size: usize,
    /// Usable bytes above `base`.
    pub len: usize,
}

// SAFETY: a span is a description of memory the creator owns; it carries no
// thread affinity.
unsafe impl Send for ContinuationStackSpan {}
// SAFETY: as above.
unsafe impl Sync for ContinuationStackSpan {}

impl ContinuationStackSpan {
    /// The address range of the usable stack.
    pub fn range(&self) -> Range<usize> {
        self.base as usize..self.base as usize + self.len
    }
}

/// Allocates continuation stacks.
///
/// # Safety
///
/// Every span returned must be readable and writable for `len` bytes from
/// `base`, must not overlap any other live span or any memory Wasmtime owns,
/// and must stay valid until the [`Store`](crate::Store) that requested it is
/// dropped (continuations are never freed before their store).
pub unsafe trait ContinuationStackCreator: Send + Sync {
    /// Return a stack of at least `size` usable bytes.
    fn new_stack(&self, size: usize) -> Result<ContinuationStackSpan>;
}
