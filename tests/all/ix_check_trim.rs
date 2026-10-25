//! ix fork: tests for `Config::ix_leaf_stack_slack` and
//! `Config::ix_table_devirt`.

#![cfg(not(miri))]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use wasmtime::*;

fn trap_of(e: Error) -> Trap {
    match e.downcast::<Trap>() {
        Ok(t) => t,
        Err(e) => panic!("expected a trap, got {e:?}"),
    }
}

// ------------------------------------------------------------ stack slack

/// Recursion `rec(n)` goes `n` levels deep and then calls `$leaf` (a function
/// with no calls and a few hundred bytes of spills, so it runs unchecked with
/// slack on). `$spin` is the same leaf with a loop, so it reaches the epoch
/// builtin from the slack region.
const STACK_WAT: &str = r#"
(module
  (memory 1)
  (func $leaf (param $p i32) (result i32)
    (local $a v128)
    ;; 24 independent v128 loads kept live across one long chain: forces
    ;; spills, so the frame is nonzero.
    (local.set $a
      (i32x4.add
        (i32x4.add
          (i32x4.add (i32x4.add (v128.load offset=0 (local.get $p)) (v128.load offset=16 (local.get $p)))
                     (i32x4.add (v128.load offset=32 (local.get $p)) (v128.load offset=48 (local.get $p))))
          (i32x4.add (i32x4.add (v128.load offset=64 (local.get $p)) (v128.load offset=80 (local.get $p)))
                     (i32x4.add (v128.load offset=96 (local.get $p)) (v128.load offset=112 (local.get $p)))))
        (i32x4.mul
          (i32x4.mul
            (i32x4.mul (v128.load offset=128 (local.get $p)) (v128.load offset=144 (local.get $p)))
            (i32x4.mul (v128.load offset=160 (local.get $p)) (v128.load offset=176 (local.get $p))))
          (i32x4.mul
            (i32x4.mul (v128.load offset=192 (local.get $p)) (v128.load offset=208 (local.get $p)))
            (i32x4.mul (v128.load offset=224 (local.get $p)) (v128.load offset=240 (local.get $p)))))))
    (i32.add (i32x4.extract_lane 0 (local.get $a)) (i32x4.extract_lane 3 (local.get $a))))

  (func $spin (param $p i32) (result i32)
    (loop $l (br_if $l (i32.eqz (i32.load (local.get $p)))))
    (i32.const 7))

  (func $rec (export "rec") (param $n i32) (param $which i32) (result i32)
    (if (result i32) (i32.eqz (local.get $n))
      (then
        (if (result i32) (local.get $which)
          (then (call $spin (i32.const 1024)))
          (else (call $leaf (i32.const 0)))))
      (else (call $rec (i32.sub (local.get $n) (i32.const 1)) (local.get $which)))))

  ;; Unbounded recursion whose every level also calls the leaf.
  (func $deep (export "deep") (param $n i32) (result i32)
    (drop (call $leaf (i32.const 0)))
    (call $deep (i32.add (local.get $n) (i32.const 1))))
)
"#;

fn slack_config(slack: u32) -> Config {
    let mut c = Config::new();
    c.max_wasm_stack(64 * 1024);
    c.ix_leaf_stack_slack(slack);
    c.epoch_interruption(true);
    c
}

#[test]
fn ix_slack_is_bounded() {
    let mut c = Config::new();
    c.max_wasm_stack(64 * 1024);
    c.ix_leaf_stack_slack(16 * 1024 + 1);
    assert!(
        Engine::new(&c).is_err(),
        "slack over a quarter of the budget"
    );
    let mut c = Config::new();
    c.max_wasm_stack(1 << 20);
    c.ix_leaf_stack_slack(64 * 1024 + 1);
    assert!(Engine::new(&c).is_err(), "slack over 64 KiB");
    let mut c = Config::new();
    c.max_wasm_stack(64 * 1024);
    c.ix_leaf_stack_slack(16 * 1024);
    assert!(Engine::new(&c).is_ok());
}

