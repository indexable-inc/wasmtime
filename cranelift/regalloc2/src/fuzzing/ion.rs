//! Fuzz the `ion` register allocator.

use crate::{checker, fuzzing::func, ion};
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
}

impl Arbitrary<'_> for TestCase {
    fn arbitrary(u: &mut Unstructured) -> Result<TestCase> {
        let func = func::Func::arbitrary_with_options(u, &OPTIONS)?;
        let annotate = bool::arbitrary(u)?;
        let check_ssa = bool::arbitrary(u)?;
        Ok(TestCase {
            func,
            annotate,
            check_ssa,
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
    } = &t;
    log::trace!("func:\n{func:?}");

    let env = func::machine_env();
    thread_local! {
        // We test that ctx is cleared properly between runs.
        static CTX: RefCell<ion::Ctx> = RefCell::default();
    }

    CTX.with(|ctx| {
        ion::run(func, &env, &mut *ctx.borrow_mut(), *annotate, *check_ssa, false)
            .expect("regalloc did not succeed");

        let mut checker = checker::Checker::new(func, &env);
        checker.prepare(&ctx.borrow().output);
        checker.run().expect("checker failed");
    });
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
