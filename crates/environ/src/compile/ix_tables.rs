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
//! * no element segment that could trap at instantiation (one aimed at an
//!   imported table, with a non-constant offset or item, or out of bounds)
//!   comes before any of its segments. Instantiation applies segments in
//!   order and stops at the first trap, and a failed instantiation can still
//!   leave this instance's functions reachable through an imported table
//!   that an earlier segment wrote. Those functions must then see exactly
//!   the partly initialized table stock Wasmtime would give them, so the
//!   table is only static if all of its segments are applied before
//!   anything can trap;
//! * no function body contains any table instruction naming it other than
//!   `table.get`, `table.size`, `call_indirect` and `return_call_indirect`,
//!   reachable or not (deny by default: an instruction this scan does not
//!   know as a read counts as a write).
//!
//! Then its runtime size is its minimum forever, and element `i` is always
//! the function the segments put there (or null). `ModuleTranslation::
//! ix_static_tables` holds the result for the compiler.

use crate::module::TableSegmentElements;
use crate::{IxDispatchPlan, IxDispatchRow};
use crate::{
    ConstOp, EntityIndex, FuncIndex, ModuleInternedTypeIndex, ModuleTranslation, TableIndex,
    TableInitialValue, WasmHeapType,
};
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::{sync::Arc, vec::Vec};
use cranelift_entity::EntityRef;
use cranelift_entity::packed_option::ReservedValue;
use wasmparser::{FunctionBody, Operator};

/// Tables the module may write at runtime, from scanning function bodies.
#[derive(Default)]
pub struct IxTableWrites {
    written: BTreeSet<u32>,
    /// `(table, type index)` of every `call_indirect` and
    /// `return_call_indirect`, with its number of sites.
    call_indirect: BTreeMap<(u32, u32), u64>,
    /// Operands of the `call_indirect` being decoded.
    ci_table: u32,
    ci_type: u32,
    /// Whether the operator being decoded had an operand recognized as a
    /// table index (read or write).
    saw_table: bool,
    /// A body failed to parse, or used an instruction with a table-like
    /// operand this scan does not know: treat every table as written.
    all: bool,
}

/// One operand of an operator: a table index operand is a write unless the
/// operator is one of the known reads.
macro_rules! ix_table_arg {
    ($s:ident, CallIndirect, type_index, $v:ident) => { $s.ci_type = *$v; };
    ($s:ident, ReturnCallIndirect, type_index, $v:ident) => { $s.ci_type = *$v; };
    ($s:ident, CallIndirect, table_index, $v:ident) => { $s.ci_table = *$v; $s.saw_table = true; };
    ($s:ident, ReturnCallIndirect, table_index, $v:ident) => { $s.ci_table = *$v; $s.saw_table = true; };
    ($s:ident, TableGet, table, $v:ident) => { $s.saw_table = true; };
    ($s:ident, TableSize, table, $v:ident) => { $s.saw_table = true; };
    ($s:ident, $op:ident, table, $v:ident) => { $s.write(*$v); };
    ($s:ident, $op:ident, dst_table, $v:ident) => { $s.write(*$v); };
    ($s:ident, $op:ident, src_table, $v:ident) => { $s.write(*$v); };
    ($s:ident, $op:ident, table_index, $v:ident) => { $s.write(*$v); };
    ($s:ident, $op:ident, $name:ident, $v:ident) => { $s.other_operand(stringify!($name)); };
}

/// After all operands of an operator.
macro_rules! ix_table_op_end {
    ($s:ident, CallIndirect) => { $s.end_call_indirect(); };
    ($s:ident, ReturnCallIndirect) => { $s.end_call_indirect(); };
    ($s:ident, $op:ident) => { $s.end_op(stringify!($op)); };
}

