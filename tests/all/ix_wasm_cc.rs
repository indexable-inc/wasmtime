//! ix fork: tests for `Config::ix_wasm_cc` (the integer argument registers of
//! Cranelift's `tail` calling convention, used for every call between wasm
//! functions and between wasm and its trampolines).
//!
//! The option must be invisible: every test runs under each `IxWasmCc` value
//! the host supports and checks results against values computed on the host.
//! Signatures are chosen so that some integer arguments land in the added
//! registers and some still go on the stack, with float and vector arguments
//! interleaved so the integer and vector register counters advance together.

use wasmtime::*;

const ALL: [IxWasmCc; 3] = [IxWasmCc::Standard, IxWasmCc::Wide, IxWasmCc::WideTrade];

fn supported(cc: IxWasmCc) -> bool {
    cc == IxWasmCc::Standard || cfg!(any(target_arch = "x86_64", target_arch = "aarch64"))
}

fn config(cc: IxWasmCc) -> Config {
    let mut c = Config::new();
    c.ix_wasm_cc(cc);
    c.wasm_tail_call(true);
    c.wasm_function_references(true);
    c.cranelift_debug_verifier(true);
    c
}

/// Runs `f` once per supported `IxWasmCc`, with `tweak` applied to the config.
fn each_cc_with(tweak: impl Fn(&mut Config), f: impl Fn(&Engine) -> Result<()>) {
    for cc in ALL {
        if !supported(cc) {
            continue;
        }
        let mut c = config(cc);
        tweak(&mut c);
        let engine = Engine::new(&c).unwrap_or_else(|e| panic!("ix_wasm_cc = {cc}: {e:?}"));
        assert_eq!(engine.get_ix_wasm_cc(), cc);
        if let Err(e) = f(&engine) {
            panic!("ix_wasm_cc = {cc}: {e:?}");
        }
    }
}

fn each_cc(f: impl Fn(&Engine) -> Result<()>) {
    each_cc_with(|_| {}, f)
}

// ---------------------------------------------------------------------------
// Value generation and the position-sensitive mix shared by wasm and host.

#[derive(Clone, Copy, Debug, PartialEq)]
enum Ty {
    I32,
    I64,
    F64,
    V128,
}

impl Ty {
    fn wat(self) -> &'static str {
        match self {
            Ty::I32 => "i32",
            Ty::I64 => "i64",
            Ty::F64 => "f64",
            Ty::V128 => "v128",
        }
    }

    /// Instructions pushing param `i` as the i64 that `Arg::bits` mirrors.
    fn widen(self, i: usize) -> String {
        match self {
            Ty::I32 => format!("local.get {i} i64.extend_i32_u"),
            Ty::I64 => format!("local.get {i}"),
            Ty::F64 => format!("local.get {i} i64.reinterpret_f64"),
            Ty::V128 => format!(
                "local.get {i} i64x2.extract_lane 0 \
                 local.get {i} i64x2.extract_lane 1 i64.const 32 i64.rotl i64.xor"
            ),
        }
    }
}

struct Arg {
    val: Val,
    bits: u64,
    wat: String,
}

fn arg(ty: Ty, i: usize, seed: u64) -> Arg {
    let lo = seed
        .wrapping_add(i as u64)
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ ((i as u64) << 59);
    let hi = lo.rotate_left(29) ^ 0xD1B5_4A32_D192_ED03;
    match ty {
        Ty::I32 => {
            let v = lo as u32;
            Arg {
                val: Val::I32(v as i32),
                bits: u64::from(v),
                wat: format!("i32.const {}", v as i32),
            }
        }
        Ty::I64 => Arg {
            val: Val::I64(lo as i64),
            bits: lo,
            wat: format!("i64.const {}", lo as i64),
        },
        Ty::F64 => {
            let b = ((i as f64 + 1.0) * 1.25 + (seed % 1000) as f64).to_bits();
            Arg {
                val: Val::F64(b),
                bits: b,
                wat: format!("i64.const {} f64.reinterpret_i64", b as i64),
            }
        }
        Ty::V128 => Arg {
            val: Val::V128(((u128::from(hi) << 64) | u128::from(lo)).into()),
            bits: lo ^ hi.rotate_left(32),
            wat: format!("v128.const i64x2 {} {}", lo as i64, hi as i64),
        },
    }
}

fn seed(k: usize) -> u64 {
    (k as u64 + 1).wrapping_mul(0x2545_F491_4F6C_DD1D)
}

fn args_for(tys: &[Ty], seed: u64) -> Vec<Arg> {
    tys.iter()
        .enumerate()
        .map(|(i, &t)| arg(t, i, seed))
        .collect()
}

fn args_wat(args: &[Arg]) -> String {
    args.iter()
        .map(|a| a.wat.as_str())
        .collect::<Vec<_>>()
        .join(" ")
}

fn mix(bits: impl IntoIterator<Item = u64>) -> u64 {
    bits.into_iter().enumerate().fold(0, |acc, (i, b)| {
        acc.rotate_left(7) ^ b.wrapping_mul(i as u64 + 1)
    })
}

fn i64s(n: usize) -> Vec<Ty> {
    vec![Ty::I64; n]
}

fn params(tys: &[Ty]) -> String {
    if tys.is_empty() {
        return String::new();
    }
    let t: Vec<_> = tys.iter().map(|t| t.wat()).collect();
    format!("(param {})", t.join(" "))
}

