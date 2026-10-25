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

/// Rows stop after their last matching element: `$ii` (slots 0, 1, 4) has 5
/// entries plus the clamp slot, `$v` (slot 2) 3 plus one. A call type with
/// no element of its type gets no row and keeps the checked path.
#[test]
fn ix_indirect_rows_truncated() {
    let wat = STATIC_WAT.replace(
        "EXTRA",
        r#"(type $none (func (param i64) (result i32)))
           (func (export "call_none") (param $i i32) (result i32)
             (call_indirect (type $none) (i64.const 1) (local.get $i)))"#,
    );
    for (c, on) in configs() {
        let engine = Engine::new(&c).unwrap();
        let module = Module::new(&engine, &wat).unwrap();
        assert_eq!(module.ix_dispatch_rows(), if on { 2 } else { 0 });
        assert_eq!(module.ix_dispatch_entries(), if on { 6 + 4 } else { 0 });
        let bytes = module.ix_dispatch_bytes();
        assert!(bytes >= module.ix_dispatch_entries() * 8 && (bytes == 0) == !on);
        let linker = Linker::new(&engine);
        let mut i = instantiate(&engine, &module, &linker);
        check_semantics(&mut i);
        let none = i
            .instance
            .get_typed_func::<i32, i32>(&mut i.store, "call_none")
            .unwrap();
        for (idx, want) in [
            (0, Trap::BadSignature),
            (3, Trap::IndirectCallToNull),
            (6, Trap::TableOutOfBounds),
        ] {
            let e = none.call(&mut i.store, idx).unwrap_err();
            assert_eq!(trap_of(e), want, "call_none index {idx}");
        }
    }
}

/// A tiny module can't make a big dispatch array: a 2^20-slot table whose
/// only elements sit at its end gets no rows (they would be 2^20 entries
/// each), and the array stays within its bound of 8 entries per element
/// plus 1024.
#[test]
fn ix_indirect_sparse_table_bounded() {
    const N: usize = 64;
    let mut wat = String::from("(module\n");
    for k in 0..N {
        // Distinct types: k i32 params.
        let params = " i32".repeat(k);
        wat.push_str(&format!("(type $t{k} (func (param{params}) (result i32)))\n"));
        wat.push_str(&format!("(func $f{k} (type $t{k}) (i32.const {k}))\n"));
    }
    wat.push_str(&format!("(table {} funcref)\n", 1 << 20));
    wat.push_str(&format!("(elem (i32.const {})", (1 << 20) - N));
    for k in 0..N {
        wat.push_str(&format!(" $f{k}"));
    }
    wat.push_str(")\n");
    for k in 0..N {
        let args = "(i32.const 0)".repeat(k);
        wat.push_str(&format!(
            "(func (export \"c{k}\") (param $i i32) (result i32) (call_indirect (type $t{k}) {args} (local.get $i)))\n"
        ));
    }
    wat.push_str(")\n");
    let wasm = wat::parse_str(&wat).unwrap();
    assert!(wasm.len() < 64 * 1024, "{}", wasm.len());
    for (c, _) in configs() {
        let engine = Engine::new(&c).unwrap();
        let module = Module::new(&engine, &wasm).unwrap();
        assert!(module.ix_dispatch_entries() <= 8 * N + 1024);
        assert_eq!(module.ix_dispatch_rows(), 0);
        let mut store = Store::new(&engine, ());
        let instance = Instance::new(&mut store, &module, &[]).unwrap();
        let c0 = instance
            .get_typed_func::<i32, i32>(&mut store, "c0")
            .unwrap();
        assert_eq!(c0.call(&mut store, (1 << 20) - N as i32).unwrap(), 0);
        let e = c0.call(&mut store, 0).unwrap_err();
        assert_eq!(trap_of(e), Trap::IndirectCallToNull);
    }
}

