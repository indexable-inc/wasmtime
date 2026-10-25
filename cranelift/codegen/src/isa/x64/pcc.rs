//! Proof-carrying-code validation for x64 VCode.
//!
//! Almost every x64 instruction is an `Inst::External` wrapping an instruction
//! of the `cranelift-assembler-x64` crate. For those, the assembler generates
//! two summaries (`pcc_mem` and `pcc_gpr_write_bits`) that this checker works
//! from:
//!
//! - Every memory operand whose amode carries the `checked` flag must be proven
//!   in bounds of a memory type, whatever the instruction is. The access size
//!   comes from the operand's location (`m32`, `xmm_m128`, ...), so it is
//!   right for every instruction, not just the ones listed below.
//! - Every register that the instruction defines and that carries a fact must
//!   have that fact proven. A handful of instructions (moves, `lea`, add, sub,
//!   shift, and, compares and conditional moves, constant materialization) have
//!   precise semantics here; for any other instruction the only fact that can
//!   be proven is the one every 32-bit GPR write gives (the upper 32 bits are
//!   zero). Anything else is an error, so an unknown instruction can never
//!   carry an unproven fact.

use crate::ir::pcc::*;
use crate::ir::types::*;
use crate::isa::x64::inst::Inst;
use crate::isa::x64::inst::args::{Amode, Gpr, RegMem, SyntheticAmode, ToWritableReg};
use crate::isa::x64::inst::external::{CraneliftRegisters, PairedGpr};
use crate::machinst::pcc::*;
use crate::machinst::{InsnIndex, VCode};
use crate::machinst::{Reg, Writable};
use crate::trace;
use cranelift_assembler_x64 as asm;

type AsmInst = asm::inst::Inst<CraneliftRegisters>;

fn undefined_result(
    ctx: &FactContext,
    vcode: &mut VCode<Inst>,
    dst: Writable<Gpr>,
    reg_bits: u16,
    result_bits: u16,
) -> PccResult<()> {
    check_output(ctx, vcode, dst.to_writable_reg(), &[], |_vcode| {
        clamp_range(ctx, reg_bits, result_bits, None)
    })
}

fn ensure_no_fact(vcode: &VCode<Inst>, reg: Reg) -> PccResult<()> {
    if vcode.vreg_fact(reg.into()).is_some() {
        Err(PccError::UnsupportedFact)
    } else {
        Ok(())
    }
}

/// Flow-state between facts.
#[derive(Clone, Debug, Default)]
pub(crate) struct FactFlowState {
    cmp_flags: Option<(Fact, Fact)>,
    /// `src -> dst` for each `movl src, dst` (register source) seen so far in
    /// the block: `dst` holds `zext32(src)`. Vregs are SSA here, so the
    /// relation stays true for the rest of the block. A 32-bit compare on
    /// `src` reads `dst`'s fact (the uextend that defines a Wasm index
    /// symbol), see `zext32_fact`.
    zext32: crate::FxHashMap<Reg, Reg>,
}

