//! ix fork: tests for `Config::ix_pinned_sp` (the shadow stack pointer
//! global cached in Cranelift's pinned register).
//!
//! The option must be invisible: every test runs the same module with the
//! option off and on and expects the same observable results.

#![cfg(not(miri))]
#![cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]

use wasmtime::*;

fn engine(pinned: bool) -> Engine {
    let mut c = Config::new();
    c.wasm_tail_call(true);
    c.ix_pinned_sp(pinned);
    Engine::new(&c).unwrap()
}

fn both(test: impl Fn(&Engine)) {
    for pinned in [false, true] {
        let e = engine(pinned);
        assert_eq!(e.get_ix_pinned_sp(), u8::from(pinned));
        test(&e);
    }
}

fn sp(store: &mut Store<()>, i: &Instance) -> i32 {
    i.get_global(&mut *store, "sp").unwrap().get(&mut *store).unwrap_i32()
}

/// `$__stack_pointer` is named in the name section, so it is the designated
/// global even though it is exported.
const IMPORT_WAT: &str = r#"
(module
  (import "h" "f" (func $f (param i32) (result i32)))
  (global $__stack_pointer (export "sp") (mut i32) (i32.const 1000))
  (func $push (param $n i32)
    (global.set $__stack_pointer (i32.sub (global.get $__stack_pointer) (local.get $n))))
  (func $pop (param $n i32)
    (global.set $__stack_pointer (i32.add (global.get $__stack_pointer) (local.get $n))))
  ;; Pushes 16 through an internal function, calls the import with the
  ;; current value, and returns the value it reads afterwards.
  (func (export "run") (result i32)
    (call $push (i32.const 16))
    (drop (call $f (global.get $__stack_pointer)))
    (global.get $__stack_pointer))
  (func (export "get") (result i32) (global.get $__stack_pointer))
  ;; Writes `v`, then traps.
  (func (export "set_and_trap") (param $v i32)
    (global.set $__stack_pointer (local.get $v))
    unreachable)
  ;; Re-entered from the import: pushes 32 and pops 32 through internal
  ;; functions, then leaves 777 behind.
  (func (export "inner") (result i32)
    (call $push (i32.const 32))
    (call $pop (i32.const 32))
    (global.set $__stack_pointer (i32.const 777))
    (global.get $__stack_pointer))
)
"#;

#[test]
fn host_sees_value_during_import_and_its_write_after() {
    both(|e| {
        let m = Module::new(e, IMPORT_WAT).unwrap();
        let mut store = Store::new(e, ());
        let f = Func::wrap(&mut store, |mut c: Caller<'_, ()>, arg: i32| -> i32 {
            let g = c.get_export("sp").unwrap().into_global().unwrap();
            // The value the guest passed is the one in the global's storage.
            assert_eq!(g.get(&mut c).unwrap_i32(), arg);
            assert_eq!(arg, 984);
            // A host write during the import is what the guest reads next.
            g.set(&mut c, Val::I32(500)).unwrap();
            0
        });
        let i = Instance::new(&mut store, &m, &[f.into()]).unwrap();
        let run = i.get_typed_func::<(), i32>(&mut store, "run").unwrap();
        assert_eq!(run.call(&mut store, ()).unwrap(), 500);
        assert_eq!(sp(&mut store, &i), 500);
    });
}

#[test]
fn value_after_trap_and_store_still_usable() {
    both(|e| {
        let m = Module::new(e, IMPORT_WAT).unwrap();
        let mut store = Store::new(e, ());
        let f = Func::wrap(&mut store, |_: i32| 0i32);
        let i = Instance::new(&mut store, &m, &[f.into()]).unwrap();
        let st = i
            .get_typed_func::<i32, ()>(&mut store, "set_and_trap")
            .unwrap();
        for v in [123, -5, 0x7fff_fff0] {
            let err = st.call(&mut store, v).unwrap_err();
            assert_eq!(err.downcast::<Trap>().unwrap(), Trap::UnreachableCodeReached);
            assert_eq!(sp(&mut store, &i), v);
            let get = i.get_typed_func::<(), i32>(&mut store, "get").unwrap();
            assert_eq!(get.call(&mut store, ()).unwrap(), v);
        }
    });
}

