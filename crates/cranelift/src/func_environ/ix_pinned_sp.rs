//! ix fork: the shadow stack pointer global cached in the pinned register.
//!
//! The design and its invariant are described in
//! `wasmtime_environ::ix_pinned_sp`. In short: the global's `VMContext` slot
//! is always the truth, and at every point where translated code reads the
//! pinned register it holds the slot's value for the current instance. This
//! file emits the reads, the write-through writes and every reload that keeps
//! that invariant.

use super::FuncEnvironment;
use cranelift_codegen::cursor::{Cursor, FuncCursor};
use cranelift_codegen::ir::{self, ArgumentPurpose, InstBuilder, MemFlagsData, Opcode, types};
use cranelift_frontend::FunctionBuilder;
use wasmtime_environ::ix_pinned_sp::{IX_PINNED_SP_COUNTS, mode};
use wasmtime_environ::{FuncKey, GlobalIndex, ModuleTranslation, VMOffsets};

/// Per-function state, present only when the module has a designated global
/// and this is a translated wasm function.
#[derive(Clone, Copy, Debug)]
pub(crate) struct IxPsp {
    /// `Tunables::ix_pinned_sp`.
    pub mode: u8,
    /// The designated global.
    pub global: GlobalIndex,
    /// Its slot's offset from `vmctx`.
    pub offset: i32,
    /// The function can only be reached through direct calls from the
    /// module's own functions (not escaping, not the start function). Such a
    /// function is entered with the register already valid.
    pub internal: bool,
}

impl IxPsp {
    pub(crate) fn new(
        translation: &ModuleTranslation<'_>,
        offsets: &VMOffsets<u8>,
        key: FuncKey,
        tunables_mode: u8,
    ) -> Option<IxPsp> {
        let facts = translation.ix_pinned_sp.as_ref()?;
        let FuncKey::DefinedWasmFunction(_, def_func) = key else {
            return None;
        };
        let module = &translation.module;
        let def_global = module.defined_global_index(facts.global)?;
        let offset = i32::try_from(offsets.globals().at(def_global)).unwrap();
        let func = module.func_index(def_func);
        let internal =
            !module.functions[func].is_escaping() && translation.start_func != Some(func);
        Some(IxPsp {
            mode: tunables_mode,
            global: facts.global,
            offset,
            internal,
        })
    }
}