pub(crate) fn check(
    ctx: &FactContext,
    vcode: &mut VCode<Inst>,
    inst_idx: InsnIndex,
    state: &mut FactFlowState,
) -> PccResult<()> {
    trace!("Checking facts on inst: {:?}", vcode[inst_idx]);

    // We only persist flag state for one instruction, because we
    // can't exhaustively enumerate all flags-effecting ops; so take
    // the `cmp_state` here and perhaps use it below but don't let it
    // remain.
    let cmp_flags = state.cmp_flags.take();

    match vcode[inst_idx] {
        Inst::Args { .. } => {
            // Defs on the args have "axiomatic facts": we trust the
            // ABI code to pass through the values unharmed, so the
            // facts given to us in the CLIF should still be true.
            Ok(())
        }

        Inst::CheckedSRemSeq {
            dst_quotient,
            dst_remainder,
            ..
        } => {
            undefined_result(ctx, vcode, dst_quotient, 64, 64)?;
            undefined_result(ctx, vcode, dst_remainder, 64, 64)?;
            Ok(())
        }

        Inst::CheckedSRemSeq8 { dst, .. } => undefined_result(ctx, vcode, dst, 64, 64),

        Inst::MovFromPReg { dst, .. } => undefined_result(ctx, vcode, dst, 64, 64),
        Inst::MovToPReg { .. } => Ok(()),

        Inst::XmmCmove { dst, .. } => ensure_no_fact(vcode, dst.to_writable_reg().to_reg()),

        Inst::StackProbeLoop { tmp, .. } => ensure_no_fact(vcode, tmp.to_reg()),

        Inst::CvtUint64ToFloatSeq {
            dst,
            tmp_gpr1,
            tmp_gpr2,
            ..
        } => {
            ensure_no_fact(vcode, dst.to_writable_reg().to_reg())?;
            ensure_no_fact(vcode, tmp_gpr1.to_writable_reg().to_reg())?;
            ensure_no_fact(vcode, tmp_gpr2.to_writable_reg().to_reg())?;
            Ok(())
        }

        Inst::CvtFloatToSintSeq {
            dst,
            tmp_gpr,
            tmp_xmm,
            ..
        } => {
            undefined_result(ctx, vcode, dst, 64, 64)?;
            ensure_no_fact(vcode, tmp_gpr.to_writable_reg().to_reg())?;
            ensure_no_fact(vcode, tmp_xmm.to_writable_reg().to_reg())?;
            Ok(())
        }

        Inst::CvtFloatToUintSeq {
            dst,
            tmp_gpr,
            tmp_xmm,
            tmp_xmm2,
            ..
        } => {
            undefined_result(ctx, vcode, dst, 64, 64)?;
            ensure_no_fact(vcode, tmp_gpr.to_writable_reg().to_reg())?;
            ensure_no_fact(vcode, tmp_xmm.to_writable_reg().to_reg())?;
            ensure_no_fact(vcode, tmp_xmm2.to_writable_reg().to_reg())?;
            Ok(())
        }

        Inst::XmmMinMaxSeq { dst, .. } => ensure_no_fact(vcode, dst.to_writable_reg().to_reg()),

        // A call's results are whatever the callee returns: no fact on them
        // can be checked.
        Inst::CallKnown { .. } | Inst::CallUnknown { .. } if vcode.inst_defines_facts(inst_idx) => {
            Err(PccError::UnsupportedFact)
        }

        Inst::CallKnown { .. }
        | Inst::ReturnCallKnown { .. }
        | Inst::JmpKnown { .. }
        | Inst::WinchJmpIf { .. }
        | Inst::JmpCond { .. }
        | Inst::JmpCondOr { .. }
        | Inst::TrapIf { .. }
        | Inst::TrapIfAnd { .. }
        | Inst::TrapIfOr { .. } => Ok(()),
        Inst::Rets { .. } => Ok(()),

        Inst::ReturnCallUnknown { .. } => Ok(()),

        Inst::CallUnknown { ref info } => match &info.dest {
            RegMem::Mem { addr } => {
                check_mem_synthetic(ctx, addr, vcode, I64, None)?;
                Ok(())
            }
            RegMem::Reg { .. } => Ok(()),
        },

        Inst::JmpTableSeq { tmp1, tmp2, .. } => {
            ensure_no_fact(vcode, tmp1.to_reg())?;
            ensure_no_fact(vcode, tmp2.to_reg())?;
            Ok(())
        }

        Inst::LoadExtName { dst, .. } => {
            ensure_no_fact(vcode, *dst.to_reg())?;
            Ok(())
        }

        Inst::AtomicRmwSeq {
            ty,
            ref mem,
            temp,
            dst_old,
            ..
        } => {
            ensure_no_fact(vcode, *dst_old.to_reg())?;
            ensure_no_fact(vcode, *temp.to_reg())?;
            check_mem_synthetic(ctx, mem, vcode, ty, Some(None))?;
            Ok(())
        }

        Inst::Atomic128RmwSeq { ref args } => {
            ensure_no_fact(vcode, *args.dst_old_low.to_reg())?;
            ensure_no_fact(vcode, *args.dst_old_high.to_reg())?;
            ensure_no_fact(vcode, *args.temp_low.to_reg())?;
            ensure_no_fact(vcode, *args.temp_high.to_reg())?;
            check_mem_synthetic(ctx, &args.mem_low, vcode, I64, Some(None))?;
            check_mem_synthetic(ctx, &args.mem_high, vcode, I64, Some(None))?;
            Ok(())
        }

        Inst::Atomic128XchgSeq { ref args } => {
            ensure_no_fact(vcode, *args.dst_old_low.to_reg())?;
            ensure_no_fact(vcode, *args.dst_old_high.to_reg())?;
            check_mem_synthetic(ctx, &args.mem_low, vcode, I64, Some(None))?;
            check_mem_synthetic(ctx, &args.mem_high, vcode, I64, Some(None))?;
            Ok(())
        }

        Inst::XmmUninitializedValue { dst } => {
            ensure_no_fact(vcode, dst.to_writable_reg().to_reg())
        }

        Inst::GprUninitializedValue { dst } => {
            ensure_no_fact(vcode, dst.to_writable_reg().to_reg())
        }

        Inst::ElfTlsGetAddr { dst, .. } | Inst::MachOTlsGetAddr { dst, .. } => {
            ensure_no_fact(vcode, dst.to_writable_reg().to_reg())
        }
        Inst::CoffTlsGetAddr { dst, tmp, .. } => {
            ensure_no_fact(vcode, dst.to_writable_reg().to_reg())?;
            ensure_no_fact(vcode, tmp.to_writable_reg().to_reg())?;
            Ok(())
        }

        Inst::Unwind { .. } | Inst::DummyUse { .. } => Ok(()),

        // A stack switch is a call into another stack: it reads and writes
        // only the continuations' control contexts (runtime structures, never
        // a `checked` heap access) and returns a payload the checker knows
        // nothing about, like a call's return value.
        Inst::StackSwitchBasic { out_payload0, .. } => {
            ensure_no_fact(vcode, *out_payload0.to_reg())
        }

        Inst::LabelAddress { dst, .. } => ensure_no_fact(vcode, dst.to_writable_reg().to_reg()),

        Inst::SequencePoint { .. } => Ok(()),

        Inst::External { ref inst } => {
            let inst = inst.clone();
            let result = check_external(ctx, vcode, inst_idx, &inst, cmp_flags.clone(), state);
            // Moves and `lea`s never write EFLAGS (and the compare facts
            // name values, not registers), so a compare stays usable across
            // them: lowering often puts a constant materialization or an
            // address computation between a `cmp` and its `cmov`.
            if state.cmp_flags.is_none() && preserves_flags(&inst) {
                state.cmp_flags = cmp_flags;
            }
            result
        }
    }
}