macro_rules! ix_define_scan_op {
    ($( @$proposal:ident $op:ident $({ $($arg:ident: $argty:ty),* })? => $visit:ident ($($ann:tt)*))*) => {
        impl IxTableWrites {
            /// Note what `op` does to tables. Generated from wasmparser's
            /// full operator list, so a new operator with a table operand is
            /// a write until it is listed above as a read.
            #[allow(unused_variables, reason = "most operands are not tables")]
            fn scan_op(&mut self, op: &Operator<'_>) {
                match op {
                    $(
                        Operator::$op $({ $($arg),* })? => {
                            self.saw_table = false;
                            $($( ix_table_arg!(self, $op, $arg, $arg); )*)?
                            ix_table_op_end!(self, $op);
                        }
                    )*
                    #[allow(unreachable_patterns, reason = "Operator is non_exhaustive")]
                    _ => self.all = true,
                }
            }
        }
    };
}
wasmparser::for_each_operator!(ix_define_scan_op);

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
            let op = ops.read()?;
            self.scan_op(&op);
        }
        Ok(())
    }

    fn write(&mut self, table: u32) {
        self.saw_table = true;
        self.written.insert(table);
    }

    fn end_call_indirect(&mut self) {
        *self.call_indirect.entry((self.ci_table, self.ci_type)).or_insert(0) += 1;
    }

    /// After any other operator: a `Table*` instruction whose table operand
    /// the scan did not recognize (a future operator, or a renamed field)
    /// poisons the scan, so it is a write, never a read. Every table
    /// instruction is named `Table*`; `BrTable` and `TryTable` are control
    /// flow with no table operand.
    fn end_op(&mut self, op: &str) {
        if !self.saw_table && op.starts_with("Table") {
            self.all = true;
        }
    }

    /// An operand that is not a known table index. Anything named like a
    /// table that is not one of the known non-index operands (`try_table`'s
    /// and `resume`'s handler lists) poisons the scan.
    fn other_operand(&mut self, name: &str) {
        if name.contains("table") && name != "try_table" && name != "resume_table" {
            self.all = true;
        }
    }

    fn is_written(&self, table: TableIndex) -> bool {
        self.all || self.written.contains(&table.as_u32())
    }
}

/// The fixed contents of a static table.
#[derive(Clone, Debug)]
pub struct IxStaticTable {
    /// One entry per element up to the end of the last segment applied to
    /// it, `FuncIndex::reserved_value()` for null. Every element past the
    /// end (up to the table's fixed size) is null.
    pub funcs: Arc<[FuncIndex]>,
    /// The type every non-null element has, if they all have the same one
    /// (and there is at least one).
    pub uniform: Option<ModuleInternedTypeIndex>,
}