/// Body computing `mix` over params `skip..`; the function must declare exactly
/// one `i64` local after its params.
fn mix_body(tys: &[Ty], skip: usize) -> String {
    let acc = tys.len();
    let mut s = String::new();
    for (pos, (i, ty)) in tys.iter().enumerate().skip(skip).enumerate() {
        s += &format!(
            "local.get {acc} i64.const 7 i64.rotl {} i64.const {} i64.mul i64.xor local.set {acc}\n",
            ty.widen(i),
            pos + 1
        );
    }
    s += &format!("local.get {acc}\n");
    s
}

const LIVE: usize = 10;

fn live_c(j: usize) -> u64 {
    (2 * j as u64 + 3).wrapping_mul(0x0101_0101_0101_0101) ^ 0x5555
}

fn live_mix(x: u64) -> u64 {
    (0..LIVE).fold(0, |acc, j| {
        acc ^ x
            .wrapping_mul(live_c(j))
            .wrapping_add(j as u64)
            .rotate_left(j as u32 + 1)
    })
}

/// `(func <head> (param i64 <extra>) (result i64))` that computes `LIVE` values
/// from param 0, runs `call` (which must leave one i64) while they are live,
/// and returns that i64 xor `live_mix(x)`.
fn driver(head: &str, extra: &[&str], call: &str) -> String {
    let base = 1 + extra.len();
    let r = base + LIVE;
    let mut s = format!("(func {head} (param i64");
    for e in extra {
        s += " ";
        s += e;
    }
    s += ") (result i64) (local";
    for _ in 0..=LIVE {
        s += " i64";
    }
    s += ")\n";
    for j in 0..LIVE {
        s += &format!(
            "local.get 0 i64.const {} i64.mul i64.const {j} i64.add local.set {}\n",
            live_c(j) as i64,
            base + j
        );
    }
    s += call;
    s += &format!("\nlocal.set {r} local.get {r}\n");
    for j in 0..LIVE {
        s += &format!(
            "local.get {} i64.const {} i64.rotl i64.xor\n",
            base + j,
            j + 1
        );
    }
    s += ")\n";
    s
}

const XS: [u64; 3] = [0, 1, 0xdead_beef_cafe_f00d];

type I12 = (i64, i64, i64, i64, i64, i64, i64, i64, i64, i64, i64, i64);

fn tuple12(v: [i64; 12]) -> I12 {
    (
        v[0], v[1], v[2], v[3], v[4], v[5], v[6], v[7], v[8], v[9], v[10], v[11],
    )
}

// ---------------------------------------------------------------------------
// 1 and 2: direct, host, call_indirect and call_ref calls of 0..=16 params.

/// Index `n` is `n` mixed i32/i64 params; index `17 + n` is `n` params cycling
/// i32, f64, i64, v128, i64.
fn sigs() -> Vec<Vec<Ty>> {
    let mut v = Vec::new();
    for n in 0..=16 {
        v.push(
            (0..n)
                .map(|i| if i % 3 == 2 { Ty::I64 } else { Ty::I32 })
                .collect(),
        );
    }
    for n in 0..=16 {
        v.push(
            (0..n)
                .map(|i| [Ty::I32, Ty::F64, Ty::I64, Ty::V128, Ty::I64][i % 5])
                .collect(),
        );
    }
    v
}

fn calls_module(sigs: &[Vec<Ty>]) -> String {
    let n = sigs.len();
    let mut s = String::from("(module\n");
    for (k, tys) in sigs.iter().enumerate() {
        s += &format!("(type $t{k} (func {} (result i64)))\n", params(tys));
        s += &format!(
            "(func $s{k} (export \"s{k}\") (type $t{k}) (local i64)\n{})\n",
            mix_body(tys, 0)
        );
        s += &format!("(global $g{k} (mut (ref null $t{k})) (ref.func $s{k}))\n");
    }
    s += &format!("(table $tab {n} funcref)\n(elem (table $tab) (i32.const 0) func");
    for k in 0..n {
        s += &format!(" $s{k}");
    }
    s += ")\n";
    for (k, tys) in sigs.iter().enumerate() {
        let a = args_wat(&args_for(tys, seed(k)));
        s += &driver(
            &format!("(export \"direct{k}\")"),
            &[],
            &format!("{a} call $s{k}"),
        );
        s += &driver(
            &format!("(export \"indirect{k}\")"),
            &["i32"],
            &format!("{a} local.get 1 call_indirect $tab (type $t{k})"),
        );
        s += &driver(
            &format!("(export \"ref{k}\")"),
            &[],
            &format!("{a} global.get $g{k} call_ref $t{k}"),
        );
    }
    s += ")\n";
    s
}

fn want(k: usize, tys: &[Ty]) -> u64 {
    mix(args_for(tys, seed(k)).iter().map(|a| a.bits))
}