/// Whether an assembler instruction certainly leaves EFLAGS alone: the plain
/// and extending GPR moves. (Not `lea`: emission turns `lea` into `add`,
/// which writes the flags, when register allocation gives the destination
/// the base or index register; the checker runs before register allocation.)
fn preserves_flags(inst: &AsmInst) -> bool {
    use asm::inst::Inst as I;
    matches!(
        inst,
        I::movq_rm(_)
            | I::movl_rm(_)
            | I::movq_mr(_)
            | I::movl_mr(_)
            | I::movl_oi(_)
            | I::movabsq_oi(_)
            | I::movq_mi_sxl(_)
            | I::movl_mi(_)
            | I::movzbl_rm(_)
            | I::movzbq_rm(_)
            | I::movzwl_rm(_)
            | I::movzwq_rm(_)
    )
}

/// Check a memory access through a codegen-side amode (the few x64
/// pseudo-instructions that are not assembler instructions).
///
/// `store` is `None` for a load and `Some(stored_fact)` for a store (atomic
/// read-modify-writes are checked as stores, which also checks the address for
/// the load half).
fn check_mem_synthetic(
    ctx: &FactContext,
    amode: &SyntheticAmode,
    vcode: &VCode<Inst>,
    ty: Type,
    store: Option<Option<&Fact>>,
) -> PccResult<Option<Fact>> {
    let addr = match amode {
        SyntheticAmode::Real(amode) if amode.get_flags().checked() => {
            compute_addr(ctx, vcode, amode, 64).ok_or(PccError::MissingFact)?
        }
        _ => return Ok(None),
    };
    match store {
        None => Ok(ctx.load(&addr, ty)?.cloned()),
        Some(stored_fact) => {
            ctx.store(&addr, ty, stored_fact)?;
            Ok(None)
        }
    }
}

fn compute_addr(ctx: &FactContext, vcode: &VCode<Inst>, amode: &Amode, bits: u16) -> Option<Fact> {
    trace!("compute_addr: {:?}", amode);
    match *amode {
        Amode::ImmReg { simm32, base, .. } => {
            let base = get_fact_or_default(vcode, base, bits);
            trace!("base = {:?}", base);
            let simm32: i64 = simm32.into();
            ctx.offset(&base, bits, simm32)
        }
        Amode::ImmRegRegShift {
            simm32,
            base,
            index,
            shift,
            ..
        } => {
            let base = get_fact_or_default(vcode, base.into(), bits);
            let index = get_fact_or_default(vcode, index.into(), bits);
            trace!("base = {:?} index = {:?}", base, index);
            let shifted = ctx.shl(&index, bits, shift.into())?;
            let sum = ctx.add(&base, &shifted, bits)?;
            let simm32: i64 = simm32.into();
            ctx.offset(&sum, bits, simm32)
        }
        Amode::RipRelative { .. } => None,
    }
}

/// The fact describing the address an assembler amode computes, or `None` if
/// it cannot be described (a RIP-relative or frame-offset-relative address).
fn compute_asm_addr(
    ctx: &FactContext,
    vcode: &VCode<Inst>,
    amode: &asm::Amode<Gpr>,
) -> Option<Fact> {
    trace!("compute_asm_addr: {:?}", amode);
    match amode {
        asm::Amode::ImmReg { base, simm32, .. } => {
            if simm32.offset.is_some() {
                // Offsets only known at emission time are frame offsets
                // (stack slots, incoming arguments): not describable.
                return None;
            }
            let base = get_fact_or_default(vcode, base.to_reg(), 64);
            ctx.offset(&base, 64, simm32.simm32.value().into())
        }
        asm::Amode::ImmRegRegShift {
            base,
            index,
            scale,
            simm32,
            ..
        } => {
            let base = get_fact_or_default(vcode, base.to_reg(), 64);
            let index = get_fact_or_default(vcode, index.as_ref().to_reg(), 64);
            let shifted = ctx.shl(&index, 64, scale.log2().into())?;
            let sum = ctx.add(&base, &shifted, 64)?;
            ctx.offset(&sum, 64, simm32.value().into())
        }
        asm::Amode::RipRelative { .. } => None,
    }
}

/// The address sum of a 32-bit `lea`, from the inputs' numeric ranges only.
/// A range fact at least 32 bits wide bounds the register's low 32 bits,
/// which are all a 32-bit `lea` reads, so each is used as a 64-bit range; the
/// caller keeps the result only when it fits in 32 bits, where it is exact.
/// A narrower fact says nothing about bits above its width, so it is not used.
fn compute_leal(ctx: &FactContext, vcode: &VCode<Inst>, amode: &asm::Amode<Gpr>) -> Option<Fact> {
    let range = |r: Reg| match get_fact_or_default(vcode, r, 64) {
        Fact::Range { bit_width, min, max } if bit_width >= 32 => Some(Fact::Range {
            bit_width: 64,
            min,
            max,
        }),
        _ => None,
    };
    match amode {
        asm::Amode::ImmReg { base, simm32, .. } => {
            if simm32.offset.is_some() {
                return None;
            }
            ctx.offset(&range(base.to_reg())?, 64, simm32.simm32.value().into())
        }
        asm::Amode::ImmRegRegShift {
            base,
            index,
            scale,
            simm32,
            ..
        } => {
            let base = range(base.to_reg())?;
            let index = range(index.as_ref().to_reg())?;
            let shifted = ctx.shl(&index, 64, scale.log2().into())?;
            let sum = ctx.add(&base, &shifted, 64)?;
            ctx.offset(&sum, 64, simm32.value().into())
        }
        asm::Amode::RipRelative { .. } => None,
    }
}

fn access_type(bytes: u8) -> PccResult<Type> {
    Ok(match bytes {
        1 => I8,
        2 => I16,
        4 => I32,
        8 => I64,
        16 => I8X16,
        _ => return Err(PccError::UnimplementedInst),
    })
}

