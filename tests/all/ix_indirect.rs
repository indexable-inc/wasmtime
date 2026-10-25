//! ix fork: tests for `Config::ix_static_call_indirect`.
//!
//! Every case runs with the dispatch on and off, lazy and eager tables, and
//! must give the same results and the same trap codes.

#![cfg(not(miri))]

use wasmtime::*;

/// Slots: 0 `$inc`, 1 `$add10` (both `$ii`), 2 `$other` (`$v`), 3 null,
/// 4 `$bump` (`$ii`, reads and writes a per-instance global), 5 null.
const STATIC_WAT: &str = r#"
(module
  (type $ii (func (param i32) (result i32)))
  (type $v (func (result i32)))
  (global $g (mut i32) (i32.const 0))
  (table 6 funcref)
  (elem (i32.const 0) $inc $add10 $other)
  (elem (i32.const 3) funcref (ref.null func) (ref.func $bump))
  (func $inc (type $ii) (i32.add (local.get 0) (i32.const 1)))
  (func $add10 (type $ii) (i32.add (local.get 0) (i32.const 10)))
  (func $other (type $v) (i32.const 99))
  (func $bump (type $ii)
    (global.set $g (i32.add (global.get $g) (local.get 0)))
    (global.get $g))
  (func (export "call") (param $i i32) (param $x i32) (result i32)
    (call_indirect (type $ii) (local.get $x) (local.get $i)))
  (func (export "tail") (param $i i32) (param $x i32) (result i32)
    (return_call_indirect (type $ii) (local.get $x) (local.get $i)))
  (func (export "call_v") (param $i i32) (result i32)
    (call_indirect (type $v) (local.get $i)))
  EXTRA
)
"#;

fn config(dispatch: bool, lazy: bool) -> Config {
    let mut c = Config::new();
    c.ix_static_call_indirect(dispatch);
    c.table_lazy_init(lazy);
    c
}

/// `(config, dispatch on)`.
fn configs() -> impl Iterator<Item = (Config, bool)> {
    [(true, true), (true, false), (false, true), (false, false)]
        .into_iter()
        .map(|(d, l)| (config(d, l), d))
}

fn trap_of(e: Error) -> Trap {
    match e.downcast::<Trap>() {
        Ok(t) => t,
        Err(e) => panic!("expected a trap, got {e:?}"),
    }
}

struct Inst {
    store: Store<()>,
    call: TypedFunc<(i32, i32), i32>,
    tail: TypedFunc<(i32, i32), i32>,
    call_v: TypedFunc<i32, i32>,
    instance: Instance,
}

fn instantiate(engine: &Engine, module: &Module, linker: &Linker<()>) -> Inst {
    let mut store = Store::new(engine, ());
    let instance = linker.instantiate(&mut store, module).unwrap();
    let call = instance.get_typed_func(&mut store, "call").unwrap();
    let tail = instance.get_typed_func(&mut store, "tail").unwrap();
    let call_v = instance.get_typed_func(&mut store, "call_v").unwrap();
    Inst {
        store,
        call,
        tail,
        call_v,
        instance,
    }
}

/// The calls and traps every variant of the module must give, for the slot
/// layout above.
fn check_semantics(i: &mut Inst) {
    for f in [i.call.clone(), i.tail.clone()] {
        assert_eq!(f.call(&mut i.store, (0, 5)).unwrap(), 6);
        assert_eq!(f.call(&mut i.store, (1, 5)).unwrap(), 15);
        // Wrong type, null, out of bounds, and Spectre-style huge indices.
        let cases = [
            (2, Trap::BadSignature),
            (3, Trap::IndirectCallToNull),
            (5, Trap::IndirectCallToNull),
            (6, Trap::TableOutOfBounds),
            (7, Trap::TableOutOfBounds),
            (-1, Trap::TableOutOfBounds),
            (i32::MIN, Trap::TableOutOfBounds),
            (i32::MAX, Trap::TableOutOfBounds),
            (1 << 16, Trap::TableOutOfBounds),
        ];
        for (idx, want) in cases {
            let e = f.call(&mut i.store, (idx, 0)).unwrap_err();
            assert_eq!(trap_of(e), want, "index {idx}");
        }
    }
    assert_eq!(i.call_v.call(&mut i.store, 2).unwrap(), 99);
    for (idx, want) in [
        (0, Trap::BadSignature),
        (4, Trap::BadSignature),
        (3, Trap::IndirectCallToNull),
        (6, Trap::TableOutOfBounds),
        (-1, Trap::TableOutOfBounds),
    ] {
        let e = i.call_v.call(&mut i.store, idx).unwrap_err();
        assert_eq!(trap_of(e), want, "call_v index {idx}");
    }
}

/// The callee runs against its own instance's state (the dispatch passes
/// the caller's vmctx, which must be the same instance).
fn check_instance_state(engine: &Engine, module: &Module, linker: &Linker<()>) {
    let mut a = instantiate(engine, module, linker);
    let mut b = instantiate(engine, module, linker);
    assert_eq!(a.call.call(&mut a.store, (4, 3)).unwrap(), 3);
    assert_eq!(a.tail.call(&mut a.store, (4, 3)).unwrap(), 6);
    assert_eq!(b.call.call(&mut b.store, (4, 1)).unwrap(), 1);
    assert_eq!(a.call.call(&mut a.store, (4, 0)).unwrap(), 6);
    assert_eq!(b.tail.call(&mut b.store, (4, 0)).unwrap(), 1);
}

