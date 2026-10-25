//! Common helpers for ISA-specific proof-carrying-code implementations.

use crate::ir::pcc::{Fact, FactContext, PccError, PccResult};
use crate::machinst::{Reg, VCode, VCodeInst, Writable};
use crate::trace;

pub(crate) fn get_fact_or_default<I: VCodeInst>(vcode: &VCode<I>, reg: Reg, width: u16) -> Fact {
    trace!(
        "get_fact_or_default: reg {reg:?} -> {:?}",
        vcode.vreg_fact(reg.into())
    );
    match vcode.vreg_fact(reg.into()) {
        // In the checker's numeric retry, a register with a symbolic fact
        // holds the numeric fact derived at its def (see `check_output`).
        Some(fact) if vcode.pcc_numeric && fact.is_symbolic() => {
            match vcode.def_shadow(reg.into()) {
                Some(shadow) => shadow.clone(),
                None if matches!(fact, Fact::Def { .. }) => Fact::max_range_for_width(width),
                None => fact.clone(),
            }
        }
        // A register that defines a symbol holds exactly that symbol.
        Some(Fact::Def { value }) => Fact::value(width, *value),
        Some(fact) => fact.clone(),
        None => Fact::max_range_for_width(width),
    }
}

/// The fact of a select's (cmov's, csel's) arm. Like `get_fact_or_default`,
/// except that a register naming a symbol whose def proved it an exact
/// constant reads as that constant. The case is the null arm of a Spectre
/// guard: one GVN'd zero register can also be a constant-zero Wasm index
/// and so carry that index's `Def`, and `union` needs the zero to join it
/// with the pointer arm. Both facts hold for the register, so either may be
/// used.
pub(crate) fn get_select_arm_fact<I: VCodeInst>(vcode: &VCode<I>, reg: Reg, width: u16) -> Fact {
    if let Some(Fact::Def { .. }) = vcode.vreg_fact(reg.into()) {
        if let Some(c @ Fact::Range { min, max, .. }) = vcode.def_shadow(reg.into()) {
            if min == max {
                return c.clone();
            }
        }
    }
    get_fact_or_default(vcode, reg, width)
}

pub(crate) fn has_fact<I: VCodeInst>(vcode: &VCode<I>, reg: Reg) -> bool {
    vcode.vreg_fact(reg.into()).is_some()
}

pub(crate) fn fail_if_missing(fact: Option<Fact>) -> PccResult<Fact> {
    fact.ok_or(PccError::UnsupportedFact)
}

pub(crate) fn clamp_range(
    ctx: &FactContext,
    to_bits: u16,
    from_bits: u16,
    fact: Option<Fact>,
) -> PccResult<Option<Fact>> {
    let max = if from_bits > 64 {
        return Ok(None);
    } else if from_bits == 64 {
        u64::MAX
    } else {
        (1u64 << from_bits) - 1
    };
    trace!(
        "clamp_range: fact {:?} from {} to {}",
        fact, from_bits, to_bits
    );
    Ok(fact
        .and_then(|f| ctx.uextend(&f, from_bits, to_bits))
        .or_else(|| {
            let result = Fact::Range {
                bit_width: to_bits,
                min: 0,
                max,
            };
            trace!(" -> clamping to {:?}", result);
            Some(result)
        }))
}

pub(crate) fn check_subsumes(ctx: &FactContext, subsumer: &Fact, subsumee: &Fact) -> PccResult<()> {
    check_subsumes_optionals(ctx, Some(subsumer), Some(subsumee))
}

pub(crate) fn check_subsumes_optionals(
    ctx: &FactContext,
    subsumer: Option<&Fact>,
    subsumee: Option<&Fact>,
) -> PccResult<()> {
    trace!(
        "checking if derived fact {:?} subsumes stated fact {:?}",
        subsumer, subsumee
    );

    if ctx.subsumes_fact_optionals(subsumer, subsumee) {
        Ok(())
    } else {
        log::debug!("pcc: derived fact {subsumer:?} does not subsume stated {subsumee:?}");
        Err(PccError::UnsupportedFact)
    }
}