fn gpr(g: &Gpr) -> Reg {
    g.to_reg()
}

/// A register operand of an assembler instruction.
fn rg(g: &asm::Gpr<Gpr>) -> Reg {
    g.as_ref().to_reg()
}

/// A written register operand of an assembler instruction.
fn wg(g: &asm::Gpr<Writable<Gpr>>) -> Writable<Reg> {
    wgpr(g.as_ref())
}

fn wgpr(g: &Writable<Gpr>) -> Writable<Reg> {
    g.map(|g| g.to_reg())
}

fn paired_out(p: &PairedGpr) -> Writable<Reg> {
    wgpr(&p.write)
}

fn paired_in(p: &PairedGpr) -> Reg {
    p.read.to_reg()
}

/// A register-or-memory source operand.
enum Src {
    Reg(Reg),
    Mem,
}

fn src_of(rm: &asm::GprMem<Gpr, Gpr>) -> Src {
    match rm {
        asm::GprMem::Gpr(g) => Src::Reg(gpr(g)),
        asm::GprMem::Mem(_) => Src::Mem,
    }
}

/// The value of an 8-byte constant-pool operand, read little-endian.
fn rip_constant_u64(vcode: &VCode<Inst>, amode: &asm::Amode<Gpr>) -> Option<u64> {
    let asm::Amode::RipRelative {
        target: asm::DeferredTarget::Constant(c),
    } = amode
    else {
        return None;
    };
    let data = vcode
        .constants
        .get(crate::machinst::VCodeConstant::from_u32(c.0))
        .as_slice();
    let bytes: [u8; 8] = data.get(..8)?.try_into().ok()?;
    Some(u64::from_le_bytes(bytes))
}

/// The outcome of checking an instruction's memory operand.
struct MemCheck {
    /// The fact on the loaded value, for a checked load.
    loaded: Option<Fact>,
    /// Whether the access was a checked load (so `loaded` is meaningful).
    checked_load: bool,
}