#[test]
#[cfg_attr(miri, ignore)]
fn many_args_direct() {
    let sigs = sigs();
    let wat = calls_module(&sigs);
    each_cc(|engine| {
        let module = Module::new(engine, &wat)?;
        let mut store = Store::new(engine, ());
        let instance = Instance::new(&mut store, &module, &[])?;
        for (k, tys) in sigs.iter().enumerate() {
            let args = args_for(tys, seed(k));
            let w = want(k, tys);
            // Host -> wasm through the untyped entry trampoline.
            let f = instance.get_func(&mut store, &format!("s{k}")).unwrap();
            let vals: Vec<Val> = args.iter().map(|a| a.val).collect();
            let mut out = [Val::I64(0)];
            f.call(&mut store, &vals, &mut out)?;
            assert_eq!(out[0].unwrap_i64() as u64, w, "host -> s{k} {tys:?}");
            // Wasm -> wasm with values live across the call.
            let d = instance.get_typed_func::<i64, i64>(&mut store, &format!("direct{k}"))?;
            for x in XS {
                assert_eq!(
                    d.call(&mut store, x as i64)? as u64,
                    w ^ live_mix(x),
                    "direct{k} {tys:?}"
                );
            }
        }

        // Typed host -> wasm calls.
        let a = args_for(&sigs[12], seed(12));
        let i = |n: usize| a[n].val.unwrap_i32();
        let l = |n: usize| a[n].val.unwrap_i64();
        let t = instance
            .get_typed_func::<(i32, i32, i64, i32, i32, i64, i32, i32, i64, i32, i32, i64), i64>(
                &mut store, "s12",
            )?;
        let r = t.call(
            &mut store,
            (
                i(0),
                i(1),
                l(2),
                i(3),
                i(4),
                l(5),
                i(6),
                i(7),
                l(8),
                i(9),
                i(10),
                l(11),
            ),
        )?;
        assert_eq!(r as u64, want(12, &sigs[12]));

        let a = args_for(&sigs[33], seed(33));
        let i = |n: usize| a[n].val.unwrap_i32();
        let l = |n: usize| a[n].val.unwrap_i64();
        let f = |n: usize| a[n].val.unwrap_f64();
        let v = |n: usize| a[n].val.unwrap_v128();
        let t = instance.get_typed_func::<(
            i32,
            f64,
            i64,
            V128,
            i64,
            i32,
            f64,
            i64,
            V128,
            i64,
            i32,
            f64,
            i64,
            V128,
            i64,
            i32,
        ), i64>(&mut store, "s33")?;
        let r = t.call(
            &mut store,
            (
                i(0),
                f(1),
                l(2),
                v(3),
                l(4),
                i(5),
                f(6),
                l(7),
                v(8),
                l(9),
                i(10),
                f(11),
                l(12),
                v(13),
                l(14),
                i(15),
            ),
        )?;
        assert_eq!(r as u64, want(33, &sigs[33]));
        Ok(())
    });
}

#[test]
#[cfg_attr(miri, ignore)]
fn many_args_indirect() {
    let sigs = sigs();
    let wat = calls_module(&sigs);
    each_cc(|engine| {
        let module = Module::new(engine, &wat)?;
        let mut store = Store::new(engine, ());
        let instance = Instance::new(&mut store, &module, &[])?;
        for (k, tys) in sigs.iter().enumerate() {
            let w = want(k, tys);
            let ind =
                instance.get_typed_func::<(i64, i32), i64>(&mut store, &format!("indirect{k}"))?;
            let rf = instance.get_typed_func::<i64, i64>(&mut store, &format!("ref{k}"))?;
            for x in XS {
                let got = ind.call(&mut store, (x as i64, k as i32))? as u64;
                assert_eq!(got, w ^ live_mix(x), "indirect{k} {tys:?}");
                let got = rf.call(&mut store, x as i64)? as u64;
                assert_eq!(got, w ^ live_mix(x), "ref{k} {tys:?}");
            }
        }
        // A signature mismatch still traps cleanly.
        let ind = instance.get_typed_func::<(i64, i32), i64>(&mut store, "indirect12")?;
        let err = ind.call(&mut store, (0, 13)).unwrap_err();
        assert_eq!(err.downcast::<Trap>()?, Trap::BadSignature);
        assert_eq!(
            ind.call(&mut store, (5, 12))? as u64,
            want(12, &sigs[12]) ^ live_mix(5)
        );
        Ok(())
    });
}

// ---------------------------------------------------------------------------
// 3: tail calls.

/// Hop `i` takes `ar[i]` i64 params and tail calls hop `i + 1` with
/// `p[j % ar[i]] + j`, directly on even hops and through the table on odd
/// ones. The last hop returns `mix` of its params.
fn chain_module(ar: &[usize]) -> String {
    let n = ar.len();
    let mut s = String::from("(module\n");
    s += &format!("(table $tab {n} funcref)\n(elem (table $tab) (i32.const 0) func");
    for i in 0..n {
        s += &format!(" $h{i}");
    }
    s += ")\n";
    for (i, &a) in ar.iter().enumerate() {
        s += &format!("(type $ht{i} (func {} (result i64)))\n", params(&i64s(a)));
    }
    for (i, &a) in ar.iter().enumerate() {
        let export = if i == 0 { "(export \"chain\")" } else { "" };
        if i + 1 == n {
            s += &format!(
                "(func $h{i} {export} (type $ht{i}) (local i64)\n{})\n",
                mix_body(&i64s(a), 0)
            );
            continue;
        }
        let b = ar[i + 1];
        let mut body = String::new();
        for j in 0..b {
            body += &format!("local.get {} i64.const {j} i64.add\n", j % a);
        }
        if i % 2 == 0 {
            body += &format!("return_call $h{}\n", i + 1);
        } else {
            body += &format!(
                "i32.const {} return_call_indirect $tab (type $ht{})\n",
                i + 1,
                i + 1
            );
        }
        s += &format!("(func $h{i} {export} (type $ht{i})\n{body})\n");
    }
    s += ")\n";
    s
}