/// Many distinct call types over a one-slot table plan in near-linear time
/// (the plan used to dedupe rows and look them up with linear scans). Every
/// type is called once in dead code, which validation and the scan still
/// see; 32 of them also have a function and a live call through a row.
#[test]
fn ix_indirect_many_types_compile_fast() {
    const TYPES: usize = 200_000;
    // Row `k` is `k + 2` entries: 32 rows fit the cap (8 per element + 1024).
    const LIVE: usize = 32;
    let valtypes = ["i32", "i64", "f32", "f64"];
    let mut wat = String::with_capacity(TYPES * 64);
    wat.push_str("(module\n");
    for k in 0..TYPES {
        // Nine params, the base-4 digits of `k`: all distinct, all final.
        wat.push_str("(type (func (param");
        let mut n = k;
        for _ in 0..9 {
            wat.push(' ');
            wat.push_str(valtypes[n % 4]);
            n /= 4;
        }
        wat.push_str(") (result i32)))\n");
    }
    let sig = |k: usize| {
        let mut n = k;
        let mut s = String::new();
        for _ in 0..9 {
            s.push_str(match n % 4 {
                0 => "(i32.const 0)",
                1 => "(i64.const 0)",
                2 => "(f32.const 0)",
                _ => "(f64.const 0)",
            });
            n /= 4;
        }
        s
    };
    for k in 0..LIVE {
        wat.push_str(&format!("(func $f{k} (type {k}) (i32.const {k}))\n"));
    }
    wat.push_str(&format!("(table {LIVE} funcref)\n(elem (i32.const 0)"));
    for k in 0..LIVE {
        wat.push_str(&format!(" $f{k}"));
    }
    wat.push_str(")\n(func (export \"dead\")\n unreachable\n");
    for k in 0..TYPES {
        wat.push_str(&format!(" call_indirect (type {k}) drop\n"));
    }
    wat.push_str(")\n(func (export \"live\") (param $i i32) (result i32)\n (i32.const 0)\n");
    for k in 0..LIVE {
        wat.push_str(&format!(
            " (i32.add (call_indirect (type {k}) {} (i32.const {k})))\n",
            sig(k)
        ));
    }
    wat.push_str("))\n");
    let wasm = wat::parse_str(&wat).unwrap();
    let mut c = config(true, true);
    c.parallel_compilation(true);
    let engine = Engine::new(&c).unwrap();
    let t = std::time::Instant::now();
    let module = Module::new(&engine, &wasm).unwrap();
    let took = t.elapsed();
    assert_eq!(module.ix_dispatch_rows(), LIVE);
    assert!(took.as_secs() < 60, "compile took {took:?}");
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let live = instance
        .get_typed_func::<i32, i32>(&mut store, "live")
        .unwrap();
    assert_eq!(live.call(&mut store, 0).unwrap(), (0..LIVE as i32).sum::<i32>());
}