#[test]
fn ix_slack_artifacts_do_not_mix() -> Result<()> {
    let a = Engine::new(&slack_config(4096))?;
    let b = Engine::new(&slack_config(0))?;
    let bytes = a.precompile_module(STACK_WAT.as_bytes())?;
    // SAFETY: bytes come from `precompile_module` just above.
    let err = unsafe { Module::deserialize(&b, &bytes) }.unwrap_err();
    assert!(
        format!("{err:?}").contains("ix leaf stack slack"),
        "{err:?}"
    );
    Ok(())
}

/// Deep recursion still traps cleanly, every time, with unchecked leaves at
/// the bottom of the stack.
#[test]
fn ix_slack_deep_recursion_traps() -> Result<()> {
    for slack in [0, 4096] {
        let engine = Engine::new(&slack_config(slack))?;
        let module = Module::new(&engine, STACK_WAT)?;
        let mut store = Store::new(&engine, ());
        store.set_epoch_deadline(u64::MAX / 2);
        let instance = Instance::new(&mut store, &module, &[])?;
        let deep = instance.get_typed_func::<i32, i32>(&mut store, "deep")?;
        for _ in 0..200 {
            let t = trap_of(deep.call(&mut store, 0).unwrap_err());
            assert_eq!(t, Trap::StackOverflow, "slack {slack}");
        }
        // The store still works after all those overflows.
        let rec = instance.get_typed_func::<(i32, i32), i32>(&mut store, "rec")?;
        assert_eq!(rec.call(&mut store, (10, 0))?, 0);
    }
    Ok(())
}

/// Walk the recursion depth up to the overflow point: each depth either
/// returns or traps with `StackOverflow`; the leaf at the bottom never
/// faults. At the deepest depth that returns, the spinning leaf reaches the
/// epoch builtin from the slack region and is interrupted cleanly.
#[test]
fn ix_slack_epoch_from_deepest_leaf() -> Result<()> {
    let engine = Engine::new(&slack_config(4096))?;
    let module = Module::new(&engine, STACK_WAT)?;
    let mut store = Store::new(&engine, ());
    store.set_epoch_deadline(u64::MAX / 2);
    let instance = Instance::new(&mut store, &module, &[])?;
    let rec = instance.get_typed_func::<(i32, i32), i32>(&mut store, "rec")?;
    let mut deepest = None;
    for n in 0..100_000 {
        match rec.call(&mut store, (n, 0)) {
            Ok(_) => deepest = Some(n),
            Err(e) => {
                assert_eq!(trap_of(e), Trap::StackOverflow);
                break;
            }
        }
    }
    let deepest = deepest.expect("depth 0 returns");
    assert!(
        deepest > 10,
        "recursion this shallow means the budget is wrong"
    );

    let stop = Arc::new(AtomicBool::new(false));
    let ticker = {
        let engine = engine.clone();
        let stop = stop.clone();
        std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                engine.increment_epoch();
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        })
    };
    for _ in 0..20 {
        store.set_epoch_deadline(1);
        let t = trap_of(rec.call(&mut store, (deepest, 1)).unwrap_err());
        assert_eq!(t, Trap::Interrupt);
    }
    stop.store(true, Ordering::Relaxed);
    ticker.join().unwrap();
    Ok(())
}

