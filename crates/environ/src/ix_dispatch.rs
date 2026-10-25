//! ix fork: the per-module `call_indirect` dispatch plan
//! (`Tunables::ix_static_call_indirect`). Planned in
//! `compile/ix_tables.rs`; filled in at load by the runtime.

use crate::{ModuleInternedTypeIndex, TableIndex};
use alloc::vec::Vec;

/// ix fork: the `call_indirect` dispatch array of a module
/// (`Tunables::ix_static_call_indirect`), planned at compile time from the
/// module's own bytes and serialized with it.
///
/// At load the runtime builds one read-only array of `entries` code
/// pointers per module, shared by every instance: for row `r` of table
/// image `images[r.image]` (length `N`) and type `r.ty`, entry `r.start + i`
/// for `i < N` is the wasm-ABI entry of the element's function when its
/// type is `r.ty`, else 0, and entry `r.start + N` is 0. Compiled code
/// clamps the index to `N` (Spectre-safe select), loads the entry, calls it
/// with its own vmctx when non-zero, and otherwise runs the ordinary fully
/// checked lowering, which then always traps with the right code.
#[derive(Clone, Debug, Default, serde_derive::Serialize, serde_derive::Deserialize)]
pub struct IxDispatchPlan {
    /// The fixed contents of each static table that has a row: the defined
    /// function index of each element, `u32::MAX` for null.
    pub images: Vec<Vec<u32>>,
    /// At most one row per `(table, ty)`.
    pub rows: Vec<IxDispatchRow>,
    /// Total number of entries, the sum over rows of image length + 1.
    pub entries: u32,
}

/// One `(table, type)` row of an [`IxDispatchPlan`].
#[derive(Clone, Copy, Debug, serde_derive::Serialize, serde_derive::Deserialize)]
pub struct IxDispatchRow {
    /// The static table called through.
    pub table: TableIndex,
    /// The (final) type of the `call_indirect`s using this row.
    pub ty: ModuleInternedTypeIndex,
    /// Index into `IxDispatchPlan::images`.
    pub image: u32,
    /// First entry of this row in the array.
    pub start: u32,
}

impl IxDispatchPlan {
    /// The row for calls of type `ty` through `table`, with the table's
    /// (fixed) size.
    pub fn row(&self, table: TableIndex, ty: ModuleInternedTypeIndex) -> Option<(IxDispatchRow, u32)> {
        let r = *self.rows.iter().find(|r| r.table == table && r.ty == ty)?;
        let n = u32::try_from(self.images[r.image as usize].len()).ok()?;
        Some((r, n))
    }
}