pub(crate) fn check_output<I: VCodeInst, F: Fn(&VCode<I>) -> PccResult<Option<Fact>>>(
    ctx: &FactContext,
    vcode: &mut VCode<I>,
    out: Writable<Reg>,
    ins: &[Reg],
    f: F,
) -> PccResult<()> {
    if let Some(fact) = vcode.vreg_fact(out.to_reg().into()) {
        let symbolic = fact.is_symbolic();
        let is_def = matches!(fact, Fact::Def { .. });
        let result = f(vcode)?;
        // Anything subsumes a `Def`: it only names the value.
        let checked = if is_def {
            Ok(())
        } else {
            check_subsumes_optionals(ctx, result.as_ref(), vcode.vreg_fact(out.to_reg().into()))
        };
        // A register with a symbolic fact also gets the numeric fact derived
        // from its inputs' numeric facts, for reads in the checker's numeric
        // retry (`check_vcode_facts`). Both facts are verified derivations,
        // so either may justify a use.
        if symbolic && checked.is_ok() {
            let numeric = if vcode.pcc_numeric {
                result
            } else {
                vcode.pcc_numeric = true;
                let r = f(vcode);
                vcode.pcc_numeric = false;
                r.ok().flatten()
            };
            if let Some(r) = numeric.filter(|r| !r.is_symbolic() && *r != Fact::Conflict) {
                vcode.set_def_shadow(out.to_reg().into(), r);
            }
        }
        checked
    } else if ins.iter().any(|r| {
        vcode
            .vreg_fact(r.into())
            .map(|fact| fact.propagates())
            .unwrap_or(false)
    }) {
        if let Ok(Some(fact)) = f(vcode) {
            trace!("setting vreg {:?} to {:?}", out, fact);
            vcode.set_vreg_fact(out.to_reg().into(), fact);
        }
        Ok(())
    } else {
        Ok(())
    }
}

pub(crate) fn check_unop<I: VCodeInst, F: Fn(&Fact) -> PccResult<Option<Fact>>>(
    ctx: &FactContext,
    vcode: &mut VCode<I>,
    reg_width: u16,
    out: Writable<Reg>,
    ra: Reg,
    f: F,
) -> PccResult<()> {
    check_output(ctx, vcode, out, &[ra], |vcode| {
        let ra = get_fact_or_default(vcode, ra, reg_width);
        f(&ra)
    })
}

pub(crate) fn check_binop<I: VCodeInst, F: Fn(&Fact, &Fact) -> PccResult<Option<Fact>>>(
    ctx: &FactContext,
    vcode: &mut VCode<I>,
    reg_width: u16,
    out: Writable<Reg>,
    ra: Reg,
    rb: Reg,
    f: F,
) -> PccResult<()> {
    check_output(ctx, vcode, out, &[ra, rb], |vcode| {
        let ra = get_fact_or_default(vcode, ra, reg_width);
        let rb = get_fact_or_default(vcode, rb, reg_width);
        f(&ra, &rb)
    })
}

pub(crate) fn check_constant<I: VCodeInst>(
    ctx: &FactContext,
    vcode: &mut VCode<I>,
    out: Writable<Reg>,
    bit_width: u16,
    value: u64,
) -> PccResult<()> {
    let result = Fact::constant(bit_width, value);
    if let Some(fact) = vcode.vreg_fact(out.to_reg().into()) {
        let symbolic = fact.is_symbolic();
        check_subsumes(ctx, &result, fact)?;
        // As in `check_output`: a register with a symbolic fact also keeps
        // the numeric one its def proves.
        if symbolic {
            vcode.set_def_shadow(out.to_reg().into(), result);
        }
        Ok(())
    } else {
        trace!("setting vreg {:?} to {:?}", out, result);
        vcode.set_vreg_fact(out.to_reg().into(), result);
        Ok(())
    }
}

/// The operation we're checking against an amode: either
///
/// - a *load*, and we need to validate that the field's fact subsumes
///   the load result's fact, OR
///
/// - a *store*, and we need to validate that the stored data's fact
///   subsumes the field's fact.
pub(crate) enum LoadOrStore<'a> {
    Load {
        result_fact: Option<&'a Fact>,
        from_bits: u16,
        to_bits: u16,
    },
    Store {
        stored_fact: Option<&'a Fact>,
    },
}