/// Host imports that re-enter wasm, alternating an unchecked leaf and the
/// recursive caller, at ever lower stack pointers: the chain ends in a clean
/// `StackOverflow` (a re-entry below the limit is refused before any wasm
/// runs). v50's epoch and call-hook callbacks get no `AsContextMut`, so a
/// builtin called from a leaf can never re-enter wasm; imports are the only
/// way back in, and a leaf never calls one.
#[test]
fn ix_slack_reentry_through_imports_is_bounded() -> Result<()> {
    const WAT: &str = r#"
      (module
        (import "" "h" (func $h (param i32) (result i32)))
        (memory 1)
        (func (export "leaf") (param i32) (result i32)
          (i32.add (i32.load (local.get 0)) (i32.const 1)))
        (func (export "rec") (param i32) (result i32)
          (call $h (i32.add (local.get 0) (i32.const 1)))))
    "#;
    type Fs = Option<(TypedFunc<i32, i32>, TypedFunc<i32, i32>)>;
    for slack in [0, 4096] {
        let engine = Engine::new(&slack_config(slack))?;
        let module = Module::new(&engine, WAT)?;
        let mut store = Store::new(&engine, None::<(TypedFunc<i32, i32>, TypedFunc<i32, i32>)>);
        store.set_epoch_deadline(u64::MAX / 2);
        let h = Func::wrap(
            &mut store,
            |mut cx: Caller<'_, Fs>, n: i32| -> Result<i32> {
                let (leaf, rec) = cx.data().clone().unwrap();
                leaf.call(&mut cx, 0)?;
                rec.call(&mut cx, n)
            },
        );
        let instance = Instance::new(&mut store, &module, &[h.into()])?;
        let leaf = instance.get_typed_func::<i32, i32>(&mut store, "leaf")?;
        let rec = instance.get_typed_func::<i32, i32>(&mut store, "rec")?;
        *store.data_mut() = Some((leaf.clone(), rec.clone()));
        for _ in 0..20 {
            let t = trap_of(rec.call(&mut store, 0).unwrap_err());
            assert_eq!(t, Trap::StackOverflow, "slack {slack}");
        }
        assert_eq!(leaf.call(&mut store, 0)?, 1);
    }
    Ok(())
}

// ------------------------------------------------------------ table devirt

fn devirt_engine(on: bool, lazy: bool) -> Result<Engine> {
    let mut c = Config::new();
    c.ix_table_devirt(on);
    c.table_lazy_init(lazy);
    Engine::new(&c)
}

fn run(engine: &Engine, wat: &str, export: &str, arg: i32) -> Result<i32> {
    let module = Module::new(engine, wat)?;
    let mut store = Store::new(engine, ());
    let instance = Instance::new(&mut store, &module, &[])?;
    let f = instance.get_typed_func::<i32, i32>(&mut store, export)?;
    f.call(&mut store, arg)
}

fn for_each_engine(mut f: impl FnMut(&Engine) -> Result<()>) -> Result<()> {
    for on in [false, true] {
        for lazy in [false, true] {
            f(&devirt_engine(on, lazy)?)?;
        }
    }
    Ok(())
}

/// A static table where every element has type `$t`; slot 0 is null.
const STATIC_UNIFORM: &str = r#"
(module
  (type $t (func (param i32) (result i32)))
  (type $u (func (param i32) (result i64)))
  (table 4 8 funcref)
  (elem (i32.const 1) func $a $b)
  (func $a (type $t) (i32.add (local.get 0) (i32.const 100)))
  (func $b (type $t) (i32.add (local.get 0) (i32.const 200)))
  ;; Dynamic index: the signature check is dropped, bounds and null stay.
  (func (export "dyn") (param i32) (result i32)
    (call_indirect (type $t) (i32.const 5) (local.get 0)))
  ;; Constant index: becomes a direct call to $b.
  (func (export "const") (param i32) (result i32)
    (call_indirect (type $t) (local.get 0) (i32.const 2)))
  ;; Constant index of a null slot: still traps.
  (func (export "const_null") (param i32) (result i32)
    (call_indirect (type $t) (local.get 0) (i32.const 0)))
  ;; Constant index out of bounds (size is 4 even though max is 8).
  (func (export "const_oob") (param i32) (result i32)
    (call_indirect (type $t) (local.get 0) (i32.const 4)))
  ;; Wrong type through the static table: still traps.
  (func (export "wrong") (param i32) (result i32)
    (i32.wrap_i64 (call_indirect (type $u) (local.get 0) (local.get 0))))
)
"#;

