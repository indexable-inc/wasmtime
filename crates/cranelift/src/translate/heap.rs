//! Heaps to implement WebAssembly linear memories.

use cranelift_codegen::cursor::FuncCursor;
use cranelift_codegen::ir::{self, InstBuilder, Type};
use cranelift_entity::entity_impl;
use smallvec::SmallVec;
use wasmtime_environ::{IndexType, Memory};

pub use wasmtime_environ::MemoryKind;

/// The proof-carrying-code fact a [`Load`] in a [`VmctxLoadChain`] attaches
/// to its result.
///
/// The fact is axiomatic: it restates the field fact recorded in the memory
/// type of the struct being loaded from (see `FuncEnvironment::make_heap`),
/// and the PCC checker verifies the load against that field.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum LoadPcc {
    /// The loaded value points to offset 0 of a region of the given memory
    /// type (a `VM*` struct, or a static linear memory).
    PointsTo(ir::MemoryType),
    /// The loaded value is the base of a dynamically-sized memory of the
    /// given memory type.
    DynamicBase(ir::MemoryType),
    /// The loaded value equals the given (symbolic) global value, e.g. the
    /// current length of a dynamic memory.
    GlobalValue(ir::GlobalValue),
}

impl LoadPcc {
    /// The fact for a loaded value of `bits` width.
    pub fn fact(&self, bits: u16) -> ir::pcc::Fact {
        use ir::pcc::Fact;
        match *self {
            LoadPcc::PointsTo(ty) => Fact::Mem {
                ty,
                min_offset: 0,
                max_offset: 0,
                nullable: false,
            },
            LoadPcc::DynamicBase(ty) => Fact::dynamic_base_ptr(ty),
            LoadPcc::GlobalValue(gv) => Fact::global_value(bits, gv),
        }
    }
}

/// A single load, relative to some base value.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Load {
    /// The offset added to the base before loading.
    pub offset: u32,
    /// The memory flags for the load.
    pub flags: ir::MemFlagsData,
    /// The type loaded. Only the last load in a chain may be non-pointer-typed.
    pub ty: ir::Type,
    /// When compiling with proof-carrying code, the fact for the loaded
    /// value. Such a load is marked `checked`.
    pub pcc: Option<LoadPcc>,
}

impl Load {
    /// Attach a proof-carrying-code fact to this load's result (and mark the
    /// load `checked`).
    pub fn with_pcc(mut self, pcc: Option<LoadPcc>) -> Self {
        self.pcc = pcc;
        self
    }

    /// Emit this load, relative to the given `base`.
    pub fn emit(&self, cursor: &mut FuncCursor<'_>, base: ir::Value) -> ir::Value {
        let flags = if self.pcc.is_some() {
            self.flags.with_checked()
        } else {
            self.flags
        };
        let val = cursor
            .ins()
            .load(self.ty, flags, base, i32::try_from(self.offset).unwrap());
        if let Some(pcc) = self.pcc {
            let bits = u16::try_from(self.ty.bits()).unwrap();
            cursor.func.dfg.facts[val] = Some(pcc.fact(bits));
        }
        val
    }

    /// Emit this load, relative to the given `base`, as an `ir::GlobalValue`.
    ///
    /// Prefer plain `emit`; this is only for cases where a global value is
    /// required (like the stack limit checks that Cranelift emits inline in the
    /// compiled function's prologue).
    pub fn emit_global(&self, func: &mut ir::Function, base: ir::GlobalValue) -> ir::GlobalValue {
        let flags = func.dfg.mem_flags.insert(self.flags).unwrap();
        func.global_values.push(ir::GlobalValueData::Load {
            base,
            offset: i32::try_from(self.offset).unwrap().into(),
            global_type: self.ty,
            flags,
        })
    }
}

/// A chain of loads, rooted at the `vmctx`.
///
/// Used to compute a heap's base address or bound.
#[derive(Clone, PartialEq, Hash)]
pub struct VmctxLoadChain(SmallVec<[Load; 2]>);

impl VmctxLoadChain {
    /// Create a new load sequence.
    ///
    /// The sequence must be non-empty.
    pub fn new(loads: SmallVec<[Load; 2]>) -> Self {
        assert!(!loads.is_empty());
        VmctxLoadChain(loads)
    }

    /// Emit the load chain, starting from `vmctx`, returning the final loaded
    /// value.
    pub fn emit(&self, cursor: &mut FuncCursor<'_>, vmctx: ir::Value) -> ir::Value {
        let mut val = vmctx;
        for load in &self.0 {
            val = load.emit(cursor, val);
        }
        val
    }