fn chain_expect(ar: &[usize], x: u64) -> u64 {
    let mut v = vec![x];
    for w in ar.windows(2) {
        assert_eq!(v.len(), w[0]);
        v = (0..w[1])
            .map(|j| v[j % w[0]].wrapping_add(j as u64))
            .collect();
    }
    mix(v)
}

/// `$ping` and `$pong` take a counter and 11 values and tail call each other
/// (directly, and through the table) until the counter is zero.
fn pingpong_module() -> String {
    let p12 = params(&i64s(12));
    let mut next = String::from("local.get 0 i64.const 1 i64.sub\n");
    for j in 1..11 {
        next += &format!("local.get {}\n", j + 1);
    }
    next += "local.get 1 local.get 2 i64.add local.get 0 i64.xor\n";
    let all: String = (0..12).map(|j| format!("local.get {j} ")).collect();
    let check = format!("local.get 0 i64.eqz if {all} return_call $done end\n");
    let done = mix_body(&i64s(12), 1);
    format!(
        r#"(module
  (type $pp (func {p12} (result i64)))
  (table $tab 2 funcref)
  (elem (table $tab) (i32.const 0) func $ping $pong)
  (func $ping (export "ping") (type $pp)
    {check}{next} return_call $pong)
  (func $pong (type $pp)
    {check}{next} i32.const 0 return_call_indirect $tab (type $pp))
  (func $done (type $pp) (local i64)
    {done})
)"#
    )
}

fn pingpong_expect(n: u64, x: u64) -> u64 {
    let mut p = [0u64; 12];
    p[0] = n;
    for j in 1..12 {
        p[j] = x.wrapping_add(j as u64);
    }
    while p[0] != 0 {
        let mut q = [0u64; 12];
        q[0] = p[0] - 1;
        q[1..11].copy_from_slice(&p[2..12]);
        q[11] = p[1].wrapping_add(p[2]) ^ p[0];
        p = q;
    }
    mix(p[1..].iter().copied())
}

#[test]
#[cfg_attr(miri, ignore)]
fn many_args_tail() {
    // Up and down in stack arguments on every hop.
    let chains: [&[usize]; 2] = [
        &[1, 14, 2, 9, 3, 16, 4, 12, 1, 6],
        &[1, 5, 6, 7, 8, 12, 11, 5],
    ];
    let pingpong = pingpong_module();
    each_cc(|engine| {
        let mut store = Store::new(engine, ());
        for ar in chains {
            let module = Module::new(engine, &chain_module(ar))?;
            let instance = Instance::new(&mut store, &module, &[])?;
            let f = instance.get_typed_func::<i64, i64>(&mut store, "chain")?;
            for x in XS {
                assert_eq!(
                    f.call(&mut store, x as i64)? as u64,
                    chain_expect(ar, x),
                    "{ar:?}"
                );
            }
        }

        let module = Module::new(engine, &pingpong)?;
        let instance = Instance::new(&mut store, &module, &[])?;
        let ping = instance.get_typed_func::<I12, i64>(&mut store, "ping")?;
        for (n, x) in [
            (0u64, 7u64),
            (1, 7),
            (2, 7),
            (1_000_000, 0x1234_5678_9abc_def0),
        ] {
            let mut a = [n as i64; 12];
            for j in 1..12 {
                a[j] = x.wrapping_add(j as u64) as i64;
            }
            let got = ping.call(&mut store, tuple12(a))? as u64;
            assert_eq!(got, pingpong_expect(n, x), "ping({n})");
        }
        Ok(())
    });
}

// ---------------------------------------------------------------------------
// 4: host imports with many params, and re-entry.

const HOST_SIG: [&str; 12] = [
    "i32", "i64", "f64", "i32", "i64", "f32", "i32", "i64", "f64", "i32", "i64", "i32",
];

/// The bits `mix` sees for the import arguments the wasm side derives from `x`.
fn host_bits(x: u64) -> Vec<u64> {
    HOST_SIG
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let v = x.wrapping_add(i as u64);
            match *t {
                "i32" => u64::from(v as u32),
                "i64" => v,
                "f64" => (v as i64 as f64).to_bits(),
                "f32" => u64::from((v as i64 as f32).to_bits()),
                _ => unreachable!(),
            }
        })
        .collect()
}

fn host_module() -> String {
    let mut args = String::new();
    for (i, t) in HOST_SIG.iter().enumerate() {
        let cvt = match *t {
            "i32" => "i32.wrap_i64",
            "i64" => "",
            "f64" => "f64.convert_i64_s",
            "f32" => "f32.convert_i64_s",
            _ => unreachable!(),
        };
        args += &format!("local.get 0 i64.const {i} i64.add {cvt}\n");
    }
    format!(
        r#"(module
  (import "host" "many" (func $many (param {sig}) (result i64)))
  (import "host" "reenter" (func $reenter (param i64) (result i64)))
  (func $m12 (export "m12") {p12} (result i64) (local i64)
    {m12})
  {call_many}
  {call_reenter}
)"#,
        sig = HOST_SIG.join(" "),
        p12 = params(&i64s(12)),
        m12 = mix_body(&i64s(12), 0),
        call_many = driver("(export \"call_many\")", &[], &format!("{args} call $many")),
        call_reenter = driver(
            "(export \"call_reenter\")",
            &[],
            "local.get 0 call $reenter"
        ),
    )
}

