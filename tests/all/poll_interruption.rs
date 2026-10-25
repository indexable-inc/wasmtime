//! Poll interruption (ix fork): `Config::poll_interruption`.
#![cfg(all(unix, not(miri)))]

use std::time::{Duration, Instant};
use wasmtime::*;

fn engine(f: impl FnOnce(&mut Config)) -> Engine {
    let mut c = Config::new();
    c.poll_interruption(true);
    f(&mut c);
    Engine::new(&c).unwrap()
}

fn trap(e: &Error) -> Trap {
    *e.downcast_ref::<Trap>()
        .unwrap_or_else(|| panic!("not a trap: {e:?}"))
}

/// Interrupt `h` after `after` and return how long the call took to stop
/// once the interrupt had been issued.
fn interrupt_after(h: InterruptHandle, after: Duration) -> std::thread::JoinHandle<Instant> {
    std::thread::spawn(move || {
        std::thread::sleep(after);
        let t = Instant::now();
        h.interrupt();
        t
    })
}

const SPIN: &str = r#"(module (func (export "run") (loop br 0)))"#;

// f(n) makes 2^n calls with depth n and contains no loop, so only the
// function-entry polls can stop it.
const RECURSE: &str = r#"
(module
  (func $f (export "run") (param i32) (result i32)
    (if (result i32) (i32.eqz (local.get 0))
      (then (i32.const 1))
      (else
        (i32.add
          (call $f (i32.sub (local.get 0) (i32.const 1)))
          (call $f (i32.sub (local.get 0) (i32.const 1))))))))
"#;

#[test]
fn infinite_loop_is_interrupted_promptly() -> Result<()> {
    let engine = engine(|_| {});
    let module = Module::new(&engine, SPIN)?;
    let mut store = Store::new(&engine, ());
    let run = Instance::new(&mut store, &module, &[])?.get_typed_func::<(), ()>(&mut store, "run")?;
    let t = interrupt_after(store.interrupt_handle().unwrap(), Duration::from_millis(50));
    let err = run.call(&mut store, ()).unwrap_err();
    let stopped = t.join().unwrap().elapsed();
    assert_eq!(trap(&err), Trap::Interrupt);
    assert!(stopped < Duration::from_millis(100), "took {stopped:?} to stop");
    Ok(())
}

#[test]
fn recursion_only_is_interrupted_promptly() -> Result<()> {
    let engine = engine(|_| {});
    let module = Module::new(&engine, RECURSE)?;
    let mut store = Store::new(&engine, ());
    let run = Instance::new(&mut store, &module, &[])?.get_typed_func::<i32, i32>(&mut store, "run")?;
    // Sanity: it computes when not interrupted.
    assert_eq!(run.call(&mut store, 10)?, 1024);
    let t = interrupt_after(store.interrupt_handle().unwrap(), Duration::from_millis(50));
    let err = run.call(&mut store, 62).unwrap_err();
    let stopped = t.join().unwrap().elapsed();
    assert_eq!(trap(&err), Trap::Interrupt);
    assert!(stopped < Duration::from_millis(100), "took {stopped:?} to stop");
    Ok(())
}

#[test]
fn out_of_bounds_is_still_a_memory_trap() -> Result<()> {
    let engine = engine(|_| {});
    let module = Module::new(
        &engine,
        r#"(module (memory 1)
             (func (export "run") (param i32) (result i32) (i32.load (local.get 0))))"#,
    )?;
    let mut store = Store::new(&engine, ());
    let run = Instance::new(&mut store, &module, &[])?.get_typed_func::<i32, i32>(&mut store, "run")?;
    assert_eq!(run.call(&mut store, 0)?, 0);
    let err = run.call(&mut store, 1 << 20).unwrap_err();
    assert_eq!(trap(&err), Trap::MemoryOutOfBounds);
    // And once interrupted, the poll at entry wins and says so.
    let h = store.interrupt_handle().unwrap();
    h.interrupt();
    assert_eq!(trap(&run.call(&mut store, 1 << 20).unwrap_err()), Trap::Interrupt);
    assert!(h.rearm());
    assert_eq!(trap(&run.call(&mut store, 1 << 20).unwrap_err()), Trap::MemoryOutOfBounds);
    Ok(())
}

#[test]
fn interrupt_before_call_traps_at_entry_and_rearm_resumes() -> Result<()> {
    let engine = engine(|_| {});
    let module = Module::new(&engine, r#"(module (func (export "run") (result i32) i32.const 7))"#)?;
    let mut store = Store::new(&engine, ());
    let run = Instance::new(&mut store, &module, &[])?.get_typed_func::<(), i32>(&mut store, "run")?;
    let h = store.interrupt_handle().unwrap();
    assert!(!h.rearm());
    h.interrupt();
    h.interrupt();
    assert!(h.is_interrupted());
    assert_eq!(trap(&run.call(&mut store, ()).unwrap_err()), Trap::Interrupt);
    assert!(h.rearm());
    assert!(!h.rearm());
    assert_eq!(run.call(&mut store, ())?, 7);
    Ok(())
}

#[test]
fn interrupt_from_a_host_call_stops_the_caller() -> Result<()> {
    let engine = engine(|_| {});
    let module = Module::new(
        &engine,
        r#"(module (import "" "f" (func $f))
             (func (export "run") (call $f) (loop br 0)))"#,
    )?;
    let mut store = Store::new(&engine, ());
    let h = store.interrupt_handle().unwrap();
    let f = Func::wrap(&mut store, move || h.interrupt());
    let run = Instance::new(&mut store, &module, &[f.into()])?.get_typed_func::<(), ()>(&mut store, "run")?;
    assert_eq!(trap(&run.call(&mut store, ()).unwrap_err()), Trap::Interrupt);
    Ok(())
}