impl FuncEnvironment<'_> {
    /// The designated-global state, if `global` is the designated global.
    pub(crate) fn ix_psp_for(&self, global: GlobalIndex) -> Option<IxPsp> {
        self.ix_psp.filter(|p| p.global == global)
    }

    fn ix_psp_flags(&mut self, func: &mut ir::Function, global: GlobalIndex) -> MemFlagsData {
        let region = self.global_alias_region(func, global);
        MemFlagsData::trusted().with_alias_region(Some(region))
    }

    /// `global.get` of the designated global. Returns `None` when the
    /// ordinary load should be emitted.
    pub(crate) fn ix_psp_global_get(
        &mut self,
        builder: &mut FunctionBuilder<'_>,
        p: IxPsp,
    ) -> Option<ir::Value> {
        match p.mode {
            mode::ON | mode::REG_ONLY => {
                let r = builder.ins().get_pinned_reg(types::I64);
                Some(builder.ins().ireduce(types::I32, r))
            }
            mode::COUNT => {
                self.ix_psp_count(builder, p.global, 0);
                None
            }
            _ => None,
        }
    }

    /// `global.set` of the designated global, after the ordinary store was
    /// decided. Returns whether the ordinary store must still be emitted.
    pub(crate) fn ix_psp_global_set(
        &mut self,
        builder: &mut FunctionBuilder<'_>,
        p: IxPsp,
        val: ir::Value,
    ) -> bool {
        match p.mode {
            mode::ON => {
                let w = builder.ins().uextend(types::I64, val);
                builder.ins().set_pinned_reg(w);
                true
            }
            mode::REG_ONLY => {
                let w = builder.ins().uextend(types::I64, val);
                builder.ins().set_pinned_reg(w);
                false
            }
            mode::COUNT => {
                self.ix_psp_count(builder, p.global, 1);
                true
            }
            _ => true,
        }
    }

    fn ix_psp_count(&mut self, builder: &mut FunctionBuilder<'_>, global: GlobalIndex, i: usize) {
        let flags = self.ix_psp_flags(builder.func, global);
        let addr = core::ptr::from_ref(&IX_PINNED_SP_COUNTS[i]) as i64;
        let addr = builder.ins().iconst(types::I64, addr);
        let n = builder.ins().load(types::I64, flags, addr, 0);
        let n = builder.ins().iadd_imm(n, 1);
        builder.ins().store(flags, n, addr, 0);
    }

    /// Function entry: reload the register unless the function is internal.
    pub(crate) fn ix_psp_function_entry(&mut self, builder: &mut FunctionBuilder<'_>) {
        let Some(p) = self.ix_psp else { return };
        if p.mode != mode::ON || p.internal {
            return;
        }
        let flags = self.ix_psp_flags(builder.func, p.global);
        let vmctx = builder
            .func
            .special_param(ArgumentPurpose::VMContext)
            .unwrap();
        let v = builder.ins().load(types::I32, flags, vmctx, p.offset);
        let w = builder.ins().uextend(types::I64, v);
        builder.ins().set_pinned_reg(w);
    }

    /// After translation: reload the register from the slot after every call
    /// that can leave the module's own code, and at the start of every
    /// `try_call` successor.
    ///
    /// The only calls that need no reload are direct `call`s to a function
    /// this module defines that cannot tail-call out of the module: such a
    /// callee keeps the invariant itself and returns with the register equal
    /// to the slot. Everything else (imports, `call_indirect`, `call_ref`,
    /// builtins, libcalls, other modules' functions in a component) may run
    /// host code or another instance's code, which can change the slot or
    /// leave its own value in the register.
    pub(crate) fn ix_psp_insert_reloads(&mut self, func: &mut ir::Function) {
        let Some(p) = self.ix_psp else { return };
        if p.mode != mode::ON {
            return;
        }
        let flags = self.ix_psp_flags(func, p.global);
        let vmctx = func.special_param(ArgumentPurpose::VMContext).unwrap();
        let this_module = self.translation.module_index();
        let tail_out = &self.translation.ix_pinned_sp.as_ref().unwrap().tail_out;

        let mut successors = Vec::new();
        let mut pos = FuncCursor::new(func);
        while let Some(_block) = pos.next_block() {
            while let Some(inst) = pos.next_inst() {
                let reload_after = match pos.func.dfg.insts[inst].opcode() {
                    Opcode::Call => {
                        let ir::InstructionData::Call { func_ref, .. } = pos.func.dfg.insts[inst]
                        else {
                            unreachable!()
                        };
                        let stays_inside = match pos.func.dfg.ext_funcs[func_ref].name {
                            ir::ExternalName::User(r) => {
                                let name = &pos.func.params.user_named_funcs()[r];
                                match FuncKey::from_raw_parts(name.namespace, name.index) {
                                    FuncKey::DefinedWasmFunction(m, f) => {
                                        m == this_module && !tail_out.contains(f)
                                    }
                                    _ => false,
                                }
                            }
                            _ => false,
                        };
                        !stays_inside
                    }
                    Opcode::CallIndirect => true,
                    Opcode::TryCall | Opcode::TryCallIndirect => {
                        let dfg = &pos.func.dfg;
                        for dest in dfg.insts[inst]
                            .branch_destination(&dfg.jump_tables, &dfg.exception_tables)
                        {
                            successors.push(dest.block(&dfg.value_lists));
                        }
                        false
                    }
                    _ => false,
                };
                if reload_after {
                    // Insert after `inst`, then park the cursor on the last
                    // inserted instruction so the walk resumes with the
                    // original next one.
                    pos.goto_after_inst(inst);
                    let v = pos.ins().load(types::I32, flags, vmctx, p.offset);
                    let w = pos.ins().uextend(types::I64, v);
                    let last = pos.ins().set_pinned_reg(w);
                    pos.goto_inst(last);
                }
            }
        }
        successors.sort_unstable();
        successors.dedup();
        for block in successors {
            pos.goto_first_insertion_point(block);
            let v = pos.ins().load(types::I32, flags, vmctx, p.offset);
            let w = pos.ins().uextend(types::I64, v);
            pos.ins().set_pinned_reg(w);
        }
    }
}