#[test]
#[cfg_attr(miri, ignore)]
fn host_imports_many_args() {
    let wat = host_module();
    each_cc(|engine| {
        let module = Module::new(engine, &wat)?;
        let mut store = Store::new(engine, ());
        let many = Func::wrap(
            &mut store,
            |a0: i32,
             a1: i64,
             a2: f64,
             a3: i32,
             a4: i64,
             a5: f32,
             a6: i32,
             a7: i64,
             a8: f64,
             a9: i32,
             a10: i64,
             a11: i32|
             -> i64 {
                mix([
                    u64::from(a0 as u32),
                    a1 as u64,
                    a2.to_bits(),
                    u64::from(a3 as u32),
                    a4 as u64,
                    u64::from(a5.to_bits()),
                    u64::from(a6 as u32),
                    a7 as u64,
                    a8.to_bits(),
                    u64::from(a9 as u32),
                    a10 as u64,
                    u64::from(a11 as u32),
                ]) as i64
            },
        );
        let reenter = Func::wrap(
            &mut store,
            |mut caller: Caller<'_, ()>, x: i64| -> Result<i64> {
                let m12 = caller.get_export("m12").unwrap().into_func().unwrap();
                let m12 = m12.typed::<I12, i64>(&caller)?;
                let mut a = [0i64; 12];
                for (j, v) in a.iter_mut().enumerate() {
                    *v = x.wrapping_add(j as i64);
                }
                m12.call(&mut caller, tuple12(a))
            },
        );
        let instance = Instance::new(&mut store, &module, &[many.into(), reenter.into()])?;
        let call_many = instance.get_typed_func::<i64, i64>(&mut store, "call_many")?;
        let call_reenter = instance.get_typed_func::<i64, i64>(&mut store, "call_reenter")?;
        for x in XS {
            assert_eq!(
                call_many.call(&mut store, x as i64)? as u64,
                mix(host_bits(x)) ^ live_mix(x)
            );
            let m12 = mix((0..12).map(|j| x.wrapping_add(j)));
            assert_eq!(
                call_reenter.call(&mut store, x as i64)? as u64,
                m12 ^ live_mix(x)
            );
        }
        Ok(())
    });
}

// ---------------------------------------------------------------------------
// 5: calls across modules in one store.

#[test]
#[cfg_attr(miri, ignore)]
fn cross_module() {
    let all = sigs();
    let tys = &all[12];
    let p = params(tys);
    let a_wat = format!(
        "(module (func (export \"m12\") {p} (result i64) (local i64)\n{}))",
        mix_body(tys, 0)
    );
    let args = args_wat(&args_for(tys, seed(12)));
    let b_wat = format!(
        r#"(module
  (type $t (func {p} (result i64)))
  (import "a" "m12" (func $imp (type $t)))
  (table $tab 2 funcref)
  (elem (table $tab) (i32.const 0) func $imp)
  {direct}
  {via_table}
  {via_param}
)"#,
        direct = driver("(export \"direct\")", &[], &format!("{args} call $imp")),
        via_table = driver(
            "(export \"via_table\")",
            &[],
            &format!("{args} i32.const 0 call_indirect $tab (type $t)")
        ),
        via_param = driver(
            "(export \"via_param\")",
            &["funcref"],
            &format!(
                "i32.const 1 local.get 1 table.set $tab\n\
                 {args} i32.const 1 call_indirect $tab (type $t)"
            )
        ),
    );
    let w = want(12, tys);
    each_cc(|engine| {
        let a = Module::new(engine, &a_wat)?;
        let b = Module::new(engine, &b_wat)?;
        let mut store = Store::new(engine, ());
        let ia = Instance::new(&mut store, &a, &[])?;
        let m12 = ia.get_func(&mut store, "m12").unwrap();
        let ib = Instance::new(&mut store, &b, &[m12.into()])?;
        let direct = ib.get_typed_func::<i64, i64>(&mut store, "direct")?;
        let via_table = ib.get_typed_func::<i64, i64>(&mut store, "via_table")?;
        let via_param = ib.get_typed_func::<(i64, Option<Func>), i64>(&mut store, "via_param")?;
        for x in XS {
            let e = w ^ live_mix(x);
            assert_eq!(direct.call(&mut store, x as i64)? as u64, e);
            assert_eq!(via_table.call(&mut store, x as i64)? as u64, e);
            assert_eq!(via_param.call(&mut store, (x as i64, Some(m12)))? as u64, e);
        }
        Ok(())
    });
}

// ---------------------------------------------------------------------------
// 6 and 7: traps, backtraces and stack overflow.

fn trap_module() -> String {
    let mut tys = vec![Ty::I32];
    tys.extend(i64s(9));
    let p10 = params(&tys);
    let mut f0_call = String::from("local.get 1\n");
    for j in 1..10 {
        f0_call += &format!("local.get 0 i64.const {j} i64.add\n");
    }
    f0_call += "call $f1";
    let mut f1 = String::from("local.get 0\n");
    for j in 1..10 {
        f1 += &format!("local.get {j} i64.const 3 i64.mul\n");
    }
    f1 += "call $f2 i64.const 1 i64.add";
    format!(
        "(module
  (func $f2 {p10} (result i64) (local i64)
    local.get 0 if unreachable end
    {mix})
  (func $f1 {p10} (result i64)
    {f1})
  {f0}
)",
        mix = mix_body(&tys, 1),
        f0 = driver("$f0 (export \"f0\")", &["i32"], &f0_call),
    )
}