fn check_external(
    ctx: &FactContext,
    vcode: &mut VCode<Inst>,
    inst_idx: InsnIndex,
    inst: &AsmInst,
    cmp_flags: Option<(Fact, Fact)>,
    state: &mut FactFlowState,
) -> PccResult<()> {
    use asm::inst::Inst as I;

    // Step 1: the memory operand, if any and if `checked`.
    let stored_fact = match inst {
        I::movq_mr(i) => vcode.vreg_fact(rg(&i.r64).into()).cloned(),
        I::movl_mr(i) => vcode.vreg_fact(rg(&i.r32).into()).cloned(),
        _ => None,
    };
    let mut mem = MemCheck {
        loaded: None,
        checked_load: false,
    };
    if let Some(m) = inst.pcc_mem() {
        // `bt m, r` reads at `m + (r >> 3)` (a signed bit offset), not at
        // `m`, so its summary does not bound the access.
        if m.amode.checked() && matches!(inst, I::btw_mr(_) | I::btl_mr(_) | I::btq_mr(_)) {
            return Err(PccError::UnsupportedFact);
        }
        if m.amode.checked() {
            log::debug!("pcc: checking {:?} access of {} bytes", m.kind, m.bytes);
            let ty = access_type(m.bytes)?;
            match m.kind {
                asm::PccMemKind::AddressOnly => {}
                asm::PccMemKind::Load => {
                    let addr =
                        compute_asm_addr(ctx, vcode, m.amode).ok_or(PccError::MissingFact)?;
                    mem.loaded = ctx.load(&addr, ty)?.cloned();
                    mem.checked_load = true;
                }
                asm::PccMemKind::Store => {
                    let addr =
                        compute_asm_addr(ctx, vcode, m.amode).ok_or(PccError::MissingFact)?;
                    ctx.store(&addr, ty, stored_fact.as_ref())?;
                }
                asm::PccMemKind::LoadStore => {
                    let addr =
                        compute_asm_addr(ctx, vcode, m.amode).ok_or(PccError::MissingFact)?;
                    ctx.load(&addr, ty)?;
                    ctx.store(&addr, ty, None)?;
                }
            }
        }
    }

    // Step 2: facts on defined registers.
    match inst {
        // Moves and loads.
        I::movq_rm(i) => check_mov(ctx, vcode, wg(&i.r64), src_of(&i.rm64), &mem, 64, 64),
        I::movl_rm(i) => {
            let src = src_of(&i.rm32);
            if let Src::Reg(src) = src {
                state.zext32.insert(src, wg(&i.r32).to_reg());
            }
            check_mov(ctx, vcode, wg(&i.r32), src, &mem, 32, 32)
        }
        I::movzbl_rm(i) => check_mov(ctx, vcode, wg(&i.r32), src_of(&i.rm8), &mem, 8, 32),
        I::movzbq_rm(i) => check_mov(ctx, vcode, wg(&i.r64), src_of(&i.rm8), &mem, 8, 64),
        I::movzwl_rm(i) => check_mov(ctx, vcode, wg(&i.r32), src_of(&i.rm16), &mem, 16, 32),
        I::movzwq_rm(i) => check_mov(ctx, vcode, wg(&i.r64), src_of(&i.rm16), &mem, 16, 64),

        // Constants.
        I::movl_oi(i) => check_constant(ctx, vcode, wg(&i.r32), 64, i.imm32.value().into()),
        I::movabsq_oi(i) => check_constant(ctx, vcode, wg(&i.r64), 64, i.imm64.value()),
        I::movq_mi_sxl(i) => match &i.rm64 {
            asm::GprMem::Gpr(dst) => {
                let value = i64::from(i.imm32.value()) as u64;
                check_constant(ctx, vcode, wgpr(dst), 64, value)
            }
            asm::GprMem::Mem(_) => Ok(()),
        },
        I::movl_mi(i) => match &i.rm32 {
            asm::GprMem::Gpr(dst) => {
                check_constant(ctx, vcode, wgpr(dst), 64, i.imm32.value().into())
            }
            asm::GprMem::Mem(_) => Ok(()),
        },
        I::xorl_rm(i) => check_xor_zero(ctx, vcode, inst_idx, i.r32.as_ref(), &i.rm32, inst),
        I::xorq_rm(i) => check_xor_zero(ctx, vcode, inst_idx, i.r64.as_ref(), &i.rm64, inst),

        // Address computation.
        I::leaq_rm(i) => {
            let out = wg(&i.r64);
            let ins = amode_regs(&i.m64);
            let amode = i.m64;
            check_output(ctx, vcode, out, &ins, |vcode| {
                Ok(compute_asm_addr(ctx, vcode, &amode))
            })
        }

        // A 32-bit `lea` writes the low 32 bits of the address sum (and zero
        // above). The low 32 bits of each input decide them, so when the sum
        // of the input ranges fits in 32 bits it is the result exactly;
        // otherwise only the zero upper half is known.
        I::leal_rm(i) => {
            let out = wg(&i.r32);
            let ins = amode_regs(&i.m32);
            let amode = i.m32;
            check_output(ctx, vcode, out, &ins, |vcode| {
                Ok(Some(match compute_leal(ctx, vcode, &amode) {
                    Some(Fact::Range { min, max, .. }) if max <= u64::from(u32::MAX) => {
                        Fact::Range {
                            bit_width: 64,
                            min,
                            max,
                        }
                    }
                    _ => Fact::max_range_for_width_extended(32, 64),
                }))
            })
        }

        // Arithmetic.
        I::addq_rm(i) => match src_of(&i.rm64) {
            Src::Reg(rhs) => check_add(ctx, vcode, i.r64.as_ref(), rhs, 64),
            Src::Mem => {
                let rhs = mem_src_fact(vcode, &i.rm64, &mem, 64);
                check_add_fact(ctx, vcode, i.r64.as_ref(), rhs, 64)
            }
        },
        I::addl_rm(i) => match src_of(&i.rm32) {
            Src::Reg(rhs) => check_add(ctx, vcode, i.r32.as_ref(), rhs, 32),
            Src::Mem => {
                let rhs = mem_src_fact(vcode, &i.rm32, &mem, 32);
                check_add_fact(ctx, vcode, i.r32.as_ref(), rhs, 32)
            }
        },
        I::addq_mr(i) => match &i.rm64 {
            asm::GprMem::Gpr(dst) => check_add(ctx, vcode, dst, rg(&i.r64), 64),
            asm::GprMem::Mem(_) => Ok(()),
        },
        I::addl_mr(i) => match &i.rm32 {
            asm::GprMem::Gpr(dst) => check_add(ctx, vcode, dst, rg(&i.r32), 32),
            asm::GprMem::Mem(_) => Ok(()),
        },
        I::addq_mi_sxl(i) => check_offset_rm(ctx, vcode, &i.rm64, i.imm32.value().into(), 64),
        I::addq_mi_sxb(i) => check_offset_rm(ctx, vcode, &i.rm64, i.imm8.value().into(), 64),
        I::addl_mi(i) => check_offset_rm(ctx, vcode, &i.rm32, i.imm32.value().into(), 32),
        I::addl_mi_sxb(i) => check_offset_rm(ctx, vcode, &i.rm32, i.imm8.value().into(), 32),
        I::subq_mi_sxl(i) => check_offset_rm(ctx, vcode, &i.rm64, -i64::from(i.imm32.value()), 64),
        I::subq_mi_sxb(i) => check_offset_rm(ctx, vcode, &i.rm64, -i64::from(i.imm8.value()), 64),
        I::subq_rm(i) => match src_of(&i.rm64) {
            Src::Reg(rhs) => {
                let out = paired_out(i.r64.as_ref());
                let lhs = paired_in(i.r64.as_ref());
                check_binop(ctx, vcode, 64, out, lhs, rhs, |lhs, rhs| {
                    match rhs.as_const(64) {
                        Some(k) => Ok(ctx.offset(lhs, 64, (k as i64).wrapping_neg())),
                        None => Ok(None),
                    }
                })
            }
            Src::Mem => check_generic(ctx, vcode, inst_idx, inst),
        },
        I::shlq_mi(i) => check_shl_rm(ctx, vcode, &i.rm64, i.imm8.value(), 64),
        I::shll_mi(i) => check_shl_rm(ctx, vcode, &i.rm32, i.imm8.value(), 32),
        I::andl_mi(i) => check_and_imm_rm(ctx, vcode, &i.rm32, i.imm32.value().into()),
        I::andq_mi_sxl(i) => {
            let imm = i.imm32.value();
            if imm >= 0 {
                check_and_imm_rm(ctx, vcode, &i.rm64, imm as u64)
            } else {
                check_generic(ctx, vcode, inst_idx, inst)
            }
        }

        // Compares feeding conditional moves (bounds checks with Spectre
        // mitigation).
        I::cmpq_rm(i) => {
            let lhs = get_fact_or_default(vcode, rg(&i.r64), 64);
            let rhs = match src_of(&i.rm64) {
                Src::Reg(rhs) => get_fact_or_default(vcode, rhs, 64),
                Src::Mem => mem_src_fact(vcode, &i.rm64, &mem, 64)
                    .unwrap_or_else(|| Fact::max_range_for_width(64)),
            };
            state.cmp_flags = Some((lhs, rhs));
            Ok(())
        }
        I::cmpq_mr(i) => {
            let lhs = match src_of(&i.rm64) {
                Src::Reg(lhs) => get_fact_or_default(vcode, lhs, 64),
                Src::Mem => mem_src_fact(vcode, &i.rm64, &mem, 64)
                    .unwrap_or_else(|| Fact::max_range_for_width(64)),
            };
            let rhs = get_fact_or_default(vcode, rg(&i.r64), 64);
            state.cmp_flags = Some((lhs, rhs));
            Ok(())
        }
        I::cmpq_mi(i) => {
            if let Src::Reg(lhs) = src_of(&i.rm64) {
                let lhs = get_fact_or_default(vcode, lhs, 64);
                let rhs = Fact::constant(64, i64::from(i.imm32.value()) as u64);
                state.cmp_flags = Some((lhs, rhs));
            }
            Ok(())
        }
        I::cmpq_mi_sxb(i) => {
            if let Src::Reg(lhs) = src_of(&i.rm64) {
                let lhs = get_fact_or_default(vcode, lhs, 64);
                let rhs = Fact::constant(64, i64::from(i.imm8.value()) as u64);
                state.cmp_flags = Some((lhs, rhs));
            }
            Ok(())
        }
        // `test r, r` sets ZF, SF and PF like `cmp r, 0`, and clears CF and
        // OF as `cmp r, 0` does (subtracting 0 never borrows or overflows),
        // so it records the same flags. Lowering uses it for `icmp _, x, 0`.
        I::testq_mr(i) => {
            if let Src::Reg(lhs) = src_of(&i.rm64) {
                if lhs == rg(&i.r64) {
                    let lhs = get_fact_or_default(vcode, lhs, 64);
                    state.cmp_flags = Some((lhs, Fact::constant(64, 0)));
                }
            }
            Ok(())
        }
        // `testl r, r` sets ZF from the low 32 bits and clears CF: for the
        // unsigned conditions the checker uses (`a`, `ae`, `ne`, `e`) these
        // are the flags of the 64-bit `zext32(r) - 0`. The egraph narrows
        // `icmp ugt (uextend x), 0` to `icmp ne x, 0` on the i32, so this is
        // how the zero-bound check of `offset=4294967295` lowers.
        I::testl_mr(i) => {
            if let Src::Reg(lhs) = src_of(&i.rm32) {
                if lhs == rg(&i.r32) {
                    let lhs = zext32_fact(vcode, state, lhs);
                    state.cmp_flags = Some((lhs, Fact::constant(64, 0)));
                }
            }
            Ok(())
        }
        // After a compare whose right-hand side is the constant 0, CF is 0, so
        // `ne` (ZF = 0) is exactly `a` (CF = 0 and ZF = 0) and `e` is exactly
        // `be`. Lowering turns `icmp ugt x, 0` (a bounds check whose adjusted
        // bound is 0: a static offset of reservation minus access size) into
        // `ne`. `cmove dst, src` is `cmova` with its arms swapped: the taken
        // side knows `0 >= lhs`, the other `lhs > 0`.
        I::cmovneq_rm(i) if cmp_rhs_is_zero(&cmp_flags) => check_cmov(
            ctx,
            vcode,
            i.r64.as_ref(),
            &i.rm64,
            &mem,
            cmp_flags.map(|f| (f, InequalityKind::Strict)),
        ),
        I::cmoveq_rm(i) if cmp_rhs_is_zero(&cmp_flags) => check_cmov(
            ctx,
            vcode,
            i.r64.as_ref(),
            &i.rm64,
            &mem,
            cmp_flags.map(|(lhs, rhs)| ((rhs, lhs), InequalityKind::Loose)),
        ),
        I::cmovaq_rm(i) => check_cmov(
            ctx,
            vcode,
            i.r64.as_ref(),
            &i.rm64,
            &mem,
            cmp_flags.map(|f| (f, InequalityKind::Strict)),
        ),
        I::cmovaeq_rm(i) => check_cmov(
            ctx,
            vcode,
            i.r64.as_ref(),
            &i.rm64,
            &mem,
            cmp_flags.map(|f| (f, InequalityKind::Loose)),
        ),
        // Any other 64-bit conditional move: the result is one of its two
        // inputs, whatever the condition.
        I::cmovoq_rm(asm::inst::cmovoq_rm { r64, rm64 })
        | I::cmovnoq_rm(asm::inst::cmovnoq_rm { r64, rm64 })
        | I::cmovbq_rm(asm::inst::cmovbq_rm { r64, rm64 })
        | I::cmoveq_rm(asm::inst::cmoveq_rm { r64, rm64 })
        | I::cmovneq_rm(asm::inst::cmovneq_rm { r64, rm64 })
        | I::cmovbeq_rm(asm::inst::cmovbeq_rm { r64, rm64 })
        | I::cmovsq_rm(asm::inst::cmovsq_rm { r64, rm64 })
        | I::cmovnsq_rm(asm::inst::cmovnsq_rm { r64, rm64 })
        | I::cmovpq_rm(asm::inst::cmovpq_rm { r64, rm64 })
        | I::cmovnpq_rm(asm::inst::cmovnpq_rm { r64, rm64 })
        | I::cmovlq_rm(asm::inst::cmovlq_rm { r64, rm64 })
        | I::cmovgeq_rm(asm::inst::cmovgeq_rm { r64, rm64 })
        | I::cmovleq_rm(asm::inst::cmovleq_rm { r64, rm64 })
        | I::cmovgq_rm(asm::inst::cmovgq_rm { r64, rm64 }) => {
            check_cmov(ctx, vcode, r64.as_ref(), rm64, &mem, None)
        }

        _ => check_generic(ctx, vcode, inst_idx, inst),
    }
}