impl ModuleTranslation<'_> {
    /// Compute `ix_static_tables` from the raw table initializers. Must run
    /// before `finalize_table_init` rewrites them. Linear in the size of the
    /// module's element segments (plus a log factor).
    pub(crate) fn ix_compute_static_tables(&mut self) {
        // Cap each image like `try_func_table_init` does, and all of them
        // together the same, so a module with many huge tables costs at
        // most this many entries at compile time.
        const MAX_TABLE: u64 = 1 << 20;
        const MAX_TOTAL: usize = 1 << 20;
        let module = &self.module;
        let exported: BTreeSet<TableIndex> = module
            .exports
            .values()
            .filter_map(|e| match e {
                EntityIndex::Table(t) => Some(*t),
                _ => None,
            })
            .collect();
        let mut images: Vec<Option<Vec<FuncIndex>>> = self
            .table_initialization
            .initial_values
            .iter()
            .map(|(def, init)| {
                let index = module.table_index(def);
                let table = &module.tables[index];
                let ok = matches!(table.ref_type.heap_type, WasmHeapType::Func)
                    && matches!(init, TableInitialValue::Null)
                    && !self.ix_table_writes.is_written(index)
                    && !exported.contains(&index)
                    && table.limits.min <= MAX_TABLE;
                ok.then(Vec::new)
            })
            .collect();

        let mut total = 0usize;
        // Whether a segment before this point could have trapped.
        let mut may_have_trapped = false;
        for seg in self.table_initialization.segments.iter() {
            let def = module.defined_table_index(seg.table_index);
            let offset = match seg.offset.ops() {
                &[ConstOp::I32Const(o)] => Some(u64::from(o.cast_unsigned())),
                &[ConstOp::I64Const(o)] => Some(o.cast_unsigned()),
                _ => None,
            };
            let end = offset.and_then(|o| o.checked_add(seg.elements.len()));
            let in_bounds = def.is_some()
                && end.is_some_and(|e| e <= module.tables[seg.table_index].limits.min);
            let items_const = match &seg.elements {
                TableSegmentElements::Functions(_) => true,
                TableSegmentElements::Expressions { exprs, .. } => exprs
                    .iter()
                    .all(|e| matches!(e.ops(), &[ConstOp::RefFunc(_)] | &[ConstOp::RefNull(_)])),
            };
            let could_trap = !in_bounds || !items_const;
            if let Some(def) = def {
                let slot = &mut images[def.index()];
                if may_have_trapped || could_trap {
                    *slot = None;
                }
                if let (Some(img), Some(offset), Some(end)) = (slot.as_mut(), offset, end) {
                    // Both fit in `usize`: `end <= min <= MAX_TABLE`.
                    let (offset, end) = (offset as usize, end as usize);
                    if img.len() < end {
                        let grow = end - img.len();
                        if total + grow > MAX_TOTAL {
                            *slot = None;
                        } else {
                            total += grow;
                            img.resize(end, FuncIndex::reserved_value());
                        }
                    }
                    if let Some(img) = slot.as_mut() {
                        let dst = &mut img[offset..end];
                        match &seg.elements {
                            TableSegmentElements::Functions(fs) => dst.copy_from_slice(fs),
                            TableSegmentElements::Expressions { exprs, .. } => {
                                for (d, e) in dst.iter_mut().zip(exprs.iter()) {
                                    *d = match e.ops() {
                                        &[ConstOp::RefFunc(f)] => f,
                                        _ => FuncIndex::reserved_value(),
                                    };
                                }
                            }
                        }
                    }
                }
            }
            if could_trap {
                may_have_trapped = true;
            }
        }

        self.ix_static_tables = images
            .into_iter()
            .map(|img| {
                let img = img?;
                let mut uniform = None;
                let mut mixed = false;
                for f in img.iter().filter(|f| !f.is_reserved_value()) {
                    let ty = module.functions[*f].signature.unwrap_module_type_index();
                    match uniform {
                        None => uniform = Some(ty),
                        Some(u) if u == ty => {}
                        Some(_) => {
                            mixed = true;
                            break;
                        }
                    }
                }
                Some(IxStaticTable {
                    funcs: img.into(),
                    uniform: if mixed { None } else { uniform },
                })
            })
            .collect();
    }

    /// Plan the `call_indirect` dispatch rows (`Tunables::ix_static_call_indirect`)
    /// into `self.module.ix_dispatch`. Needs `ix_static_tables`.
    ///
    /// A row is made for each `(table, type)` pair some body calls through
    /// when:
    ///
    /// * the table is static (above), 32-bit indexed, and every non-null
    ///   element is a function this module defines, so the caller's own
    ///   vmctx is the callee's (all elements are this instance's
    ///   functions);
    /// * the type is final, so a callee matches it exactly when its
    ///   engine-canonical type index equals the type's (no subtypes; the
    ///   runtime compares engine indices, not module-interned ones);
    /// * at least one element may have that type. The row stops after the
    ///   last element whose function type has the same shape (same value
    ///   types, any two reference types counted alike), so it covers every
    ///   element of the same engine type even when the module declares the
    ///   type twice (two interned indices, one engine type). Calls past the
    ///   row's end take the checked path; inside it the runtime fills by
    ///   engine type, so a zero entry is a real mismatch.
    ///
    /// The whole array holds at most `ENTRIES_PER_ELEMENT` entries per
    /// non-null element of the tables with rows (plus `ENTRIES_FLOOR`), and
    /// never more than `MAX_ENTRIES`, so its memory is a small multiple of
    /// the module's own element segments. Rows are kept in order of static
    /// call sites per entry (the most calls for the memory first); the rest
    /// keep the ordinary lowering. Static site counts say nothing about how
    /// hot a call is: at 8 per element Turso's guest (2450 elements, 33
    /// rows, 50322 truncated entries) kept the rows of about 98% of its
    /// sites but dropped the long row of its VDBE op handlers, its hottest
    /// `call_indirect`. At 32 per element (at most 256 bytes of array per
    /// element, once per module) that row fits, which took about 150
    /// instructions off each Turso lookup and 245 off each insert on Zen 5
    /// (wasm-host `bench/results/2026-10-26-ix-aot-beat-w2c`).
    ///
    /// The runtime fills each row once per module (see `IxDispatchPlan`);
    /// the compiler lowers calls through a row with no bounds, null or
    /// signature check on the hot path. Linear in the element segments, the
    /// call sites and the arity of the distinct types (plus log factors):
    /// each distinct element type and each call type gets one shape key, and
    /// a row's length is one map lookup, never a scan of the element types.
    pub(crate) fn ix_compute_dispatch(&mut self, types: &crate::ModuleTypesBuilder) {
        const MAX_ENTRIES: u64 = 1 << 20;
        const ENTRIES_PER_ELEMENT: u64 = 32;
        const ENTRIES_FLOOR: u64 = 1 << 10;
        let module = &self.module;

        // Distinct `(table, interned type)` pairs called through a usable
        // table, sorted by table, with their number of sites.
        let mut wanted: BTreeMap<(TableIndex, ModuleInternedTypeIndex), u64> = BTreeMap::new();
        for (&(table, ty), &sites) in self.ix_table_writes.call_indirect.iter() {
            let table = TableIndex::from_u32(table);
            let Some(def) = module.defined_table_index(table) else {
                continue;
            };
            if !matches!(self.ix_static_tables.get(def.index()), Some(Some(_))) {
                continue;
            }
            if module.tables[table].idx_type != crate::IndexType::I32 {
                continue;
            }
            let Some(ty) = module.types.get(crate::TypeIndex::from_u32(ty)) else {
                continue;
            };
            let crate::EngineOrModuleTypeIndex::Module(ty) = *ty else {
                continue;
            };
            if !types[ty].is_final {
                continue;
            }
            *wanted.entry((table, ty)).or_insert(0) += sites;
        }

        struct Cand {
            table: TableIndex,
            ty: ModuleInternedTypeIndex,
            sites: u64,
            len: u64,
        }
        let mut cands = Vec::new();
        let mut nonnull: u64 = 0;
        // Per table: the last index of each element function-type shape
        // (`ix_func_shape_key`) and of any element whose type has no shape
        // key, or None when some element is not a defined function.
        let mut stats: Option<(TableIndex, Option<(BTreeMap<Vec<u8>, u64>, Option<u64>)>)> =
            None;
        for (&(table, ty), &sites) in wanted.iter() {
            if stats.as_ref().map(|s| s.0) != Some(table) {
                let def = module.defined_table_index(table).unwrap();
                let st = self.ix_static_tables[def.index()].as_ref().unwrap();
                let mut by_ty: BTreeMap<ModuleInternedTypeIndex, u64> = BTreeMap::new();
                let mut ok = true;
                let mut n = 0;
                for (i, f) in st.funcs.iter().enumerate() {
                    if f.is_reserved_value() {
                        continue;
                    }
                    if module.defined_func_index(*f).is_none() {
                        ok = false;
                        break;
                    }
                    n += 1;
                    let fty = module.functions[*f].signature.unwrap_module_type_index();
                    by_ty.insert(fty, i as u64);
                }
                if ok {
                    nonnull += n;
                }
                // One shape key per distinct element type, so this is linear
                // in the element types (times their arity) plus log factors.
                let mut by_shape: BTreeMap<Vec<u8>, u64> = BTreeMap::new();
                let mut wild: Option<u64> = None;
                for (&fty, &last) in &by_ty {
                    match ix_func_shape_key(types, fty) {
                        Some(k) => {
                            let e = by_shape.entry(k).or_insert(0);
                            *e = (*e).max(last);
                        }
                        None => wild = wild.max(Some(last)),
                    }
                }
                stats = Some((table, ok.then_some((by_shape, wild))));
            }
            let Some((_, Some((by_shape, wild)))) = &stats else {
                continue;
            };
            // The row must reach every element whose engine type may equal
            // `ty`, not only those with the same interned index: without type
            // canonicalization (no GC types) two identical `(type (func ..))`
            // declarations intern to different indices but one engine type,
            // and the runtime fills (and the compiler treats a zero entry as
            // a mismatch) by engine type. Shapes that differ can never be
            // equal; the rest are included and the runtime fill decides.
            let last = match ix_func_shape_key(types, ty) {
                Some(k) => by_shape.get(&k).copied().max(*wild),
                // No key for the call's type: every element may match.
                None => by_shape.values().copied().max().max(*wild),
            };
            if let Some(last) = last {
                cands.push(Cand {
                    table,
                    ty,
                    sites,
                    len: last + 1,
                });
            }
        }

        // Most call sites per entry first, then by key.
        cands.sort_by(|a, b| {
            let ka = u128::from(a.sites) * u128::from(b.len + 1);
            let kb = u128::from(b.sites) * u128::from(a.len + 1);
            kb.cmp(&ka).then((a.table, a.ty).cmp(&(b.table, b.ty)))
        });
        let cap = MAX_ENTRIES.min(nonnull * ENTRIES_PER_ELEMENT + ENTRIES_FLOOR);
        let mut entries: u64 = 0;
        let mut kept = Vec::new();
        for c in cands {
            if entries + c.len + 1 > cap {
                continue;
            }
            entries += c.len + 1;
            kept.push(c);
        }
        kept.sort_by_key(|c| (c.table, c.ty));

        let mut plan = IxDispatchPlan::default();
        let mut start: u64 = 0;
        let mut i = 0;
        while i < kept.len() {
            let table = kept[i].table;
            let mut j = i;
            while j < kept.len() && kept[j].table == table {
                j += 1;
            }
            let longest = kept[i..j].iter().map(|c| c.len).max().unwrap();
            let def = module.defined_table_index(table).unwrap();
            let st = self.ix_static_tables[def.index()].as_ref().unwrap();
            let image: Vec<u32> = st.funcs[..longest as usize]
                .iter()
                .map(|f| {
                    if f.is_reserved_value() {
                        u32::MAX
                    } else {
                        module.defined_func_index(*f).unwrap().as_u32()
                    }
                })
                .collect();
            let image_index = u32::try_from(plan.images.len()).unwrap();
            plan.images.push(image);
            for c in &kept[i..j] {
                plan.rows.push(IxDispatchRow {
                    table,
                    ty: c.ty,
                    image: image_index,
                    start: u32::try_from(start).unwrap(),
                    len: u32::try_from(c.len).unwrap(),
                });
                start += c.len + 1;
            }
            i = j;
        }
        debug_assert_eq!(start, entries);
        plan.entries = u32::try_from(entries).unwrap();
        log::debug!(
            target: "ix_check_trim",
            "ix dispatch: {} rows, {} entries over {} tables ({} non-null elements, cap {})",
            plan.rows.len(),
            plan.entries,
            plan.images.len(),
            nonnull,
            cap
        );
        self.module.ix_dispatch = plan;
    }
}