#[test]
fn stores_are_independent_across_threads() -> Result<()> {
    let engine = engine(|_| {});
    let module = Module::new(&engine, SPIN)?;
    let spawn = |engine: Engine, module: Module| {
        let (tx, rx) = std::sync::mpsc::channel();
        let j = std::thread::spawn(move || {
            let mut store = Store::new(&engine, ());
            let run = Instance::new(&mut store, &module, &[])
                .unwrap()
                .get_typed_func::<(), ()>(&mut store, "run")
                .unwrap();
            tx.send(store.interrupt_handle().unwrap()).unwrap();
            let r = run.call(&mut store, ());
            (trap(&r.unwrap_err()), Instant::now())
        });
        (rx.recv().unwrap(), j)
    };
    let (ha, a) = spawn(engine.clone(), module.clone());
    let (hb, b) = spawn(engine.clone(), module.clone());
    std::thread::sleep(Duration::from_millis(20));
    ha.interrupt();
    let (ta, at) = a.join().unwrap();
    assert_eq!(ta, Trap::Interrupt);
    // B keeps running after A stopped.
    std::thread::sleep(Duration::from_millis(50));
    assert!(!b.is_finished());
    hb.interrupt();
    let (tb, bt) = b.join().unwrap();
    assert_eq!(tb, Trap::Interrupt);
    assert!(bt > at + Duration::from_millis(40));
    Ok(())
}

#[test]
fn pooling_slot_reuse_polls_the_new_store() -> Result<()> {
    let mut pool = PoolingAllocationConfig::default();
    pool.total_core_instances(1);
    pool.total_memories(1);
    pool.total_tables(1);
    let engine = engine(|c| {
        c.allocation_strategy(InstanceAllocationStrategy::Pooling(pool));
    });
    let module = Module::new(
        &engine,
        r#"(module (memory 1)
             (func (export "run") (param i32) (result i32)
               (local i32)
               (loop
                 (local.set 1 (i32.add (local.get 1) (i32.const 1)))
                 (br_if 0 (i32.lt_u (local.get 1) (local.get 0))))
               (local.get 1)))"#,
    )?;
    // Store A: interrupted, and its handle outlives it.
    let old = {
        let mut a = Store::new(&engine, ());
        let run = Instance::new(&mut a, &module, &[])?.get_typed_func::<i32, i32>(&mut a, "run")?;
        let h = a.interrupt_handle().unwrap();
        h.interrupt();
        assert_eq!(trap(&run.call(&mut a, 10).unwrap_err()), Trap::Interrupt);
        h
    };
    // Store B reuses the only slot and runs, though A's word is still set.
    let mut b = Store::new(&engine, ());
    let run = Instance::new(&mut b, &module, &[])?.get_typed_func::<i32, i32>(&mut b, "run")?;
    assert!(old.is_interrupted());
    assert_eq!(run.call(&mut b, 1000)?, 1000);
    // Interrupting the dead store's handle does nothing to B.
    old.interrupt();
    assert_eq!(run.call(&mut b, 1000)?, 1000);
    b.interrupt_handle().unwrap().interrupt();
    assert_eq!(trap(&run.call(&mut b, 1000).unwrap_err()), Trap::Interrupt);
    Ok(())
}

#[test]
fn config_and_artifacts() -> Result<()> {
    let mut c = Config::new();
    c.poll_interruption(true).epoch_interruption(true);
    assert!(Engine::new(&c).is_err());
    let mut c = Config::new();
    c.poll_interruption(true).signals_based_traps(false);
    assert!(Engine::new(&c).is_err());
    // A module compiled with polls doesn't load into an engine without them.
    let poll = engine(|_| {});
    let bytes = poll.precompile_module(SPIN.as_bytes())?;
    let plain = Engine::default();
    assert!(unsafe { Module::deserialize(&plain, &bytes) }.is_err());
    // No handle without the feature.
    assert!(Store::new(&plain, ()).interrupt_handle().is_none());
    Ok(())
}

#[test]
fn only_x86_64_and_aarch64_targets() -> Result<()> {
    // riscv64 and s390x lower `atomic_load` without its trap code, so a poll
    // fault there would not be recognised as a trap.
    for target in ["riscv64gc-unknown-linux-gnu", "s390x-unknown-linux-gnu"] {
        let mut c = Config::new();
        c.poll_interruption(true).target(target)?;
        let err = Engine::new(&c).map(|_| ()).unwrap_err().to_string();
        assert!(err.contains("only supported on x86_64 and aarch64"), "{target}: {err}");
    }
    for target in ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"] {
        let mut c = Config::new();
        c.poll_interruption(true).target(target)?;
        Engine::new(&c)?;
    }
    Ok(())
}

