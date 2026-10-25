//! ix fork: tables whose contents the module itself fixes forever.
//!
//! A table is *static* when every one of these holds, all read from the
//! module's own bytes (never from anything the guest claims):
//!
//! * it is defined by the module, not imported, and not exported, so no
//!   other module and no host code can reach it (Wasmtime's API hands out a
//!   table only through an export);
//! * it is a `funcref` table whose initial value is null;
//! * its contents come only from active element segments with constant
//!   offsets that fit in its minimum size, whose items are constant
//!   `ref.func` or `ref.null` (so they are known before any code runs);
//! * no function body contains `table.set`, `table.grow`, `table.fill`,
//!   `table.init`, or `table.copy` into it, reachable or not.
//!
//! Then its runtime size is its minimum forever, and element `i` is always
//! the function the segments put there (or null). Instantiation applies the
//! segments in order before the start function runs; if one traps, the
//! instance never exists. `ModuleTranslation::ix_static_tables` holds the
//! result for the compiler.

use crate::module::TableSegmentElements;
use crate::{
    ConstOp, DefinedTableIndex, EntityIndex, FuncIndex, ModuleInternedTypeIndex, ModuleTranslation,
    TableIndex, TableInitialValue, WasmHeapType,
};
use alloc::{sync::Arc, vec, vec::Vec};
use cranelift_entity::packed_option::ReservedValue;
use wasmparser::{FunctionBody, Operator};

/// Tables the module may write at runtime, from scanning function bodies.
#[derive(Default)]
pub struct IxTableWrites {
    written: Vec<u32>,
    /// A body failed to parse: treat every table as written. (Such a module
    /// fails validation anyway; this only keeps the fact sound regardless.)
    all: bool,
}

impl IxTableWrites {
    /// Record every table this body can write.
    pub fn scan(&mut self, body: &FunctionBody<'_>) {
        if self.scan_inner(body).is_err() {
            self.all = true;
        }
    }

    fn scan_inner(&mut self, body: &FunctionBody<'_>) -> wasmparser::Result<()> {
        let mut ops = body.get_operators_reader()?;
        while !ops.eof() {
            match ops.read()? {
                Operator::TableSet { table }
                | Operator::TableGrow { table }
                | Operator::TableFill { table }
                | Operator::TableInit { table, .. }
                | Operator::TableCopy {
                    dst_table: table, ..
                } => self.written.push(table),
                _ => {}
            }
        }
        Ok(())
    }

    fn is_written(&self, table: TableIndex) -> bool {
        self.all || self.written.contains(&table.as_u32())
    }
}

/// The fixed contents of a static table.
#[derive(Clone, Debug)]
pub struct IxStaticTable {
    /// One entry per element of its (fixed) size,
    /// `FuncIndex::reserved_value()` for null.
    pub funcs: Arc<[FuncIndex]>,
    /// The type every non-null element has, if they all have the same one
    /// (and there is at least one).
    pub uniform: Option<ModuleInternedTypeIndex>,
}

impl ModuleTranslation<'_> {
    /// Compute `ix_static_tables` from the raw table initializers. Must run
    /// before `finalize_table_init` rewrites them.
    pub(crate) fn ix_compute_static_tables(&mut self) {
        let module = &self.module;
        let mut out: Vec<Option<IxStaticTable>> = Vec::new();
        for (def, init) in self.table_initialization.initial_values.iter() {
            let index = module.table_index(def);
            out.push(self.ix_static_table(def, index, init));
        }
        self.ix_static_tables = out;
    }

    fn ix_static_table(
        &self,
        def: DefinedTableIndex,
        index: TableIndex,
        init: &TableInitialValue,
    ) -> Option<IxStaticTable> {
        // Cap the image like `try_func_table_init` does.
        const MAX: u64 = 1 << 20;
        let module = &self.module;
        let table = &module.tables[index];
        if !matches!(table.ref_type.heap_type, WasmHeapType::Func) {
            return None;
        }
        if !matches!(init, TableInitialValue::Null) {
            return None;
        }
        if self.ix_table_writes.is_written(index) {
            return None;
        }
        if module
            .exports
            .values()
            .any(|e| matches!(e, EntityIndex::Table(t) if *t == index))
        {
            return None;
        }
        let size = table.limits.min;
        if size > MAX {
            return None;
        }
        let _ = def;
        let mut image = vec![FuncIndex::reserved_value(); usize::try_from(size).ok()?];
        for seg in self.table_initialization.segments.iter() {
            if seg.table_index != index {
                continue;
            }
            let offset = match seg.offset.ops() {
                &[ConstOp::I32Const(o)] => u64::from(o.cast_unsigned()),
                &[ConstOp::I64Const(o)] => o.cast_unsigned(),
                _ => return None,
            };
            let end = offset.checked_add(seg.elements.len())?;
            if end > size {
                return None;
            }
            let dst = &mut image[usize::try_from(offset).ok()?..usize::try_from(end).ok()?];
            match &seg.elements {
                TableSegmentElements::Functions(fs) => dst.copy_from_slice(fs),
                TableSegmentElements::Expressions { exprs, .. } => {
                    for (d, e) in dst.iter_mut().zip(exprs.iter()) {
                        *d = match e.ops() {
                            &[ConstOp::RefFunc(f)] => f,
                            &[ConstOp::RefNull(_)] => FuncIndex::reserved_value(),
                            _ => return None,
                        };
                    }
                }
            }
        }
        let mut uniform = None;
        for f in image.iter().filter(|f| !f.is_reserved_value()) {
            let ty = module.functions[*f].signature.unwrap_module_type_index();
            match uniform {
                None => uniform = Some(ty),
                Some(u) if u == ty => {}
                Some(_) => {
                    uniform = None;
                    break;
                }
            }
        }
        // `break` above leaves `uniform` None only on a mismatch; an empty
        // table also ends None.
        Some(IxStaticTable {
            funcs: image.into(),
            uniform,
        })
    }
}
