//! ix fork: the per-module `call_indirect` dispatch plan
//! (`Tunables::ix_static_call_indirect`). Planned in
//! `compile/ix_tables.rs`; filled in at load by the runtime.

use crate::{ModuleInternedTypeIndex, TableIndex};
use alloc::vec::Vec;

crate::ix_abi_struct! {
    /// ix fork: the `call_indirect` dispatch array of a module
    /// (`Tunables::ix_static_call_indirect`), planned at compile time from the
    /// module's own bytes and serialized with it.
    ///
    /// At load the runtime builds one read-only array of `entries` code
    /// pointers per module, shared by every instance: for row `r` over table
    /// image `images[r.image]` and type `r.ty`, entry `r.start + i` for
    /// `i < r.len` is the wasm-ABI entry of element `i`'s function when its
    /// type is `r.ty`, else 0, and entry `r.start + r.len` is 0. Compiled code
    /// clamps the index to `r.len` (Spectre-safe select), loads the entry and
    /// calls it with its own vmctx when non-zero. A zero entry (another type,
    /// null, out of bounds, or past the row's last match) runs the ordinary
    /// fully checked lowering, which then calls or traps exactly as stock
    /// Wasmtime does.
    #[derive(Clone, Debug, Default, serde_derive::Serialize, serde_derive::Deserialize)]
    pub struct IxDispatchPlan {
        /// The fixed contents of each static table that has a row, cut after
        /// the longest row over it: the defined function index of each
        /// element, `u32::MAX` for null.
        pub images: Vec<Vec<u32>>,
        /// At most one row per `(table, ty)`, sorted by `(table, ty)`.
        pub rows: Vec<IxDispatchRow>,
        /// Total number of entries, the sum over rows of `len + 1`.
        pub entries: u32,
    }
}

crate::ix_abi_struct! {
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
        /// Number of table elements the row covers: one past the last element
        /// of type `ty`. At most the image's length; at least 1.
        pub len: u32,
    }
}

impl IxDispatchPlan {
    /// The row for calls of type `ty` through `table`.
    pub fn row(&self, table: TableIndex, ty: ModuleInternedTypeIndex) -> Option<IxDispatchRow> {
        let i = self
            .rows
            .binary_search_by(|r| (r.table, r.ty).cmp(&(table, ty)))
            .ok()?;
        Some(self.rows[i])
    }
}
