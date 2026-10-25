//! Fuzz the `ion` register allocator.

use crate::{checker, fuzzing::func, ion, RegAllocError};
use arbitrary::{Arbitrary, Result, Unstructured};
use core::cell::RefCell;
use std::thread_local;

/// `ion`-specific options for generating functions.
const OPTIONS: func::Options = func::Options {
    reused_inputs: true,
    fixed_regs: true,
    fixed_nonallocatable: true,
    clobbers: true,
    reftypes: true,
    callsite_ish_constraints: true,
    ..func::Options::DEFAULT
};

/// A convenience wrapper to generate a [`func::Func`] with `ion`-specific
/// options enabled.
#[derive(Clone, Debug)]
pub struct TestCase {
    func: func::Func,
    annotate: bool,
    check_ssa: bool,
    /// A work budget (`RegallocOptions::fuel`) for a first run, which may
    /// run out; the function is then allocated again with no budget in the
    /// same context, which must not see anything the aborted run left.
    fuel: Option<u64>,
}

impl Arbitrary<'_> for TestCase {
    fn arbitrary(u: &mut Unstructured) -> Result<TestCase> {
        let func = func::Func::arbitrary_with_options(u, &OPTIONS)?;
        let annotate = bool::arbitrary(u)?;
        let check_ssa = bool::arbitrary(u)?;
        let fuel = if bool::arbitrary(u)? {
            Some(u.int_in_range(0..=200_000)?)
        } else {
            None
        };
        Ok(TestCase {
            func,
            annotate,
            check_ssa,
            fuel,
        })
    }
}

/// Test a single function with the `ion` allocator.
///
/// This also:
/// - optionally creates annotations
/// - optionally verifies the incoming SSA
/// - runs the [`checker`].
pub fn check(t: TestCase) {
    let TestCase {
        func,
        annotate,
        check_ssa,
        fuel,
    } = &t;
    log::trace!("func:\n{func:?}");

    let env = func::machine_env();
    thread_local! {
        // We test that ctx is cleared properly between runs.
        static CTX: RefCell<ion::Ctx> = RefCell::default();
    }

    let run_and_check = |fuel: Option<u64>| {
        CTX.with(|ctx| {
            match ion::run(
                func,
                &env,
                &mut *ctx.borrow_mut(),
                *annotate,
                *check_ssa,
                fuel,
            ) {
                Ok(()) => {}
                Err(RegAllocError::OutOfFuel) if fuel.is_some() => return,
                Err(e) => panic!("regalloc did not succeed: {e:?}"),
            }
            let mut checker = checker::Checker::new(func, &env);
            checker.prepare(&ctx.borrow().output);
            checker.run().expect("checker failed");
        })
    };
    if fuel.is_some() {
        run_and_check(*fuel);
    }
    run_and_check(None);
}

#[test]
fn smoke() {
    arbtest::arbtest(|u| {
        let test_case = TestCase::arbitrary(u)?;
        check(test_case);
        Ok(())
    })
    .budget_ms(1_000);
}