fn trap_expect(x: u64) -> u64 {
    mix((1..10).map(|j| x.wrapping_add(j).wrapping_mul(3))).wrapping_add(1) ^ live_mix(x)
}

#[test]
#[cfg_attr(miri, ignore)]
fn traps_and_backtrace() {
    let wat = trap_module();
    each_cc_with(
        |c| {
            c.compiler_inlining(Inlining::No);
        },
        |engine| {
            let module = Module::new(engine, &wat)?;
            let mut store = Store::new(engine, ());
            let instance = Instance::new(&mut store, &module, &[])?;
            let f0 = instance.get_typed_func::<(i64, i32), i64>(&mut store, "f0")?;
            for x in XS {
                let err = f0.call(&mut store, (x as i64, 1)).unwrap_err();
                let names: Vec<Option<String>> = err
                    .downcast_ref::<WasmBacktrace>()
                    .expect("error has a WasmBacktrace")
                    .frames()
                    .iter()
                    .map(|f| f.func_name().map(str::to_string))
                    .collect();
                assert_eq!(err.downcast::<Trap>()?, Trap::UnreachableCodeReached);
                assert!(names.len() >= 3, "{names:?}");
                assert_eq!(names[0].as_deref(), Some("f2"), "{names:?}");
                assert_eq!(names[1].as_deref(), Some("f1"), "{names:?}");
                assert_eq!(names[2].as_deref(), Some("f0"), "{names:?}");
                // The store still works after the trap.
                assert_eq!(f0.call(&mut store, (x as i64, 0))? as u64, trap_expect(x));
            }
            Ok(())
        },
    );
}