/// The shape of a module function type: param count, then each param and
/// result value type, with every reference type collapsed to one marker
/// (their concrete indices are module-interned and may differ for one engine
/// type). Two function types with different keys never share an engine
/// type; equal keys may or may not, which the runtime fill settles by engine
/// type. `None` for a type that is not a plain (unshared) function type, which
/// is treated as possibly equal to anything.
fn ix_func_shape_key(types: &crate::ModuleTypesBuilder, t: ModuleInternedTypeIndex) -> Option<Vec<u8>> {
    let f = types[t].as_func()?;
    let code = |v: &crate::WasmValType| match v {
        crate::WasmValType::I32 => 0u8,
        crate::WasmValType::I64 => 1,
        crate::WasmValType::F32 => 2,
        crate::WasmValType::F64 => 3,
        crate::WasmValType::V128 => 4,
        crate::WasmValType::Ref(_) => 5,
    };
    let mut k = Vec::with_capacity(9 + f.params().len() + f.results().len());
    k.extend_from_slice(&(f.params().len() as u64).to_le_bytes());
    k.extend(f.params().iter().map(code));
    k.push(0xff);
    k.extend(f.results().iter().map(code));
    Some(k)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan(wat: &str) -> IxTableWrites {
        let wasm = wat::parse_str(wat).unwrap();
        let mut w = IxTableWrites::default();
        for payload in wasmparser::Parser::new(0).parse_all(&wasm) {
            if let wasmparser::Payload::CodeSectionEntry(body) = payload.unwrap() {
                w.scan(&body);
            }
        }
        w
    }

    #[test]
    fn reads_are_not_writes() {
        let w = scan(
            r#"(module
                (type $t (func))
                (table $a 1 funcref)
                (table $b 1 funcref)
                (func (drop (table.get $a (i32.const 0))) (drop (table.size $b))
                      (call_indirect $b (type $t) (i32.const 0)))
                (func (return_call_indirect $a (type $t) (i32.const 0)))
                (func (block (br_table 0 0 (i32.const 0)))))"#,
        );
        assert!(!w.all);
        assert!(w.written.is_empty());
        assert_eq!(w.call_indirect.len(), 2);
    }

    #[test]
    fn try_table_is_not_a_table_op() {
        let w = scan(
            r#"(module
                (tag $e)
                (func (try_table (catch_all 0)) ))"#,
        );
        assert!(!w.all);
    }

    #[test]
    fn every_table_operand_is_a_write() {
        let w = scan(
            r#"(module
                (table $a 1 funcref) (table $b 1 funcref) (table $c 1 funcref)
                (table $d 1 funcref) (table $e 1 funcref) (table $f 1 funcref)
                (elem $s func)
                (func
                  (table.set $a (i32.const 0) (ref.null func))
                  (drop (table.grow $b (ref.null func) (i32.const 1)))
                  (table.fill $c (i32.const 0) (ref.null func) (i32.const 0))
                  (table.init $d $s (i32.const 0) (i32.const 0) (i32.const 0))
                  (table.copy $e $f (i32.const 0) (i32.const 0) (i32.const 0))))"#,
        );
        assert!(!w.all);
        assert_eq!(w.written.iter().copied().collect::<Vec<_>>(), [0, 1, 2, 3, 4, 5]);
    }
}