/// An element segment that traps at instantiation, after an earlier one
/// wrote one of this instance's functions into an imported table: the
/// instance fails, but that function stays callable, and must see the
/// static table exactly as stock Wasmtime leaves it (its segment, after the
/// trap, never applied: all null). So the table is not static.
#[test]
fn ix_indirect_trap_before_segment_not_static() {
    let wat = |before: bool| {
        let st = r#"(elem (table $st) (i32.const 0) func $inc)"#;
        format!(
            r#"(module
              (type $ii (func (param i32) (result i32)))
              (import "host" "t" (table $imp 1 funcref))
              (table $st 1 funcref)
              (func $inc (type $ii) (i32.add (local.get 0) (i32.const 1)))
              (func $leak (type $ii) (call_indirect $st (type $ii) (local.get 0) (i32.const 0)))
              {}
              (elem (table $imp) (i32.const 0) func $leak)
              (elem (table $imp) (i32.const 1000) func $inc)
              {}
              (func (export "call") (param i32) (result i32)
                (call_indirect $st (type $ii) (local.get 0) (i32.const 0))))"#,
            if before { "" } else { st },
            if before { st } else { "" },
        )
    };
    for (c, on) in configs() {
        let engine = Engine::new(&c).unwrap();
        // Trap before the static table's segment: not static.
        let module = Module::new(&engine, &wat(true)).unwrap();
        assert_eq!(module.ix_dispatch_rows(), 0);
        let mut store = Store::new(&engine, ());
        let ty = TableType::new(RefType::FUNCREF, 1, None);
        let t = Table::new(&mut store, ty, Ref::Func(None)).unwrap();
        assert!(Instance::new(&mut store, &module, &[t.into()]).is_err());
        let leaked = t.get(&mut store, 0).unwrap();
        let leaked = leaked.unwrap_func().unwrap().typed::<i32, i32>(&store).unwrap();
        let e = leaked.call(&mut store, 5).unwrap_err();
        assert_eq!(trap_of(e), Trap::IndirectCallToNull);

        // Its segment first, the imported-table ones (which could trap)
        // only after it: the table is fully applied first, still static.
        let module = Module::new(&engine, &wat(false)).unwrap();
        assert_eq!(module.ix_dispatch_rows(), if on { 1 } else { 0 });
        let mut store = Store::new(&engine, ());
        let ty = TableType::new(RefType::FUNCREF, 1, None);
        let t = Table::new(&mut store, ty, Ref::Func(None)).unwrap();
        assert!(Instance::new(&mut store, &module, &[t.into()]).is_err());
        let leaked = t.get(&mut store, 0).unwrap();
        let leaked = leaked.unwrap_func().unwrap().typed::<i32, i32>(&store).unwrap();
        assert_eq!(leaked.call(&mut store, 5).unwrap(), 6);
    }
}

/// Artifacts from earlier fork builds, whose version tag was the hand-bumped
/// `+ix1` (ca78074 intrinsics, a162330 dispatch v1), are refused with an
/// error naming the ix ABI, both under the default version strategy and
/// under a custom version string that matches theirs (then the metadata
/// check catches them). Fixtures: `tests/all/ix_fixtures/README.md`.
#[test]
fn ix_indirect_old_artifacts_rejected() {
    // (commit, the version string it wrote, the artifact). Each must be
    // refused by the default strategy (version string), and by the artifact
    // metadata alone when the version string is not checked (`None`) or
    // matches (`Custom` set to the old string).
    let fixtures: [(&str, &str, &[u8]); 3] = [
        ("ca78074", "50+ix1", include_bytes!("ix_fixtures/ca78074.cwasm")),
        ("a162330", "50+ix1", include_bytes!("ix_fixtures/a162330.cwasm")),
        ("217de37", "50+ix3", include_bytes!("ix_fixtures/217de37.cwasm")),
    ];
    for (name, old_version, bytes) in fixtures {
        assert!(
            bytes.windows(old_version.len()).any(|w| w == old_version.as_bytes()),
            "{name}: fixture lacks {old_version}"
        );
        for dispatch in [false, true] {
            let strategies = [
                ("default", ModuleVersionStrategy::WasmtimeVersion),
                ("custom", ModuleVersionStrategy::Custom(old_version.into())),
                ("none", ModuleVersionStrategy::None),
            ];
            for (label, strategy) in strategies {
                let what = format!("{name} ({label}, dispatch {dispatch})");
                let mut c = config(dispatch, true);
                c.module_version(strategy).unwrap();
                let engine = Engine::new(&c).unwrap();
                let err = unsafe { Module::deserialize(&engine, bytes) }.unwrap_err();
                let msg = format!("{err:?}");
                assert!(msg.contains("ix fork ABI"), "{what}: {msg}");
            }
        }
    }
    // And this build's own artifacts carry the new version and load.
    let engine = Engine::new(&config(true, true)).unwrap();
    let wat = STATIC_WAT.replace("EXTRA", "");
    let bytes = Module::new(&engine, &wat).unwrap().serialize().unwrap();
    let tag = format!("+ix{}.{}", wasmtime::IX_INTRINSICS_ABI, wasmtime_environ_tag());
    assert!(bytes.windows(tag.len()).any(|w| w == tag.as_bytes()));
    unsafe { Module::deserialize(&engine, &bytes) }.unwrap();
}

fn wasmtime_environ_tag() -> &'static str {
    wasmtime::IX_ABI_TAG
}
