//! ix fork: compile-time denial of service by hostile wasm.
//!
//! The generators are the cases of wasm-host
//! bench/results/2026-10-25-compile-dos (`scripts/gen.py`), at the sizes
//! that took base Cranelift close to a minute or several GB. With the
//! default `regalloc_budget = reject`, each one is either rejected by the
//! register allocator's work budget or compiles within it (which bounds the
//! allocator's work linearly in the function's size). The fuel counts
//! themselves are pinned in regalloc2's `fuzzing::ion::budget` tests.

#![cfg(not(miri))]

use std::fmt::Write;
use wasmtime::*;

const HDR: &str = "(module\n  (import \"e\" \"g\" (func $g))\n";

fn params16() -> String {
    let mut s = String::from("(param i32");
    for _ in 0..16 {
        s.push_str(" i64");
    }
    s.push(')');
    s
}

/// `n` if/else diamonds, each adding into `$acc`; the egraph sinks the sum to
/// the return, so all `n` results stay live across the later blocks.
fn ifelse(n: usize) -> String {
    let mut o = format!(
        "{HDR}  (func (export \"f\") {} (result i64)\n    (local $acc i64)\n",
        params16()
    );
    for b in 0..n {
        let _ = writeln!(o, "    local.get 0 i32.const {} i32.and", 1u32 << (b % 31));
        o.push_str("    if (result i64)\n");
        o.push_str("      call $g local.get 1 local.get 2 i64.add local.get 3 i64.add local.get 4 i64.add\n");
        o.push_str("    else\n");
        o.push_str("      call $g local.get 5 local.get 6 i64.add local.get 7 i64.add local.get 8 i64.add\n");
        o.push_str("    end\n");
        o.push_str("    local.get $acc i64.add local.set $acc\n");
    }
    o.push_str("    local.get $acc\n");
    for i in 9..=16 {
        let _ = writeln!(o, "    local.get {i} i64.add");
    }
    o.push_str("  )\n)\n");
    o
}

/// `ifelse` with the diamond results left on the operand stack: the same
/// liveness shape at every opt level.
fn ifelse_flat(n: usize) -> String {
    let mut o = format!("{HDR}  (func (export \"f\") {} (result i64)\n", params16());
    for b in 0..n {
        let _ = writeln!(o, "    local.get 0 i32.const {} i32.and", 1u32 << (b % 31));
        o.push_str("    if (result i64)\n      call $g local.get 1 local.get 2 i64.add\n");
        o.push_str("    else\n      call $g local.get 5 local.get 6 i64.add\n    end\n");
    }
    for _ in 0..n - 1 {
        o.push_str("    i64.add\n");
    }
    o.push_str("  )\n)\n");
    o
}

/// `n` locals carried around a loop that has a call (the spilled-bundle
/// retry was O(spilled bundles x conflicts) on it).
fn copies(n: usize) -> String {
    let mut o = format!("{HDR}  (func (export \"f\") (param i64) (result i64)\n    (local");
    for _ in 0..n {
        o.push_str(" i64");
    }
    o.push_str(")\n    loop $l\n");
    for i in 1..=n {
        let _ = writeln!(o, "      local.get {i} local.get 0 i64.add local.set {i}");
    }
    o.push_str("      call $g\n");
    o.push_str("      local.get 0 i64.const 1 i64.sub local.tee 0 i64.eqz i32.eqz br_if $l\n");
    o.push_str("    end\n    local.get 1\n");
    for i in 2..=n {
        let _ = writeln!(o, "    local.get {i} i64.add");
    }
    o.push_str("  )\n)\n");
    o
}