#[test]
fn ix_indirect_static_table() {
    let wat = STATIC_WAT.replace("EXTRA", "");
    for (c, on) in configs() {
        let engine = Engine::new(&c).unwrap();
        let module = Module::new(&engine, &wat).unwrap();
        // One row per (table, type) called through: `$ii` and `$v`.
        assert_eq!(module.ix_dispatch_rows(), if on { 2 } else { 0 });
        let linker = Linker::new(&engine);
        check_semantics(&mut instantiate(&engine, &module, &linker));
        check_instance_state(&engine, &module, &linker);
        // A second module of the same engine, and a reload of the artifact.
        let bytes = module.serialize().unwrap();
        let module = unsafe { Module::deserialize(&engine, &bytes).unwrap() };
        assert_eq!(module.ix_dispatch_rows(), if on { 2 } else { 0 });
        check_semantics(&mut instantiate(&engine, &module, &linker));
    }
}

/// Tables the host or the module can change keep every check: the call sees
/// the new contents.
#[test]
fn ix_indirect_mutable_tables_keep_checks() {
    // Exported: the host writes it.
    let wat = STATIC_WAT.replace("EXTRA", r#"(export "t" (table 0))"#);
    for (c, _) in configs() {
        let engine = Engine::new(&c).unwrap();
        let module = Module::new(&engine, &wat).unwrap();
        assert_eq!(module.ix_dispatch_rows(), 0);
        let linker = Linker::new(&engine);
        let mut i = instantiate(&engine, &module, &linker);
        check_semantics(&mut i);
        let t = i.instance.get_table(&mut i.store, "t").unwrap();
        let add10 = t.get(&mut i.store, 1).unwrap();
        t.set(&mut i.store, 0, add10.clone()).unwrap();
        t.set(&mut i.store, 3, add10).unwrap();
        assert_eq!(i.call.call(&mut i.store, (0, 5)).unwrap(), 15);
        assert_eq!(i.call.call(&mut i.store, (3, 5)).unwrap(), 15);
        t.set(&mut i.store, 0, Ref::Func(None)).unwrap();
        let e = i.call.call(&mut i.store, (0, 5)).unwrap_err();
        assert_eq!(trap_of(e), Trap::IndirectCallToNull);
    }

    // Written by the module itself (`table.set`, `table.grow`).
    let wat = STATIC_WAT.replace(
        "EXTRA",
        r#"(func (export "set") (param i32 i32) (table.set (local.get 0) (table.get (local.get 1))))
           (func (export "grow") (result i32) (table.grow (ref.func $inc) (i32.const 2)))"#,
    );
    for (c, _) in configs() {
        let engine = Engine::new(&c).unwrap();
        let module = Module::new(&engine, &wat).unwrap();
        assert_eq!(module.ix_dispatch_rows(), 0);
        let linker = Linker::new(&engine);
        let mut i = instantiate(&engine, &module, &linker);
        check_semantics(&mut i);
        let set = i
            .instance
            .get_typed_func::<(i32, i32), ()>(&mut i.store, "set")
            .unwrap();
        let grow = i
            .instance
            .get_typed_func::<(), i32>(&mut i.store, "grow")
            .unwrap();
        set.call(&mut i.store, (3, 1)).unwrap();
        assert_eq!(i.call.call(&mut i.store, (3, 5)).unwrap(), 15);
        assert_eq!(grow.call(&mut i.store, ()).unwrap(), 6);
        assert_eq!(i.call.call(&mut i.store, (6, 5)).unwrap(), 6);
        assert_eq!(i.call.call(&mut i.store, (7, 5)).unwrap(), 6);
        let e = i.call.call(&mut i.store, (8, 5)).unwrap_err();
        assert_eq!(trap_of(e), Trap::TableOutOfBounds);
    }
}

/// A table holding an imported function keeps every check (the callee needs
/// its own vmctx, not the caller's).
#[test]
fn ix_indirect_imported_element_keeps_checks() {
    let wat = r#"
    (module
      (type $ii (func (param i32) (result i32)))
      (import "host" "f" (func $f (type $ii)))
      (table 2 funcref)
      (elem (i32.const 0) $f $inc)
      (func $inc (type $ii) (i32.add (local.get 0) (i32.const 1)))
      (func (export "call") (param $i i32) (param $x i32) (result i32)
        (call_indirect (type $ii) (local.get $x) (local.get $i))))
    "#;
    for (c, _) in configs() {
        let engine = Engine::new(&c).unwrap();
        let module = Module::new(&engine, wat).unwrap();
        assert_eq!(module.ix_dispatch_rows(), 0);
        let mut store = Store::new(&engine, ());
        let f = Func::wrap(&mut store, |x: i32| x * 100);
        let instance = Instance::new(&mut store, &module, &[f.into()]).unwrap();
        let call = instance
            .get_typed_func::<(i32, i32), i32>(&mut store, "call")
            .unwrap();
        assert_eq!(call.call(&mut store, (0, 3)).unwrap(), 300);
        assert_eq!(call.call(&mut store, (1, 3)).unwrap(), 4);
        let e = call.call(&mut store, (2, 3)).unwrap_err();
        assert_eq!(trap_of(e), Trap::TableOutOfBounds);
    }
}

/// The setting is part of the artifact compatibility check.
#[test]
fn ix_indirect_artifact_compat() {
    let wat = STATIC_WAT.replace("EXTRA", "");
    let on = Engine::new(&config(true, true)).unwrap();
    let off = Engine::new(&config(false, true)).unwrap();
    let bytes = Module::new(&on, &wat).unwrap().serialize().unwrap();
    let err = unsafe { Module::deserialize(&off, &bytes) }.unwrap_err();
    assert!(
        format!("{err:?}").contains("ix static call_indirect"),
        "{err:?}"
    );
    let bytes = Module::new(&off, &wat).unwrap().serialize().unwrap();
    assert!(unsafe { Module::deserialize(&on, &bytes) }.is_err());
}