#[test]
fn ix_devirt_static_table_semantics() -> Result<()> {
    for_each_engine(|e| {
        assert_eq!(run(e, STATIC_UNIFORM, "dyn", 1)?, 105);
        assert_eq!(run(e, STATIC_UNIFORM, "dyn", 2)?, 205);
        assert_eq!(
            trap_of(run(e, STATIC_UNIFORM, "dyn", 0).unwrap_err()),
            Trap::IndirectCallToNull
        );
        assert_eq!(
            trap_of(run(e, STATIC_UNIFORM, "dyn", 3).unwrap_err()),
            Trap::IndirectCallToNull
        );
        assert_eq!(
            trap_of(run(e, STATIC_UNIFORM, "dyn", 4).unwrap_err()),
            Trap::TableOutOfBounds
        );
        assert_eq!(
            trap_of(run(e, STATIC_UNIFORM, "dyn", -1).unwrap_err()),
            Trap::TableOutOfBounds
        );
        assert_eq!(run(e, STATIC_UNIFORM, "const", 7)?, 207);
        assert_eq!(
            trap_of(run(e, STATIC_UNIFORM, "const_null", 0).unwrap_err()),
            Trap::IndirectCallToNull
        );
        assert_eq!(
            trap_of(run(e, STATIC_UNIFORM, "const_oob", 0).unwrap_err()),
            Trap::TableOutOfBounds
        );
        for i in [1, 2] {
            assert_eq!(
                trap_of(run(e, STATIC_UNIFORM, "wrong", i).unwrap_err()),
                Trap::BadSignature
            );
        }
        Ok(())
    })
}

/// The same table, but some function in the module can write it, so none
/// of the facts hold. Every way of writing a table is covered, and a
/// mismatched signature written in at runtime must trap.
#[test]
fn ix_devirt_mutable_table_keeps_checks() -> Result<()> {
    let writers = [
        "(func (export \"w\") (param i32) (result i32) (table.set (local.get 0) (ref.func $bad)) (i32.const 0))",
        "(func (export \"w\") (param i32) (result i32) (table.fill (local.get 0) (ref.func $bad) (i32.const 1)) (i32.const 0))",
        "(func (export \"w\") (param i32) (result i32) (table.init $pe (local.get 0) (i32.const 0) (i32.const 1)) (i32.const 0))",
        "(func (export \"w\") (param i32) (result i32) (table.copy (local.get 0) (i32.const 3) (i32.const 1)) (i32.const 0))",
        "(func (export \"w\") (param i32) (result i32) (drop (table.grow (ref.func $bad) (i32.const 1))) (i32.const 0))",
    ];
    for (k, writer) in writers.iter().enumerate() {
        // Slot 3 holds $bad (wrong type) for the table.copy case.
        let wat = format!(
            r#"(module
              (type $t (func (param i32) (result i32)))
              (table 4 8 funcref)
              (elem (i32.const 1) func $a $a)
              (elem (i32.const 3) func $bad)
              (elem $pe func $bad)
              (func $a (type $t) (i32.add (local.get 0) (i32.const 100)))
              (func $bad (param i64) (result i64) (local.get 0))
              {writer}
              (func (export "call") (param i32) (result i32)
                (call_indirect (type $t) (i32.const 0) (local.get 0)))
              (func (export "const1") (param i32) (result i32)
                (call_indirect (type $t) (i32.const 0) (i32.const 1)))
            )"#
        );
        for_each_engine(|e| {
            let module = Module::new(e, &wat)?;
            let mut store = Store::new(e, ());
            let inst = Instance::new(&mut store, &module, &[])?;
            let w = inst.get_typed_func::<i32, i32>(&mut store, "w")?;
            let call = inst.get_typed_func::<i32, i32>(&mut store, "call")?;
            let const1 = inst.get_typed_func::<i32, i32>(&mut store, "const1")?;
            assert_eq!(call.call(&mut store, 1)?, 100);
            assert_eq!(
                trap_of(call.call(&mut store, 3).unwrap_err()),
                Trap::BadSignature
            );
            // Overwrite slot 1 (copy: slot 1 <- slot 3; grow appends at 4).
            let slot = if k == 4 { 4 } else { 1 };
            w.call(&mut store, if k == 4 { 0 } else { 1 })?;
            assert_eq!(
                trap_of(call.call(&mut store, slot).unwrap_err()),
                Trap::BadSignature,
                "writer {k}"
            );
            if slot == 1 {
                assert_eq!(
                    trap_of(const1.call(&mut store, 0).unwrap_err()),
                    Trap::BadSignature,
                    "writer {k}"
                );
            }
            Ok(())
        })?;
    }
    Ok(())
}