#[test]
fn reentry_through_import() {
    both(|e| {
        let m = Module::new(e, IMPORT_WAT).unwrap();
        let mut store = Store::new(e, ());
        let f = Func::wrap(&mut store, |mut c: Caller<'_, ()>, arg: i32| -> i32 {
            assert_eq!(arg, 984);
            let inner = c.get_export("inner").unwrap().into_func().unwrap();
            let inner = inner.typed::<(), i32>(&c).unwrap();
            assert_eq!(inner.call(&mut c, ()).unwrap(), 777);
            let g = c.get_export("sp").unwrap().into_global().unwrap();
            assert_eq!(g.get(&mut c).unwrap_i32(), 777);
            0
        });
        let i = Instance::new(&mut store, &m, &[f.into()]).unwrap();
        let run = i.get_typed_func::<(), i32>(&mut store, "run").unwrap();
        // The outer frame sees what the re-entered call left behind.
        assert_eq!(run.call(&mut store, ()).unwrap(), 777);
    });
}

#[test]
fn host_set_between_calls() {
    both(|e| {
        let m = Module::new(e, IMPORT_WAT).unwrap();
        let mut store = Store::new(e, ());
        let f = Func::wrap(&mut store, |_: i32| 0i32);
        let i = Instance::new(&mut store, &m, &[f.into()]).unwrap();
        let get = i.get_typed_func::<(), i32>(&mut store, "get").unwrap();
        for v in [1, 42, -1] {
            i.get_global(&mut store, "sp")
                .unwrap()
                .set(&mut store, Val::I32(v))
                .unwrap();
            assert_eq!(get.call(&mut store, ()).unwrap(), v);
        }
    });
}

#[test]
fn start_function_reads_the_slot() {
    // The start function is not escaping, but the host calls it, so it must
    // reload the register at entry. It copies the designated global into
    // `$out` and pushes a frame through an internal function.
    let wat = r#"
    (module
      (global $__stack_pointer (mut i32) (i32.const 4096))
      (global $out (export "out") (mut i32) (i32.const 0))
      (func $push (global.set $__stack_pointer (i32.sub (global.get $__stack_pointer) (i32.const 8))))
      (func $start
        (call $push)
        (global.set $out (global.get $__stack_pointer)))
      (start $start))
    "#;
    both(|e| {
        let m = Module::new(e, wat).unwrap();
        let mut store = Store::new(e, ());
        let i = Instance::new(&mut store, &m, &[]).unwrap();
        let out = i.get_global(&mut store, "out").unwrap();
        assert_eq!(out.get(&mut store).unwrap_i32(), 4088);
    });
}

/// Module B: its own `__stack_pointer` at 5000, read by an export.
const B_WAT: &str = r#"
(module
  (global $__stack_pointer (mut i32) (i32.const 5000))
  (func (export "g") (result i32)
    (global.set $__stack_pointer (i32.sub (global.get $__stack_pointer) (i32.const 1)))
    (global.get $__stack_pointer)))
"#;

/// Module A calls B in four ways and checks it still sees its own global.
const A_WAT: &str = r#"
(module
  (import "b" "g" (func $g (result i32)))
  (global $__stack_pointer (mut i32) (i32.const 1000))
  (type $t (func (result i32)))
  (table 1 funcref)
  (elem (i32.const 0) $g)
  ;; Internal and never escaping: tail-calls out of the module.
  (func $tail_out (result i32)
    (return_call_indirect (type $t) (i32.const 0)))
  ;; Internal: a direct tail call to a function that tail-calls out.
  (func $tail_chain (result i32)
    (return_call $tail_out))
  (func $tail_import (result i32)
    (return_call $g))
  (func (export "via_import") (result i32)
    (drop (call $g)) (global.get $__stack_pointer))
  (func (export "via_table") (result i32)
    (drop (call_indirect (type $t) (i32.const 0))) (global.get $__stack_pointer))
  (func (export "via_tail") (result i32)
    (drop (call $tail_out)) (global.get $__stack_pointer))
  (func (export "via_tail_chain") (result i32)
    (drop (call $tail_chain)) (global.get $__stack_pointer))
  (func (export "via_tail_import") (result i32)
    (drop (call $tail_import)) (global.get $__stack_pointer))
)
"#;

#[test]
fn calls_into_another_instance_keep_our_value() {
    both(|e| {
        let mut store = Store::new(e, ());
        let b = Instance::new(&mut store, &Module::new(e, B_WAT).unwrap(), &[]).unwrap();
        let g = b.get_func(&mut store, "g").unwrap();
        let a = Instance::new(&mut store, &Module::new(e, A_WAT).unwrap(), &[g.into()]).unwrap();
        for name in [
            "via_import",
            "via_table",
            "via_tail",
            "via_tail_chain",
            "via_tail_import",
        ] {
            let f = a.get_typed_func::<(), i32>(&mut store, name).unwrap();
            assert_eq!(f.call(&mut store, ()).unwrap(), 1000, "{name}");
        }
        // B's global moved once per call (and stayed B's).
        let g = g.typed::<(), i32>(&store).unwrap();
        assert_eq!(g.call(&mut store, ()).unwrap(), 5000 - 6);
    });
}