/// The registers an amode reads.
fn amode_regs(amode: &asm::Amode<Gpr>) -> smallvec::SmallVec<[Reg; 2]> {
    match amode {
        asm::Amode::ImmReg { base, .. } => smallvec::smallvec![base.to_reg()],
        asm::Amode::ImmRegRegShift { base, index, .. } => {
            smallvec::smallvec![base.to_reg(), index.as_ref().to_reg()]
        }
        asm::Amode::RipRelative { .. } => smallvec::smallvec![],
    }
}

/// A move or load of `from_bits` zero-extended into a `to_bits` register
/// (`to_bits` is 32 or 64; a 32-bit write zeroes the upper half).
fn check_mov(
    ctx: &FactContext,
    vcode: &mut VCode<Inst>,
    out: Writable<Reg>,
    src: Src,
    mem: &MemCheck,
    from_bits: u16,
    _to_bits: u16,
) -> PccResult<()> {
    match src {
        Src::Reg(src) => check_unop(ctx, vcode, 64, out, src, |src| {
            clamp_range(ctx, 64, from_bits, Some(src.clone()))
        }),
        Src::Mem => {
            let loaded = if mem.checked_load {
                mem.loaded.clone()
            } else {
                None
            };
            check_output(ctx, vcode, out, &[], |_vcode| {
                clamp_range(ctx, 64, from_bits, loaded.clone())
            })
        }
    }
}