/// An exported table can be written by the host: no facts.
#[test]
fn ix_devirt_exported_table_keeps_checks() -> Result<()> {
    let wat = r#"(module
      (type $t (func (param i32) (result i32)))
      (table (export "t") 2 funcref)
      (elem (i32.const 0) func $a $a)
      (func $a (type $t) (local.get 0))
      (func $bad (export "bad") (param i64) (result i64) (local.get 0))
      (func (export "call") (param i32) (result i32)
        (call_indirect (type $t) (i32.const 0) (i32.const 1)))
    )"#;
    for_each_engine(|e| {
        let module = Module::new(e, wat)?;
        let mut store = Store::new(e, ());
        let inst = Instance::new(&mut store, &module, &[])?;
        let call = inst.get_typed_func::<i32, i32>(&mut store, "call")?;
        assert_eq!(call.call(&mut store, 0)?, 0);
        let t = inst.get_table(&mut store, "t").unwrap();
        let bad = inst.get_func(&mut store, "bad").unwrap();
        t.set(&mut store, 1, Ref::Func(Some(bad)))?;
        assert_eq!(
            trap_of(call.call(&mut store, 0).unwrap_err()),
            Trap::BadSignature
        );
        Ok(())
    })
}

/// An imported table can be written by its owner: no facts.
#[test]
fn ix_devirt_imported_table_keeps_checks() -> Result<()> {
    let wat = r#"(module
      (type $t (func (param i32) (result i32)))
      (import "" "t" (table 2 funcref))
      (elem (i32.const 0) func $a $a)
      (func $a (type $t) (local.get 0))
      (func (export "call") (param i32) (result i32)
        (call_indirect (type $t) (i32.const 0) (i32.const 1)))
    )"#;
    for_each_engine(|e| {
        let module = Module::new(e, wat)?;
        let mut store = Store::new(e, ());
        let ty = TableType::new(RefType::FUNCREF, 2, None);
        let table = Table::new(&mut store, ty, Ref::Func(None))?;
        let inst = Instance::new(&mut store, &module, &[table.into()])?;
        let call = inst.get_typed_func::<i32, i32>(&mut store, "call")?;
        assert_eq!(call.call(&mut store, 0)?, 0);
        let bad = Func::wrap(&mut store, |x: i64| x);
        table.set(&mut store, 1, Ref::Func(Some(bad)))?;
        assert_eq!(
            trap_of(call.call(&mut store, 0).unwrap_err()),
            Trap::BadSignature
        );
        Ok(())
    })
}

/// A static table with mixed types keeps the signature check.
#[test]
fn ix_devirt_mixed_static_table_keeps_sig_check() -> Result<()> {
    let wat = r#"(module
      (type $t (func (param i32) (result i32)))
      (table 3 3 funcref)
      (elem (i32.const 0) func $a $bad $a)
      (func $a (type $t) (local.get 0))
      (func $bad (param i64) (result i64) (local.get 0))
      (func (export "call") (param i32) (result i32)
        (call_indirect (type $t) (local.get 0) (local.get 0)))
      (func (export "const_bad") (param i32) (result i32)
        (call_indirect (type $t) (local.get 0) (i32.const 1)))
    )"#;
    for_each_engine(|e| {
        assert_eq!(run(e, wat, "call", 2)?, 2);
        assert_eq!(
            trap_of(run(e, wat, "call", 1).unwrap_err()),
            Trap::BadSignature
        );
        assert_eq!(
            trap_of(run(e, wat, "const_bad", 0).unwrap_err()),
            Trap::BadSignature
        );
        Ok(())
    })
}