/// A long run of [`check`] for the ix-cranelift-opt changes: `RA_FUZZ_MS`
/// milliseconds (default 60 s) of random functions through ion and the
/// checker. `cargo test --release -p regalloc2 --features fuzzing -- --ignored long`
#[test]
#[ignore]
fn long() {
    let ms = std::env::var("RA_FUZZ_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(60_000);
    let cases = core::cell::Cell::new(0u64);
    arbtest::arbtest(|u| {
        let test_case = TestCase::arbitrary(u)?;
        check(test_case);
        cases.set(cases.get() + 1);
        Ok(())
    })
    .budget_ms(ms);
    std::println!("ion checker: {} functions", cases.get());
}

/// Tests of Ion's work budget (`RegallocOptions::fuel`, ix fork).
///
/// They pin exact fuel counts. Fuel is integer arithmetic over the
/// allocator's own data structures, with no clock, address or hash seed in
/// it, so the counts are the same on every run and every machine; a change
/// here means a change to where Ion spends fuel, and Cranelift's calibration
/// (`REGALLOC_FUEL_PER_UNIT` in cranelift-codegen `machinst/compile.rs`)
/// should be measured again.
mod budget {
    use super::func::{machine_env, Func};
    use crate::{checker, ion, Ctx, RegAllocError};
    use alloc::format;
    use alloc::string::String;

    fn run(f: &Func, ctx: &mut Ctx, fuel: Option<u64>) -> Result<(), RegAllocError> {
        ion::run(f, &machine_env(), ctx, false, true, fuel)
    }

    /// Everything a run produced, as text: the stats (which include the fuel
    /// used, and for an aborted run show how far it got) and the output.
    fn outcome(ctx: &Ctx, r: Result<(), RegAllocError>) -> String {
        let o = &ctx.output;
        match r {
            Ok(()) => format!(
                "ok {:?} {} {:?} {:?} {:?}",
                o.stats, o.num_spillslots, o.edits, o.allocs, o.inst_alloc_offsets
            ),
            Err(e) => format!("{e:?} {:?}", o.stats),
        }
    }

    fn check(f: &Func, ctx: &Ctx) {
        let env = machine_env();
        let mut checker = checker::Checker::new(f, &env);
        checker.prepare(&ctx.output);
        checker.run().expect("checker failed");
    }

    /// The least fuel with which `f` allocates, given what an unbounded run
    /// used and its liveness-words peak (the cap is fuel / 16 words, so it
    /// can bind before the fuel does). Success is monotone in the budget
    /// (what Ion spends does not depend on what is left), so this bisects.
    fn least_fuel(f: &Func, ctx: &mut Ctx, used: u64, peak: u64) -> u64 {
        let (mut lo, mut hi) = (0, (used + 1).max(peak * 16));
        assert!(run(f, ctx, Some(hi)).is_ok());
        while hi - lo > 1 {
            let mid = lo + (hi - lo) / 2;
            match run(f, ctx, Some(mid)) {
                Ok(()) => hi = mid,
                Err(RegAllocError::OutOfFuel) => lo = mid,
                Err(e) => panic!("{e:?}"),
            }
        }
        hi
    }

    /// (diamonds, stride, fuel used with no budget, liveness words peak,
    /// least fuel that succeeds).
    const GOLDEN: &[(usize, usize, u64, u64, u64)] = &[
        // Fuel binds: the least budget is one more than an unbounded run used.
        (64, 1, 29234, 386, 29235),
        (512, 1, 955332, 13836, 955333),
        // The liveness-word cap binds: the least budget is 16 x the peak.
        (256, 64, 705845, 197636, 3162176),
    ];

    /// The same function always uses the same fuel, in a fresh or a reused
    /// context, and gives the same allocation; the counts are pinned.
    #[test]
    fn fuel_is_deterministic() {
        let mut reused = Ctx::default();
        let mut got = alloc::vec::Vec::new();
        for &(n, stride, ..) in GOLDEN {
            let f = Func::diamonds(n, stride);
            let mut fresh = Ctx::default();
            let r = run(&f, &mut fresh, None);
            let first = outcome(&fresh, r);
            assert!(first.starts_with("ok "), "{first}");
            check(&f, &fresh);
            for _ in 0..2 {
                let r = run(&f, &mut reused, None);
                assert_eq!(outcome(&reused, r), first);
            }
            let s = fresh.output.stats;
            got.push((
                n,
                stride,
                s.fuel_used,
                s.live_words_peak,
                least_fuel(&f, &mut reused, s.fuel_used, s.live_words_peak),
            ));
        }
        assert_eq!(&got[..], GOLDEN);
    }

    /// Short of fuel, Ion stops at the same point every time (same stats),
    /// whatever ran in the context before, and the least fuel that succeeds
    /// is pinned in [`GOLDEN`]. An aborted run leaves the reused context
    /// clean: the next run in it matches a fresh context and passes the
    /// checker.
    #[test]
    fn out_of_fuel_is_repeatable_and_clean() {
        let other = Func::diamonds(40, 3);
        let mut fresh_other = Ctx::default();
        let r = run(&other, &mut fresh_other, None);
        let other_ok = outcome(&fresh_other, r);

        for &(n, stride, _, _, least) in GOLDEN {
            let f = Func::diamonds(n, stride);
            // Below the least budget, at several depths into the run.
            for fuel in [least - 1, least / 2, least / 16] {
                let mut fresh = Ctx::default();
                let r = run(&f, &mut fresh, Some(fuel));
                assert!(matches!(r, Err(RegAllocError::OutOfFuel)), "{r:?}");
                let stopped = outcome(&fresh, r);

                let mut reused = Ctx::default();
                // After a success, after an abort, and after the same abort.
                for warm in [Some(&other), None, None] {
                    if let Some(w) = warm {
                        run(w, &mut reused, None).unwrap();
                    }
                    let r = run(&f, &mut reused, Some(fuel));
                    assert_eq!(outcome(&reused, r), stopped, "fuel {fuel}");
                }
                // The context the abort left allocates another function as a
                // fresh one does.
                let r = run(&other, &mut reused, None);
                assert_eq!(outcome(&reused, r), other_ok);
                check(&other, &reused);
            }
            let mut ctx = Ctx::default();
            assert!(run(&f, &mut ctx, Some(least)).is_ok());
            check(&f, &ctx);
        }
    }

    /// The liveness-word cap (fuel / 16 words over all livein and liveout
    /// sets) stops a function whose liveness sets would grow past it even
    /// when the fuel itself would last. Stride 64 gives every long-lived
    /// value a word of its own, so the sets are large for the fuel spent.
    #[test]
    fn live_word_cap_binds_before_fuel() {
        let f = Func::diamonds(256, 64);
        let mut ctx = Ctx::default();
        run(&f, &mut ctx, None).unwrap();
        let (used, peak) = (ctx.output.stats.fuel_used, ctx.output.stats.live_words_peak);
        let fuel = peak * 16 - 1;
        assert!(
            fuel > used,
            "the cap should bind first: used {used}, peak {peak}"
        );
        let r = run(&f, &mut ctx, Some(fuel));
        assert!(matches!(r, Err(RegAllocError::OutOfFuel)), "{r:?}");
        assert!(run(&f, &mut ctx, Some(peak * 16)).is_ok());
        check(&f, &ctx);
    }
}
