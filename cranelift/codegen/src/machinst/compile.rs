//! Compilation backend pipeline: optimized IR to VCode / binemit.

use crate::dominator_tree::DominatorTree;
use crate::ir::Function;
use crate::isa::TargetIsa;
use crate::machinst::*;
use crate::settings::{RegallocAlgorithm, RegallocBudget};
use crate::timing;
use crate::trace;

use crate::result::CodegenError;
use regalloc2::{Algorithm, Function as _, RegAllocError, RegallocOptions};

/// Work budget of the backtracking allocator per unit of function size (an
/// instruction, block or virtual register), see the `regalloc_budget`
/// setting and `RegallocOptions::fuel`.
///
/// Calibration (x86-64, opt_level speed, 2026-10-25): over the 36 868
/// functions of three real modules (Turso 13 MB, Kusama runtime 8.8 MB,
/// SpiderMonkey 7.4 MB) the most fuel any function used was 43 per unit
/// (p99 18, median 2), and the most liveness-set words 4.0 per unit (p99
/// 1.1) against the cap of 16 (`ion::FUEL_PER_LIVE_WORD`). Small functions
/// sit under `REGALLOC_FUEL_MIN`; the largest used 10% of it. The hostile
/// cases in wasm-host bench/results/2026-10-25-compile-dos hit it within a
/// second for 16k if/else diamonds (base: 55 s, 4.3 GB).
pub const REGALLOC_FUEL_PER_UNIT: u64 = 256;

/// Budget floor, so small functions never hit it.
pub const REGALLOC_FUEL_MIN: u64 = 1 << 24;

/// Compile the given function down to VCode with allocated registers, ready
/// for binary emission.
pub fn compile<B: LowerBackend + TargetIsa>(
    f: &Function,
    domtree: &DominatorTree,
    regalloc_ctx: &mut regalloc2::Ctx,
    b: &B,
    abi: Callee<<<B as LowerBackend>::MInst as MachInst>::ABIMachineSpec>,
    emit_info: <B::MInst as MachInstEmit>::Info,
    sigs: SigSet,
    ctrl_plane: &mut ControlPlane,
) -> CodegenResult<VCode<B::MInst>> {
    // Compute lowered block order.
    let block_order = BlockLoweringOrder::new(f, domtree, ctrl_plane);

    // Build the lowering context.
    let lower =
        crate::machinst::Lower::new(f, abi, emit_info, block_order, sigs, b.flags().clone())?;

    // Lower the IR.
    let vcode = {
        log::debug!(
            "Number of CLIF instructions to lower: {}",
            f.dfg.num_insts()
        );
        log::debug!("Number of CLIF blocks to lower: {}", f.dfg.num_blocks());

        let _tt = timing::vcode_lower();
        lower.lower(b, ctrl_plane)?
    };

    log::debug!(
        "Number of lowered vcode instructions: {}",
        vcode.num_insts()
    );
    log::debug!("Number of lowered vcode blocks: {}", vcode.num_blocks());
    trace!("vcode from lowering: \n{:?}", vcode);

    // Perform register allocation.
    {
        let _tt = timing::regalloc();
        let mut options = RegallocOptions::default();
        options.verbose_log = b.flags().regalloc_verbose_logs();

        if cfg!(debug_assertions) {
            options.validate_ssa = true;
        }

        options.algorithm = match b.flags().regalloc_algorithm() {
            RegallocAlgorithm::Backtracking => Algorithm::Ion,
            RegallocAlgorithm::SinglePass => Algorithm::Fastalloc,
        };

        let budget = b.flags().regalloc_budget();
        if budget != RegallocBudget::Unbounded {
            let units = (vcode.num_insts() + vcode.num_blocks() + vcode.num_vregs()) as u64;
            options.fuel = Some(
                units
                    .saturating_mul(REGALLOC_FUEL_PER_UNIT)
                    .max(REGALLOC_FUEL_MIN),
            );
        }

        let mut result =
            regalloc2::run_with_ctx(&vcode, vcode.abi.machine_env(), &options, regalloc_ctx)
                .map(|_| ());
        if let Err(RegAllocError::OutOfFuel) = result {
            log::warn!(
                "register allocation of {} exceeded its budget ({} units for {} insts, {} blocks, {} vregs): {:?}",
                f.name,
                options.fuel.unwrap_or(0),
                vcode.num_insts(),
                vcode.num_blocks(),
                vcode.num_vregs(),
                budget,
            );
            match budget {
                RegallocBudget::Reject => return Err(CodegenError::ImplLimitExceeded),
                RegallocBudget::Fallback => {
                    options.algorithm = Algorithm::Fastalloc;
                    options.fuel = None;
                    result = regalloc2::run_with_ctx(
                        &vcode,
                        vcode.abi.machine_env(),
                        &options,
                        regalloc_ctx,
                    )
                    .map(|_| ());
                }
                RegallocBudget::Unbounded => unreachable!("no budget was set"),
            }
        } else if let Some(fuel) = options.fuel {
            log::debug!(
                "regalloc budget: used {} of {} units, live words peak {} ({} insts, {} blocks, {} vregs)",
                regalloc_ctx.output.stats.fuel_used,
                fuel,
                regalloc_ctx.output.stats.live_words_peak,
                vcode.num_insts(),
                vcode.num_blocks(),
                vcode.num_vregs(),
            );
        }

        result
            .map_err(|err| {
                log::error!(
                    "Register allocation error for vcode\n\
                     {vcode:?}\n\
                     Error: {err:?}\n\
                     CLIF for error:\n\
                     {f:?}",
                );
                err
            })
            .expect("register allocation");
    }

    // Run the regalloc checker, if requested.
    if b.flags().regalloc_checker() {
        let _tt = timing::regalloc_checker();
        let mut checker = regalloc2::checker::Checker::new(&vcode, &vcode.abi.machine_env());
        checker.prepare(&regalloc_ctx.output);
        checker
            .run()
            .map_err(|err| {
                log::error!("Register allocation checker errors:\n{err:?}\nfor vcode:\n{vcode:?}");
                err
            })
            .expect("register allocation checker");
    }

    Ok(vcode)
}
