//! ix fork: the shadow stack pointer in a pinned register.
//!
//! Rust (LLVM) wasm keeps its shadow stack pointer in a mutable `i32` global
//! (`__stack_pointer`). Every function with a shadow frame loads it from the
//! `VMContext` and stores it back, twice. With [`Tunables::ix_pinned_sp`] on,
//! Cranelift keeps one designated global in its pinned register (`r15` on
//! x86_64, `x21` on aarch64) as a cache:
//!
//! * The global's `VMContext` slot stays the truth. `global.set` writes the
//!   slot and the register (write-through), so the host, other instances and
//!   a trap always see the value wasm last wrote.
//! * `global.get` reads the register.
//! * The register is reloaded from the slot wherever it might hold something
//!   else: at the entry of every function that can be reached from outside
//!   the module's own direct calls (exported, in a table, `ref.func`, start),
//!   after every call that can leave the module's own code (imports,
//!   `call_indirect`, `call_ref`, builtins and libcalls), after every call to
//!   a function that may tail-call out of the module, and at the start of
//!   every `try_call` successor.
//! * The array-to-wasm entry trampoline saves the host's register before
//!   entering wasm and restores it on both the normal and the trap path.
//!
//! The designated global is chosen from the module's own bytes (see
//! [`designate`]). The choice affects speed only: the scheme above is correct
//! for any defined mutable `i32` global.
//!
//! [`Tunables::ix_pinned_sp`]: crate::Tunables::ix_pinned_sp

use crate::prelude::*;
use crate::{
    DefinedFuncIndex, EntityIndex, EntitySet, FuncIndex, GlobalIndex, Module, PrimaryMap,
    SecondaryMap, WasmValType,
};
use core::sync::atomic::AtomicU64;

/// `Tunables::ix_pinned_sp` values.
pub mod mode {
    /// Off: upstream code generation.
    pub const OFF: u8 = 0;
    /// On: the design described in the module docs.
    pub const ON: u8 = 1;
    /// Bench only: reserve the pinned register (so no function may allocate
    /// it) but leave every global access as it is. Measures the cost of one
    /// register fewer.
    pub const RESERVE: u8 = 2;
    /// Bench only, unsound in general: the global lives only in the register
    /// while wasm runs. The entry trampoline loads it from the slot and
    /// stores it back on a normal return. Correct only for a single instance
    /// that makes no imports, is never re-entered and never traps. It is the
    /// ceiling of what a pinned register can save.
    pub const REG_ONLY: u8 = 3;
    /// Bench only: upstream code generation plus a counter bump at every
    /// `global.get`/`global.set` of the designated global (see
    /// [`super::IX_PINNED_SP_COUNTS`]). The counters' address is baked into
    /// the code, so such code must never be serialized and loaded by another
    /// process.
    pub const COUNT: u8 = 4;

    /// Whether `mode` needs Cranelift's `enable_pinned_reg`.
    pub fn uses_pinned_reg(mode: u8) -> bool {
        matches!(mode, ON | RESERVE | REG_ONLY)
    }
}

/// Executed `global.get` (index 0) and `global.set` (index 1) of designated
/// globals, counted by code compiled in [`mode::COUNT`].
pub static IX_PINNED_SP_COUNTS: [AtomicU64; 2] = [AtomicU64::new(0), AtomicU64::new(0)];

/// The name LLVM's wasm linker gives the shadow stack pointer.
pub const STACK_POINTER_NAME: &str = "__stack_pointer";

/// Per-module facts the code generator needs, all read from the module.
#[derive(Debug)]
pub struct IxPinnedSp {
    /// The designated global: defined by the module, mutable, `i32`.
    pub global: GlobalIndex,
    /// Defined functions that may, directly or through a chain of direct
    /// `return_call`s, tail-call something outside the module's direct-call
    /// graph (`return_call_indirect`, `return_call_ref`, or `return_call` of
    /// an import). Such a callee can return to its caller with the register
    /// holding another instance's value, so callers reload after calling it.
    pub tail_out: EntitySet<DefinedFuncIndex>,
}

/// Picks the designated global:
///
/// 1. the global the name section calls `__stack_pointer`, if that global is
///    defined, mutable and `i32` (if it is named but not eligible, none);
/// 2. with no such name (a stripped module), the first defined, mutable,
///    `i32` global that is not exported. That is where `wasm-ld` puts
///    `__stack_pointer` (global 0) and where our linker puts it too.
pub fn designate(module: &Module, named: Option<GlobalIndex>) -> Option<GlobalIndex> {
    let eligible = |g: GlobalIndex| {
        module.defined_global_index(g).is_some()
            && module.globals[g].mutability
            && module.globals[g].wasm_ty == WasmValType::I32
    };
    if let Some(g) = named {
        return eligible(g).then_some(g);
    }
    let exported: Vec<GlobalIndex> = module
        .exports
        .values()
        .filter_map(|e| match e {
            EntityIndex::Global(g) => Some(*g),
            _ => None,
        })
        .collect();
    module
        .globals
        .keys()
        .find(|g| eligible(*g) && !exported.contains(g))
}

/// How one defined function's body ends calls in tail position.
#[derive(Default)]
pub struct TailCalls {
    /// It has a `return_call_indirect`, a `return_call_ref`, a `return_call`
    /// of an import, or a body that failed to parse (conservative).
    pub out: bool,
    /// Defined functions it `return_call`s directly.
    pub direct: Vec<DefinedFuncIndex>,
}

/// Scans one function body for tail calls.
pub fn scan_tail_calls(module: &Module, body: &wasmparser::FunctionBody<'_>) -> TailCalls {
    let mut t = TailCalls::default();
    let mut ops = match body.get_operators_reader() {
        Ok(ops) => ops,
        Err(_) => {
            t.out = true;
            return t;
        }
    };
    while !ops.eof() {
        match ops.read() {
            Ok(wasmparser::Operator::ReturnCall { function_index }) => {
                match module.defined_func_index(FuncIndex::from_u32(function_index)) {
                    Some(d) => t.direct.push(d),
                    None => t.out = true,
                }
            }
            Ok(
                wasmparser::Operator::ReturnCallIndirect { .. }
                | wasmparser::Operator::ReturnCallRef { .. },
            ) => t.out = true,
            Ok(_) => {}
            Err(_) => {
                t.out = true;
                return t;
            }
        }
    }
    t
}

/// Closes `scans` (indexed by defined function) over direct tail calls: a
/// function is in the result if it tail-calls out itself, or `return_call`s a
/// function that is.
pub fn tail_out_closure(
    scans: &PrimaryMap<DefinedFuncIndex, TailCalls>,
) -> EntitySet<DefinedFuncIndex> {
    let mut out = EntitySet::with_capacity(scans.len());
    // Reverse edges: callee -> functions that `return_call` it.
    let mut callers: SecondaryMap<DefinedFuncIndex, Vec<DefinedFuncIndex>> = SecondaryMap::new();
    let mut work = Vec::new();
    for (f, t) in scans.iter() {
        for &g in &t.direct {
            callers[g].push(f);
        }
        if t.out {
            out.insert(f);
            work.push(f);
        }
    }
    while let Some(g) = work.pop() {
        for &f in &callers[g] {
            if out.insert(f) {
                work.push(f);
            }
        }
    }
    out
}