/// A recursive computation that keeps its frames on the shadow stack, in a
/// stripped module (no name section), where the designated global is found
/// by the fallback rule.
const SHADOW_WAT: &str = r#"
(module
  (memory 1)
  (global (mut i32) (i32.const 65536))
  (global $calls (export "calls") (mut i32) (i32.const 0))
  (type $t (func (param i32) (result i32)))
  (table 1 funcref)
  (elem (i32.const 0) 2)
  ;; f(n): frame of 16 bytes holding n; f(n) = n + f(n-1) through a mix of
  ;; direct and indirect calls.
  (func (param i32) (result i32)
    (local i32)
    (global.set $calls (i32.add (global.get $calls) (i32.const 1)))
    (local.set 1 (i32.sub (global.get 0) (i32.const 16)))
    (global.set 0 (local.get 1))
    (i32.store (local.get 1) (local.get 0))
    (if (result i32) (i32.eqz (local.get 0))
      (then (i32.const 0))
      (else
        (i32.add
          (i32.load (local.get 1))
          (if (result i32) (i32.and (local.get 0) (i32.const 1))
            (then (call 2 (i32.sub (local.get 0) (i32.const 1))))
            (else (call_indirect (type $t) (i32.sub (local.get 0) (i32.const 1)) (i32.const 0)))))))
    (global.set 0 (i32.add (local.get 1) (i32.const 16))))
  (func (param i32) (result i32) (call 0 (local.get 0)))
  (func (param i32) (result i32) (call 1 (local.get 0)))
  (func (export "sum") (param i32) (result i32)
    (i32.add (call 0 (local.get 0)) (global.get 0))))
"#;

#[test]
fn shadow_stack_recursion_matches() {
    both(|e| {
        let wasm = wat::parse_str(SHADOW_WAT).unwrap();
        let m = Module::new(e, &wasm).unwrap();
        let mut store = Store::new(e, ());
        let i = Instance::new(&mut store, &m, &[]).unwrap();
        let sum = i.get_typed_func::<i32, i32>(&mut store, "sum").unwrap();
        // The frames balance, so the global is back at 65536.
        assert_eq!(sum.call(&mut store, 1000).unwrap(), 500500 + 65536);
        // Deep enough to overflow the shadow stack region: a trap, then
        // reuse.
        assert!(sum.call(&mut store, 5000).is_err());
    });
}

#[test]
fn designation_rule() {
    use std::sync::Mutex;
    // Counting mode bumps process-wide counters, so this test runs its cases
    // one at a time.
    static LOCK: Mutex<()> = Mutex::new(());
    let _g = LOCK.lock().unwrap();

    let mut c = Config::new();
    unsafe {
        c.ix_unsafe_pinned_sp_bench(4);
    }
    let e = Engine::new(&c).unwrap();
    let count = |wat: &str, export: &str| -> (u64, u64) {
        let m = Module::new(&e, wat).unwrap();
        let mut store = Store::new(&e, ());
        let i = Instance::new(&mut store, &m, &[]).unwrap();
        let f = i.get_typed_func::<(), i32>(&mut store, export).unwrap();
        wasmtime::ix_pinned_sp_take_counts();
        f.call(&mut store, ()).unwrap();
        wasmtime::ix_pinned_sp_take_counts()
    };
    // Named and exported: designated by name.
    let named = r#"(module
      (global $other (mut i32) (i32.const 0))
      (global $__stack_pointer (export "sp") (mut i32) (i32.const 8))
      (func (export "f") (result i32)
        (global.set $__stack_pointer (i32.const 1))
        (global.set $other (i32.const 2))
        (global.get $__stack_pointer)))"#;
    assert_eq!(count(named, "f"), (1, 1));
    // Stripped: the first defined mutable i32 that is not exported.
    let stripped = wat::parse_str(
        r#"(module
      (global (export "e") (mut i32) (i32.const 0))
      (global i64 (i64.const 0))
      (global (mut i64) (i64.const 0))
      (global (mut i32) (i32.const 8))
      (func (export "f") (result i32)
        (global.set 0 (i32.const 1))
        (global.set 3 (global.get 3))
        (global.get 3)))"#,
    )
    .unwrap();
    let m = Module::new(&e, &stripped).unwrap();
    let mut store = Store::new(&e, ());
    let i = Instance::new(&mut store, &m, &[]).unwrap();
    let f = i.get_typed_func::<(), i32>(&mut store, "f").unwrap();
    wasmtime::ix_pinned_sp_take_counts();
    f.call(&mut store, ()).unwrap();
    assert_eq!(wasmtime::ix_pinned_sp_take_counts(), (2, 1));
    // Named but imported: nothing is designated, even though another global
    // would pass the fallback rule.
    let named_imported = r#"(module
      (import "env" "__stack_pointer" (global $__stack_pointer (mut i32)))
      (global $x (mut i32) (i32.const 0))
      (func (export "f") (result i32)
        (global.set $x (global.get $__stack_pointer))
        (global.get $x)))"#;
    let m = Module::new(&e, named_imported).unwrap();
    let mut store = Store::new(&e, ());
    let g = Global::new(
        &mut store,
        GlobalType::new(ValType::I32, Mutability::Var),
        Val::I32(3),
    )
    .unwrap();
    let i = Instance::new(&mut store, &m, &[g.into()]).unwrap();
    let f = i.get_typed_func::<(), i32>(&mut store, "f").unwrap();
    wasmtime::ix_pinned_sp_take_counts();
    assert_eq!(f.call(&mut store, ()).unwrap(), 3);
    assert_eq!(wasmtime::ix_pinned_sp_take_counts(), (0, 0));
}