/// `n` live values, each masked while the original stays live.
fn band(n: usize) -> String {
    let mut o = format!("{HDR}  (func (export \"f\") (param i64) (result i64)\n    (local");
    for _ in 0..n {
        o.push_str(" i64");
    }
    o.push_str(") (local $acc i64)\n");
    for i in 1..=n {
        let _ = writeln!(
            o,
            "    local.get 0 i64.const {} i64.mul local.set {i}",
            2 * i + 1
        );
    }
    let masks = ["0xff", "0xffff", "0xffffffff"];
    for i in 1..=n {
        let _ = writeln!(
            o,
            "    local.get $acc local.get {i} i64.const {} i64.and i64.add local.set $acc",
            masks[i % 3]
        );
    }
    for i in 1..=n {
        let _ = writeln!(o, "    local.get $acc local.get {i} i64.xor local.set $acc");
    }
    o.push_str("    local.get $acc)\n)\n");
    o
}

/// `n` `if` blocks, then up to 49k locals each set once (the frontend's
/// dense per-variable block map was locals x blocks).
fn widevars(n: usize) -> String {
    let k = n.min(49000);
    let mut o = format!("{HDR}  (func (export \"f\") (param i32) (result i64)\n    (local");
    for _ in 0..k {
        o.push_str(" i64");
    }
    o.push_str(")\n");
    for b in 0..n {
        let _ = writeln!(
            o,
            "    local.get 0 i32.const {} i32.and if call $g end",
            1u32 << (b % 31)
        );
    }
    for i in 1..=k {
        let _ = writeln!(o, "    i64.const {i} local.set {i}");
    }
    o.push_str("    local.get 1\n");
    for i in (2..=k).step_by((k / 64).max(1)) {
        let _ = writeln!(o, "    local.get {i} i64.add");
    }
    o.push_str("  )\n)\n");
    o
}

fn engine(budget: Option<&str>, parallel: bool) -> Engine {
    let mut config = Config::new();
    config.parallel_compilation(parallel);
    if let Some(b) = budget {
        // SAFETY: `regalloc_budget` only decides whether a function is
        // rejected or allocated again; it never changes generated code's
        // meaning.
        unsafe {
            config.cranelift_flag_set("regalloc_budget", b);
        }
    }
    Engine::new(&config).unwrap()
}

fn assert_rejected(engine: &Engine, wat: &str, what: &str) -> String {
    match Module::new(engine, wat) {
        Ok(_) => panic!("{what}: compiled, expected the regalloc budget to reject it"),
        Err(e) => {
            let msg = format!("{e:#}");
            assert!(
                msg.contains("Implementation limit exceeded"),
                "{what}: rejected for another reason: {msg}"
            );
            msg
        }
    }
}

/// Every harness case at its hostile size is rejected or compiles within the
/// budget, with the default setting (`reject`).
#[test]
fn ix_compile_dos_cases_rejected_or_bounded() {
    let e = engine(None, false);
    assert_rejected(&e, &ifelse(16384), "ifelse 16384");
    assert_rejected(&e, &ifelse_flat(8192), "ifelse_flat 8192");
    for (what, wat) in [
        ("copies 49000", copies(49000)),
        ("band 20000", band(20000)),
        ("widevars 32000", widevars(32000)),
    ] {
        if let Err(e) = Module::new(&e, &wat) {
            panic!("{what}: expected to compile within the budget: {e:?}");
        }
    }
}

/// The reject is deterministic: the same module is rejected the same way on
/// every compile, serial or parallel, while a smaller one of the same shape
/// compiles. `fallback` allocates the rejected function with the
/// single-pass allocator instead.
#[test]
fn ix_compile_dos_reject_is_deterministic() {
    let wat = ifelse(4096);
    let first = assert_rejected(&engine(None, false), &wat, "ifelse 4096");
    for parallel in [false, true, false] {
        let e = engine(Some("reject"), parallel);
        assert_eq!(assert_rejected(&e, &wat, "ifelse 4096"), first);
    }
    Module::new(&engine(None, false), &ifelse(256)).expect("ifelse 256 compiles");
    Module::new(&engine(Some("fallback"), false), &wat).expect("fallback compiles ifelse 4096");
}