fn check_add(
    ctx: &FactContext,
    vcode: &mut VCode<Inst>,
    dst: &PairedGpr,
    rhs: Reg,
    bits: u16,
) -> PccResult<()> {
    let out = paired_out(dst);
    let lhs = paired_in(dst);
    check_binop(ctx, vcode, 64, out, lhs, rhs, |lhs, rhs| {
        clamp_range(ctx, 64, bits, ctx.add(lhs, rhs, bits))
    })
}

/// `dst += rhs` where the rhs fact is already known (a memory operand).
fn check_add_fact(
    ctx: &FactContext,
    vcode: &mut VCode<Inst>,
    dst: &PairedGpr,
    rhs: Option<Fact>,
    bits: u16,
) -> PccResult<()> {
    let out = paired_out(dst);
    let lhs = paired_in(dst);
    let rhs = rhs.unwrap_or_else(|| Fact::max_range_for_width(bits));
    check_unop(ctx, vcode, 64, out, lhs, |lhs| {
        clamp_range(ctx, 64, bits, ctx.add(lhs, &rhs, bits))
    })
}

/// The fact on a memory source operand: the checked load's fact, or the
/// value of a constant-pool operand.
fn mem_src_fact(
    vcode: &VCode<Inst>,
    rm: &asm::GprMem<Gpr, Gpr>,
    mem: &MemCheck,
    bits: u16,
) -> Option<Fact> {
    if mem.checked_load {
        return mem.loaded.clone();
    }
    let asm::GprMem::Mem(amode) = rm else {
        return None;
    };
    let v = rip_constant_u64(vcode, amode)?;
    let v = if bits == 32 { v & 0xffff_ffff } else { v };
    Some(Fact::constant(bits, v))
}

fn check_offset_rm(
    ctx: &FactContext,
    vcode: &mut VCode<Inst>,
    rm: &asm::GprMem<PairedGpr, Gpr>,
    offset: i64,
    bits: u16,
) -> PccResult<()> {
    match rm {
        asm::GprMem::Gpr(dst) => {
            let out = paired_out(dst);
            let src = paired_in(dst);
            check_unop(ctx, vcode, 64, out, src, |src| {
                clamp_range(ctx, 64, bits, ctx.offset(src, bits, offset))
            })
        }
        asm::GprMem::Mem(_) => Ok(()),
    }
}

fn check_shl_rm(
    ctx: &FactContext,
    vcode: &mut VCode<Inst>,
    rm: &asm::GprMem<PairedGpr, Gpr>,
    amount: u8,
    bits: u16,
) -> PccResult<()> {
    match rm {
        asm::GprMem::Gpr(dst) => {
            let out = paired_out(dst);
            let src = paired_in(dst);
            check_unop(ctx, vcode, 64, out, src, |src| {
                clamp_range(ctx, 64, bits, ctx.shl(src, bits, amount.into()))
            })
        }
        asm::GprMem::Mem(_) => Ok(()),
    }
}

fn check_and_imm_rm(
    ctx: &FactContext,
    vcode: &mut VCode<Inst>,
    rm: &asm::GprMem<PairedGpr, Gpr>,
    mask: u64,
) -> PccResult<()> {
    match rm {
        asm::GprMem::Gpr(dst) => {
            let out = paired_out(dst);
            // `x & mask <= mask` for a non-negative mask.
            check_output(ctx, vcode, out, &[], |_vcode| {
                Ok(Some(Fact::Range {
                    bit_width: 64,
                    min: 0,
                    max: mask,
                }))
            })
        }
        asm::GprMem::Mem(_) => Ok(()),
    }
}