    /// Emit the load chain, starting from `vmctx`, as a sequence of CLIF global
    /// values.
    ///
    /// Prefer plain `emit`; this is only for cases where a global value is
    /// required (like the stack limit checks that Cranelift emits inline in the
    /// compiled function's prologue).
    pub fn emit_global(&self, func: &mut ir::Function) -> ir::GlobalValue {
        let mut val = func.global_values.push(ir::GlobalValueData::VMContext);
        for load in &self.0 {
            val = load.emit_global(func, val);
        }
        val
    }

    /// The loads of this chain, for attaching proof-carrying-code facts.
    pub fn loads_mut(&mut self) -> &mut [Load] {
        &mut self.0
    }

    /// Like `emit_global`, but reusing a structurally identical chain of
    /// global values already in `func` when there is one.
    ///
    /// Proof-carrying code uses the result only as a symbol in facts, so
    /// reuse keeps symbols canonical: two functions (or an inlined callee and
    /// its caller) built the same way name the same bound.
    pub fn emit_global_canonical(&self, func: &mut ir::Function) -> ir::GlobalValue {
        let mut val = func
            .global_values
            .iter()
            .find(|(_, d)| matches!(d, ir::GlobalValueData::VMContext))
            .map(|(gv, _)| gv)
            .unwrap_or_else(|| func.global_values.push(ir::GlobalValueData::VMContext));
        for load in &self.0 {
            let flags = func.dfg.mem_flags.insert(load.flags).unwrap();
            let data = ir::GlobalValueData::Load {
                base: val,
                offset: i32::try_from(load.offset).unwrap().into(),
                global_type: load.ty,
                flags,
            };
            val = match func.global_values.iter().find(|(_, d)| **d == data) {
                Some((gv, _)) => gv,
                None => func.global_values.push(data),
            };
        }
        val
    }
}


/// An opaque reference to a [`HeapData`][crate::HeapData].
///
/// While the order is stable, it is arbitrary.
#[derive(Copy, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Heap(u32);
entity_impl!(Heap, "heap");

/// A heap implementing a WebAssembly linear memory.
///
/// Code compiled from WebAssembly runs in a sandbox where it can't access all
/// process memory. Instead, it is given a small set of memory areas to work in,
/// and all accesses are bounds checked. Wasmtime models this through
/// the concept of *heaps*.
///
/// Heap addresses can be smaller than the native pointer size, for example
/// unsigned `i32` offsets on a 64-bit architecture.
///
/// A heap appears as three consecutive ranges of address space:
///
/// 1. The *mapped pages* are the accessible memory range in the heap. A heap
///    may have a minimum guaranteed size which means that some mapped pages are
///    always present.
///
/// 2. The *unmapped pages* is a possibly empty range of address space that may
///    be mapped in the future when the heap is grown. They are addressable but
///    not accessible.
///
/// 3. The *offset-guard pages* is a range of address space that is guaranteed
///    to always cause a trap when accessed. It is used to optimize bounds
///    checking for heap accesses with a shared base pointer. They are
///    addressable but not accessible.
#[derive(Clone, PartialEq, Hash)]
pub struct HeapData {
    /// The address of the start of the heap's storage.
    pub base: VmctxLoadChain,

    /// The dynamic byte length of this heap, if needed.
    pub bound: VmctxLoadChain,

    /// The type of wasm memory that this heap is operating on.
    pub memory: Memory,

    /// Whether this is a linear memory or a GC heap.
    pub kind: MemoryKind,

    /// The memory type for the pointed-to memory, if using proof-carrying
    /// code. When set, every access to this heap is marked `checked`.
    pub pcc_memory_type: Option<ir::MemoryType>,

    /// A symbolic global value naming this heap's dynamic bound, for
    /// proof-carrying-code facts on dynamically bounds-checked heaps. It is
    /// never materialized by `global_value`; it only appears in facts.
    pub pcc_bound_gv: Option<ir::GlobalValue>,
}

impl HeapData {
    pub fn index_type(&self) -> Type {
        match self.memory.idx_type {
            IndexType::I32 => ir::types::I32,
            IndexType::I64 => ir::types::I64,
        }
    }

    /// Get the [`MemoryTunables`] for this heap based on its [`MemoryKind`].
    pub fn memory_tunables<'a>(
        &self,
        tunables: &'a wasmtime_environ::Tunables,
    ) -> wasmtime_environ::MemoryTunables<'a> {
        wasmtime_environ::MemoryTunables::new(tunables, self.kind)
    }
}
