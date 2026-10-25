//! Compilation backend pipeline: optimized IR to VCode / binemit.

use crate::CodegenError;
use crate::dominator_tree::DominatorTree;
use crate::ir::Function;
use crate::ir::pcc;
use crate::isa::TargetIsa;
use crate::machinst::*;
use crate::settings::{RegallocAlgorithm, RegallocBudget};
use crate::timing;
use crate::trace;

use regalloc2::{Algorithm, Function as _, RegAllocError, RegallocOptions};

/// Work budget of the backtracking allocator per unit of function size (an
/// instruction, block or virtual register), see the `regalloc_budget`
/// setting and `RegallocOptions::fuel`.
///
/// Calibration (2026-10-25, x86-64 and aarch64, opt_level speed and none):
/// over the 49 474 functions of four real modules (Turso 13 MB, Kusama
/// runtime 8.8 MB, SpiderMonkey 7.4 MB, a Go 1.26 wasip1 build) the most
/// fuel any function used was 55 per unit (p99 22, median 3), and the most
/// liveness words (set words plus `ion::RANGE_WORDS` per live range) 7.1
/// per unit against the cap of 16 (`ion::FUEL_PER_LIVE_WORD`). Binaryen
/// asyncify output is the exception: in SpiderMonkey and Kusama asyncified,
/// 4 of 24 899 functions need up to 953 fuel and 115 words per unit (651
/// and 85 at opt_level none), so under `reject` those modules fail and need
/// `fallback`. At the cap, the whole compile peaks at about 600 bytes of RSS
/// per unit on the hostile cases in wasm-host
/// bench/results/2026-10-25-compile-dos (100k `br_if`s to one label with
/// 64k live values: 1.48M units, rejected in 2.5 s at 841 MB; base: over
/// 150 s and 13.6 GB).
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
    let mut vcode = {
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

    // Perform validation of proof-carrying-code facts, if requested.
    if b.flags().enable_pcc() {
        pcc::check_vcode_facts(f, &mut vcode, b).map_err(CodegenError::Pcc)?;
    }

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
                "regalloc budget: used {} of {} units, live words peak {}, ranges {} ({} insts, {} blocks, {} vregs)",
                regalloc_ctx.output.stats.fuel_used,
                fuel,
                regalloc_ctx.output.stats.live_words_peak,
                regalloc_ctx.output.stats.final_liverange_count,
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