#[test]
fn many_interrupts_and_rearms() -> Result<()> {
    // Interrupt and re-arm one store over and over, and run many short-lived
    // stores: every interrupt stops exactly the next call, every re-arm lets
    // the following call run, and nothing leaks or changes state for others.
    let engine = engine(|_| {});
    let spin = Module::new(&engine, SPIN)?;
    let seven = Module::new(&engine, r#"(module (func (export "run") (result i32) i32.const 7))"#)?;
    let mut store = Store::new(&engine, ());
    let run = Instance::new(&mut store, &seven, &[])?.get_typed_func::<(), i32>(&mut store, "run")?;
    let h = store.interrupt_handle().unwrap();
    for _ in 0..10_000 {
        h.interrupt();
        assert_eq!(trap(&run.call(&mut store, ()).unwrap_err()), Trap::Interrupt);
        assert!(h.rearm());
        assert_eq!(run.call(&mut store, ())?, 7);
    }
    for _ in 0..2000 {
        let mut s = Store::new(&engine, ());
        let run = Instance::new(&mut s, &spin, &[])?.get_typed_func::<(), ()>(&mut s, "run")?;
        s.interrupt_handle().unwrap().interrupt();
        assert_eq!(trap(&run.call(&mut s, ()).unwrap_err()), Trap::Interrupt);
    }
    Ok(())
}

#[test]
fn interrupt_is_seen_by_a_running_loop_quickly() -> Result<()> {
    // The interrupt is one store on another thread; the spinning guest has
    // no fence. Measure from the store to the trap, many times.
    let engine = engine(|_| {});
    let module = Module::new(&engine, SPIN)?;
    let mut worst = Duration::ZERO;
    for _ in 0..200 {
        let mut store = Store::new(&engine, ());
        let run = Instance::new(&mut store, &module, &[])?.get_typed_func::<(), ()>(&mut store, "run")?;
        let t = interrupt_after(store.interrupt_handle().unwrap(), Duration::from_micros(200));
        assert_eq!(trap(&run.call(&mut store, ()).unwrap_err()), Trap::Interrupt);
        worst = worst.max(t.join().unwrap().elapsed());
    }
    // Includes thread wake-up and unwinding; a missed store would hang.
    assert!(worst < Duration::from_millis(50), "worst {worst:?}");
    Ok(())
}

#[test]
fn start_function_is_interruptible() -> Result<()> {
    // A `start` that never returns: instantiation must be preemptible too.
    let engine = engine(|_| {});
    let module = Module::new(&engine, r#"(module (func $s (loop br 0)) (start $s))"#)?;
    let mut store = Store::new(&engine, ());
    let t = interrupt_after(store.interrupt_handle().unwrap(), Duration::from_millis(20));
    let err = Instance::new(&mut store, &module, &[]).map(|_| ()).unwrap_err();
    t.join().unwrap();
    assert_eq!(trap(&err), Trap::Interrupt);
    Ok(())
}

#[test]
fn artifact_from_older_fork_abi_is_rejected() -> Result<()> {
    // ix3 changed what a poll does (test a word, not fault on a page), so an
    // `+ix2` artifact would run but never be interrupted: it, like any older
    // one, must be refused before any of its code runs. The version string
    // is `<major>+ix<N>.<abi tag>`; rewriting `N` alone must be refused.
    let engine = engine(|_| {});
    // The version suffix and `IX_INTRINSICS_ABI` come from one constant.
    let cur = format!("+ix{IX_INTRINSICS_ABI}");
    let old = format!("+ix{}", IX_INTRINSICS_ABI - 1);
    assert_eq!(cur.len(), old.len());
    let mut bytes = engine.precompile_module(SPIN.as_bytes())?;
    let at = bytes
        .windows(cur.len())
        .position(|w| w == cur.as_bytes())
        .unwrap_or_else(|| panic!("the artifact carries the {cur} version"));
    bytes[at..at + cur.len()].copy_from_slice(old.as_bytes());
    let err = unsafe { Module::deserialize(&engine, &bytes) }.map(|_| ()).unwrap_err();
    let msg = format!("{err:?}");
    assert!(msg.contains(&old) && msg.contains("incompatible"), "{msg}");
    Ok(())
}

#[test]
fn custom_version_strategy_still_checks_fork_abi() -> Result<()> {
    // A custom (or no) version string drops the `+ixN` suffix; the fork ABI
    // is still recorded in, and checked from, the artifact metadata. Same
    // ABI round-trips; the mismatch case is the `test_ix_abi_mismatch` unit
    // test in engine/serialization.rs.
    let engine = engine(|c| {
        c.module_version(ModuleVersionStrategy::Custom("custom".into()))
            .unwrap();
    });
    let bytes = engine.precompile_module(SPIN.as_bytes())?;
    unsafe { Module::deserialize(&engine, &bytes) }?;
    Ok(())
}