#[test]
#[cfg_attr(miri, ignore)]
fn stack_overflow_many_args() {
    let p12 = params(&i64s(12));
    let rest: String = (1..12).map(|j| format!("local.get {j} ")).collect();
    let wat = format!(
        "(module
  (func $r (export \"r\") {p12} (result i64)
    local.get 0 i64.const 1 i64.add {rest} call $r local.get 11 i64.add)
  (func (export \"ok\") {p12} (result i64) (local i64)
    {mix})
)",
        mix = mix_body(&i64s(12), 0)
    );
    each_cc(|engine| {
        let module = Module::new(engine, &wat)?;
        let mut store = Store::new(engine, ());
        let instance = Instance::new(&mut store, &module, &[])?;
        let r = instance.get_typed_func::<I12, i64>(&mut store, "r")?;
        let ok = instance.get_typed_func::<I12, i64>(&mut store, "ok")?;
        let a: [i64; 12] = core::array::from_fn(|j| 100 + j as i64);
        for _ in 0..2 {
            let err = r.call(&mut store, tuple12(a)).unwrap_err();
            assert_eq!(err.downcast::<Trap>()?, Trap::StackOverflow);
            let got = ok.call(&mut store, tuple12(a))? as u64;
            assert_eq!(got, mix(a.iter().map(|&v| v as u64)));
        }
        Ok(())
    });
}

// ---------------------------------------------------------------------------
// 8: compiled artifacts carry the setting.

#[test]
#[cfg_attr(miri, ignore)]
fn artifacts_do_not_mix() -> Result<()> {
    let p12 = params(&i64s(12));
    let wat = format!(
        "(module (func (export \"f\") {p12} (result i64) (local i64)\n{}))",
        mix_body(&i64s(12), 0)
    );
    let a12: [i64; 12] = core::array::from_fn(|j| 7 * j as i64 - 3);
    let ccs: Vec<IxWasmCc> = ALL.into_iter().filter(|&c| supported(c)).collect();
    for &a in &ccs {
        let ea = Engine::new(&config(a))?;
        let serialized = Module::new(&ea, &wat)?.serialize()?;
        let precompiled = ea.precompile_module(wat.as_bytes())?;
        for &b in &ccs {
            let eb = Engine::new(&config(b))?;
            for bytes in [&serialized, &precompiled] {
                // SAFETY: the bytes were produced by `serialize` or
                // `precompile_module` in this test.
                let r = unsafe { Module::deserialize(&eb, bytes) };
                if a == b {
                    let m = r?;
                    let mut store = Store::new(&eb, ());
                    let i = Instance::new(&mut store, &m, &[])?;
                    let f = i.get_typed_func::<I12, i64>(&mut store, "f")?;
                    let got = f.call(&mut store, tuple12(a12))? as u64;
                    assert_eq!(got, mix(a12.iter().map(|&v| v as u64)));
                    continue;
                }
                let e = match r {
                    Ok(_) => panic!("module compiled with {a} loaded into a {b} engine"),
                    Err(e) => format!("{e:?}"),
                };
                // The Cranelift `tail_arg_regs` shared flag is compared before
                // the tunables, so the refusal may name either one.
                assert!(
                    e.contains("ix wasm calling convention") || e.contains("tail_arg_regs"),
                    "{a} -> {b}: {e}"
                );
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 9: the host's callee-saved registers survive calls into wasm, including
// calls that trap.
//
// Soundness: the asm block declares every register the host ABI lets a callee
// clobber as clobbered (`clobber_abi("C")`) and every callee-saved register it
// checks as an `inout` operand, so the compiler keeps nothing of its own in
// them across the block. The block calls a plain `extern "C"` function with a
// pointer to a live closure, which is an ordinary C-ABI call; the stack is
// 16-byte aligned at block entry (no `nostack`) and stays aligned at the call.
// rbx (x86_64) and x19 (aarch64) are reserved by LLVM and cannot be operands,
// so the block saves them on the stack, sets them, and restores them itself,
// copying the value seen after the call into an output register first. The
// frame pointer (rbp/x29) is not checked. Windows is excluded because its x64
// ABI has a different callee-saved set and argument register.

#[cfg(all(any(target_arch = "x86_64", target_arch = "aarch64"), not(windows)))]
mod callee_saved {
    use core::ffi::c_void;

    extern "C" fn tramp(ctx: *mut c_void) {
        // SAFETY: `ctx` is the `&mut &mut dyn FnMut()` that `with_magic`
        // passes, alive for the whole asm block.
        let f = unsafe { &mut *ctx.cast::<&mut dyn FnMut()>() };
        f();
    }

    fn magic(seed: u64, i: u64) -> u64 {
        0xA5A5_0000_0000_0000 ^ (i << 40) ^ seed.wrapping_mul(0x9E37_79B9)
    }

    /// Loads magic values into the callee-saved registers, runs `f` from an
    /// `extern "C"` call, and returns (expected, found) register values.
    #[cfg(target_arch = "x86_64")]
    pub fn with_magic(seed: u64, mut f: &mut dyn FnMut()) -> (Vec<u64>, Vec<u64>) {
        let ctx = (&mut f as *mut &mut dyn FnMut()).cast::<c_void>();
        let want: Vec<u64> = (0..5).map(|i| magic(seed, i)).collect();
        let (mut r12, mut r13, mut r14, mut r15) = (want[1], want[2], want[3], want[4]);
        let rbx: u64;
        // SAFETY: see the comment above this module.
        unsafe {
            core::arch::asm!(
                "push rbx",
                "sub rsp, 8",
                "mov rbx, {m}",
                "call {f}",
                "mov rax, rbx",
                "add rsp, 8",
                "pop rbx",
                m = in(reg) want[0],
                f = in(reg) tramp as extern "C" fn(*mut c_void),
                in("rdi") ctx,
                inout("r12") r12,
                inout("r13") r13,
                inout("r14") r14,
                inout("r15") r15,
                lateout("rax") rbx,
                clobber_abi("C"),
            );
        }
        (want, vec![rbx, r12, r13, r14, r15])
    }

    #[cfg(target_arch = "aarch64")]
    pub fn with_magic(seed: u64, mut f: &mut dyn FnMut()) -> (Vec<u64>, Vec<u64>) {
        let ctx = (&mut f as *mut &mut dyn FnMut()).cast::<c_void>();
        let want: Vec<u64> = (0..10).map(|i| magic(seed, i)).collect();
        let (mut x20, mut x21, mut x22, mut x23, mut x24) =
            (want[1], want[2], want[3], want[4], want[5]);
        let (mut x25, mut x26, mut x27, mut x28) = (want[6], want[7], want[8], want[9]);
        let x19: u64;
        // SAFETY: see the comment above this module.
        unsafe {
            core::arch::asm!(
                "str x19, [sp, #-16]!",
                "mov x19, {m}",
                "blr {f}",
                "mov x9, x19",
                "ldr x19, [sp], #16",
                m = in(reg) want[0],
                f = in(reg) tramp as extern "C" fn(*mut c_void),
                in("x0") ctx,
                inout("x20") x20,
                inout("x21") x21,
                inout("x22") x22,
                inout("x23") x23,
                inout("x24") x24,
                inout("x25") x25,
                inout("x26") x26,
                inout("x27") x27,
                inout("x28") x28,
                lateout("x9") x19,
                clobber_abi("C"),
            );
        }
        (want, vec![x19, x20, x21, x22, x23, x24, x25, x26, x27, x28])
    }
}

#[cfg(all(any(target_arch = "x86_64", target_arch = "aarch64"), not(windows)))]
#[test]
#[cfg_attr(miri, ignore)]
fn host_callee_saved_preserved() {
    let mut tys = vec![Ty::I32];
    tys.extend(i64s(14));
    let mut call = String::from("local.get 1\n");
    for j in 0..14 {
        call += &format!("local.get 0 i64.const {j} i64.add\n");
    }
    call += "call $m15";
    let wat = format!(
        "(module
  (func $m15 {p} (result i64) (local i64)
    local.get 0 if unreachable end
    {mix})
  {busy}
)",
        p = params(&tys),
        mix = mix_body(&tys, 1),
        busy = driver("(export \"busy\")", &["i32"], &call),
    );
    each_cc(|engine| {
        let module = Module::new(engine, &wat)?;
        let mut store = Store::new(engine, ());
        let instance = Instance::new(&mut store, &module, &[])?;
        let busy = instance.get_typed_func::<(i64, i32), i64>(&mut store, "busy")?;
        for iter in 0..200u64 {
            let trap = iter % 2 == 1;
            let x = iter.wrapping_mul(0x9E37_79B9_7F4A_7C15);
            let mut res = None;
            let (want, got) = callee_saved::with_magic(iter, &mut || {
                res = Some(busy.call(&mut store, (x as i64, i32::from(trap))));
            });
            assert_eq!(
                want, got,
                "callee-saved registers clobbered (trap = {trap})"
            );
            match res.expect("closure ran") {
                Ok(v) => {
                    assert!(!trap);
                    assert_eq!(
                        v as u64,
                        mix((0..14).map(|j| x.wrapping_add(j))) ^ live_mix(x)
                    );
                }
                Err(e) => {
                    assert!(trap, "{e:?}");
                    assert_eq!(e.downcast::<Trap>()?, Trap::UnreachableCodeReached);
                }
            }
        }
        Ok(())
    });
}

// ---------------------------------------------------------------------------
// 10: configuration.

fn assert_refused(c: &Config, what: &str, standard_ok: bool) {
    match Engine::new(c) {
        Ok(_) => panic!("{what}: engine accepted a non-standard ix_wasm_cc"),
        // If the standard setting builds, the refusal must be ours.
        Err(e) if standard_ok => {
            let e = format!("{e:?}");
            assert!(e.contains("ix_wasm_cc"), "{what}: {e}");
        }
        Err(_) => {}
    }
}

#[test]
#[cfg_attr(miri, ignore)]
fn engine_refuses_unsupported() -> Result<()> {
    for (s, cc) in [
        ("standard", IxWasmCc::Standard),
        ("wide", IxWasmCc::Wide),
        ("wide_trade", IxWasmCc::WideTrade),
    ] {
        assert_eq!(s.parse::<IxWasmCc>(), Ok(cc));
        assert_eq!(cc.to_string(), s);
    }
    assert!("bogus".parse::<IxWasmCc>().is_err());
    assert_eq!(IxWasmCc::default(), IxWasmCc::Standard);
    assert_eq!(Engine::default().get_ix_wasm_cc(), IxWasmCc::Standard);

    let pulley = if cfg!(target_pointer_width = "64") {
        "pulley64"
    } else {
        "pulley32"
    };
    for cc in [IxWasmCc::Wide, IxWasmCc::WideTrade] {
        let mut c = Config::new();
        c.strategy(Strategy::Winch);
        let ok = Engine::new(&c).is_ok();
        c.ix_wasm_cc(cc);
        assert_refused(&c, "winch", ok);

        for target in [
            pulley,
            "s390x-unknown-linux-gnu",
            "riscv64gc-unknown-linux-gnu",
        ] {
            let mut c = Config::new();
            if c.target(target).is_err() {
                continue;
            }
            let ok = Engine::new(&c).is_ok();
            c.ix_wasm_cc(cc);
            assert_refused(&c, target, ok);
        }

        // Cross-compiling to a supported architecture works.
        for target in ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"] {
            let mut c = Config::new();
            if c.target(target).is_err() || Engine::new(&c).is_err() {
                continue;
            }
            c.ix_wasm_cc(cc);
            let e = Engine::new(&c)?;
            assert_eq!(e.get_ix_wasm_cc(), cc);
            e.precompile_module(b"(module (func (param i64 i64 i64 i64 i64 i64 i64 i64)))")?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 11: exceptions thrown from a many-param callee.

fn exn_module() -> String {
    let mut args = String::new();
    for j in 0..10 {
        args += &format!("local.get 0 i64.const {j} i64.add\n");
    }
    let call = format!(
        "block $h (result i32 i64 f64 i64 i32 i64)
      try_table (catch $e $h)
        {args} call $thrower drop
      end
      unreachable
    end
    call $fold6"
    );
    let fold = [Ty::I32, Ty::I64, Ty::F64, Ty::I64, Ty::I32, Ty::I64];
    format!(
        "(module
  (tag $e (param i32 i64 f64 i64 i32 i64))
  (func $thrower {p10} (result i64)
    local.get 0 local.get 9 i64.xor i32.wrap_i64
    local.get 1 local.get 8 i64.add
    local.get 2 f64.convert_i64_s
    local.get 3 local.get 7 i64.mul
    local.get 4 i32.wrap_i64
    local.get 5 local.get 6 i64.xor
    throw $e)
  (func $fold6 {pf} (result i64) (local i64)
    {mix})
  {catch}
)",
        p10 = params(&i64s(10)),
        pf = params(&fold),
        mix = mix_body(&fold, 0),
        catch = driver("(export \"catch\")", &[], &call),
    )
}

fn exn_expect(x: u64) -> u64 {
    let p = |j: u64| x.wrapping_add(j);
    mix([
        u64::from((p(0) ^ p(9)) as u32),
        p(1).wrapping_add(p(8)),
        (p(2) as i64 as f64).to_bits(),
        p(3).wrapping_mul(p(7)),
        u64::from(p(4) as u32),
        p(5) ^ p(6),
    ]) ^ live_mix(x)
}

#[test]
#[cfg_attr(miri, ignore)]
fn exceptions_many_args() {
    let wat = exn_module();
    each_cc_with(
        |c| {
            c.wasm_exceptions(true);
        },
        |engine| {
            let module = Module::new(engine, &wat)?;
            let mut store = Store::new(engine, ());
            let instance = Instance::new(&mut store, &module, &[])?;
            let f = instance.get_typed_func::<i64, i64>(&mut store, "catch")?;
            for x in XS {
                assert_eq!(f.call(&mut store, x as i64)? as u64, exn_expect(x));
            }
            Ok(())
        },
    );
}