/// `xor r, r` with both reads of the same register: the zero idiom.
fn check_xor_zero(
    ctx: &FactContext,
    vcode: &mut VCode<Inst>,
    inst_idx: InsnIndex,
    dst: &PairedGpr,
    rm: &asm::GprMem<Gpr, Gpr>,
    inst: &AsmInst,
) -> PccResult<()> {
    match rm {
        asm::GprMem::Gpr(src) if gpr(src) == paired_in(dst) => {
            check_constant(ctx, vcode, paired_out(dst), 64, 0)
        }
        _ => check_generic(ctx, vcode, inst_idx, inst),
    }
}

/// The fact of `zext32(reg)` as a 64-bit value: the fact of a register that a
/// `movl` earlier in the block filled with exactly that value, else `reg`'s
/// own 64-bit range when it already fits in 32 bits, else any 32-bit value.
fn zext32_fact(vcode: &VCode<Inst>, state: &FactFlowState, reg: Reg) -> Fact {
    if let Some(&wide) = state.zext32.get(&reg) {
        return get_fact_or_default(vcode, wide, 64);
    }
    match get_fact_or_default(vcode, reg, 64) {
        f @ Fact::Range {
            bit_width: 64, max, ..
        } if max <= u64::from(u32::MAX) => f,
        _ => Fact::max_range_for_width_extended(32, 64),
    }
}

/// Whether the last compare's right-hand side is the constant 0 (a `test r, r`
/// or a compare against an immediate or a register proven to hold 0).
fn cmp_rhs_is_zero(cmp_flags: &Option<(Fact, Fact)>) -> bool {
    cmp_flags
        .as_ref()
        .is_some_and(|(_, rhs)| rhs.as_const(64) == Some(0))
}

/// `cmov{a,ae} dst, src`: `dst = cond ? src : dst`, after a compare that set
/// `cmp_flags` to `(lhs, rhs)` (the flags of `lhs - rhs`). The true side
/// knows `lhs > rhs` (`a`, strict) or `lhs >= rhs` (`ae`, loose).
fn check_cmov(
    ctx: &FactContext,
    vcode: &mut VCode<Inst>,
    dst: &PairedGpr,
    src: &asm::GprMem<Gpr, Gpr>,
    mem: &MemCheck,
    cond: Option<((Fact, Fact), InequalityKind)>,
) -> PccResult<()> {
    // The taken value is a register, or a memory operand: a checked load's
    // fact or a constant-pool value.
    let taken_src = match src_of(src) {
        Src::Reg(src) => Ok(src),
        Src::Mem => {
            Err(mem_src_fact(vcode, src, mem, 64).unwrap_or_else(|| Fact::max_range_for_width(64)))
        }
    };
    let out = paired_out(dst);
    let alt = paired_in(dst);
    let ins: smallvec::SmallVec<[Reg; 2]> = match taken_src {
        Ok(src) => smallvec::smallvec![src, alt],
        Err(_) => smallvec::smallvec![alt],
    };
    check_output(ctx, vcode, out, &ins, |vcode| {
        let taken = match &taken_src {
            Ok(src) => get_select_arm_fact(vcode, *src, 64),
            Err(fact) => fact.clone(),
        };
        let not_taken = get_select_arm_fact(vcode, alt, 64);
        let (taken, not_taken) = match &cond {
            // See the aarch64 `CSel` case: the true side may widen with the
            // inequality, the false side with its negation.
            Some(((cmp_lhs, cmp_rhs), true_kind)) => {
                let false_kind = match true_kind {
                    InequalityKind::Loose => InequalityKind::Strict,
                    InequalityKind::Strict => InequalityKind::Loose,
                };
                (
                    ctx.apply_inequality(&taken, cmp_lhs, cmp_rhs, *true_kind),
                    ctx.apply_inequality(&not_taken, cmp_rhs, cmp_lhs, false_kind),
                )
            }
            None => (taken, not_taken),
        };
        clamp_range(ctx, 64, 64, ctx.union(&taken, &not_taken))
    })
}

/// Any other instruction: a defined register may only carry the fact that a
/// 32-bit write gives it (upper 32 bits zero).
fn check_generic(
    ctx: &FactContext,
    vcode: &mut VCode<Inst>,
    inst_idx: InsnIndex,
    inst: &AsmInst,
) -> PccResult<()> {
    let defs = vcode.inst_defs_with_facts(inst_idx);
    if defs.is_empty() {
        return Ok(());
    }
    // A few 32-bit instructions may leave a destination register unwritten
    // (and so its upper half not zeroed): a successful `cmpxchg` keeps `rax`,
    // and `bsf`/`bsr` of zero leave the destination unchanged.
    let text = alloc::format!("{inst}");
    let partial_write = ["cmpxchg", "lock", "xchg", "xadd", "bsf", "bsr"]
        .iter()
        .any(|p| text.starts_with(p));
    if partial_write || inst.pcc_gpr_write_bits() != Some(32) {
        trace!("unsupported fact on def of {inst:?}");
        return Err(PccError::UnsupportedFact);
    }
    let derived = Fact::max_range_for_width_extended(32, 64);
    for def in defs {
        let fact = vcode.vreg_fact(def).cloned();
        check_subsumes_optionals(ctx, Some(&derived), fact.as_ref())?;
        // A register with a symbolic fact keeps the numeric one too, for the
        // checker's numeric retry (see `check_output`).
        if fact.as_ref().is_some_and(|f| f.is_symbolic()) {
            vcode.set_def_shadow(def, derived.clone());
        }
    }
    Ok(())
}