#[test]
fn artifacts_do_not_mix() {
    let on = engine(true);
    let off = engine(false);
    let bytes_on = Module::new(&on, IMPORT_WAT).unwrap().serialize().unwrap();
    let bytes_off = Module::new(&off, IMPORT_WAT).unwrap().serialize().unwrap();
    unsafe {
        assert!(Module::deserialize(&off, &bytes_on).is_err());
        assert!(Module::deserialize(&on, &bytes_off).is_err());
        assert!(Module::deserialize(&on, &bytes_on).is_ok());
    }
}

#[test]
fn refuses_stack_switching() {
    let mut c = Config::new();
    c.ix_pinned_sp(true);
    c.wasm_stack_switching(true);
    c.wasm_exceptions(true);
    c.wasm_function_references(true);
    // On aarch64 v50 refuses stack switching itself, before our check.
    let err = Engine::new(&c).unwrap_err().to_string();
    assert!(err.contains("ix_pinned_sp") || err.contains("stack_switching"), "{err}");
}

// ---------------------------------------------------- the host's register

/// The host's value of the pinned register must survive a call into wasm on
/// the normal and the trap path: the guest picks the register's value, so a
/// missed restore would hand guest data to host code. The call is made from
/// inline asm that puts a magic value in the register and reads it back.
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
mod host_register {
    use super::*;
    use std::ffi::c_void;

    const MAGIC: u64 = 0x5eed_cafe_f00d_d00d;

    struct Ctx {
        store: Store<()>,
        f: TypedFunc<i32, ()>,
        trapped: bool,
    }

    extern "C" fn enter(ctx: *mut c_void) {
        let ctx = unsafe { &mut *ctx.cast::<Ctx>() };
        ctx.trapped = ctx.f.call(&mut ctx.store, 0x1234_5678).is_err();
    }

    fn call_with_magic(ctx: &mut Ctx) -> u64 {
        let ctx = core::ptr::from_mut(ctx).cast::<c_void>();
        let out: u64;
        #[cfg(target_arch = "x86_64")]
        unsafe {
            core::arch::asm!(
                "call {f}",
                f = in(reg) enter as extern "C" fn(*mut c_void),
                in("rdi") ctx,
                inout("r15") MAGIC => out,
                clobber_abi("C"),
            );
        }
        #[cfg(target_arch = "aarch64")]
        unsafe {
            core::arch::asm!(
                "blr {f}",
                f = in(reg) enter as extern "C" fn(*mut c_void),
                in("x0") ctx,
                inout("x21") MAGIC => out,
                clobber_abi("C"),
            );
        }
        out
    }

    #[test]
    fn survives_return_and_trap() {
        let wat = r#"
        (module
          (global $__stack_pointer (mut i32) (i32.const 0))
          (func $set (param i32) (global.set $__stack_pointer (local.get 0)))
          (func (export "ok") (param i32) (call $set (local.get 0)))
          (func (export "trap") (param i32) (call $set (local.get 0)) unreachable))
        "#;
        for pinned in [false, true] {
            let e = engine(pinned);
            let m = Module::new(&e, wat).unwrap();
            for (name, trapped) in [("ok", false), ("trap", true)] {
                let mut store = Store::new(&e, ());
                let i = Instance::new(&mut store, &m, &[]).unwrap();
                let f = i.get_typed_func::<i32, ()>(&mut store, name).unwrap();
                let mut ctx = Ctx {
                    store,
                    f,
                    trapped: false,
                };
                for _ in 0..100 {
                    assert_eq!(call_with_magic(&mut ctx), MAGIC, "{name} pinned={pinned}");
                    assert_eq!(ctx.trapped, trapped);
                }
            }
        }
    }
}
