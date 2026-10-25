//! ix bulk memory: inline `memory.copy`/`memory.fill` (crates/cranelift/src/
//! func_environ/ix_bulk.rs) checked against a byte-array model in every mode.

use std::time::{Duration, Instant};
use wasmtime::*;

const PAGES: u32 = 80; // 5 MiB: room for multi-chunk (> 2 MiB) ops.
const KS: [u32; 8] = [65, 128, 129, 200, 1000, 4096, 65536, (2 << 20) + 3];

fn wat(shared: bool) -> String {
    let mem = if shared {
        format!("(memory $m0 (export \"m0\") {PAGES} {PAGES} shared)")
    } else {
        format!("(memory $m0 (export \"m0\") {PAGES})")
    };
    let mut s = format!(
        r#"(module
          {mem}
          (memory $m1 (export "m1") 1)
          (func (export "copy") (param i32 i32 i32)
            local.get 0 local.get 1 local.get 2 memory.copy $m0 $m0)
          (func (export "fill") (param i32 i32 i32)
            local.get 0 local.get 1 local.get 2 memory.fill $m0)
          (func (export "copy10") (param i32 i32 i32)
            local.get 0 local.get 1 local.get 2 memory.copy $m1 $m0)
        "#
    );
    for k in KS {
        s.push_str(&format!(
            "(func (export \"copy_{k}\") (param i32 i32) local.get 0 local.get 1 i32.const {k} memory.copy $m0 $m0)
             (func (export \"fill_{k}\") (param i32 i32) local.get 0 local.get 1 i32.const {k} memory.fill $m0)\n"
        ));
    }
    s.push(')');
    s
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
}

#[derive(Clone, Copy, Debug)]
enum Op {
    Copy(u32, u32, u32),
    Fill(u32, u32, u32),
    /// memory 0 to memory 1.
    Copy10(u32, u32, u32),
    CopyK(u32, u32, u32),
    FillK(u32, u32, u32),
}

/// Model: returns false when the op traps (and then changes nothing).
fn model(m0: &mut [u8], m1: &mut [u8], op: Op) -> bool {
    let fits = |a: u32, n: u32, len: usize| u64::from(a) + u64::from(n) <= len as u64;
    match op {
        Op::Copy(d, s, n) | Op::CopyK(d, s, n) => {
            if !fits(d, n, m0.len()) || !fits(s, n, m0.len()) {
                return false;
            }
            m0.copy_within(s as usize..(s + n) as usize, d as usize);
        }
        Op::Fill(d, v, n) | Op::FillK(d, v, n) => {
            if !fits(d, n, m0.len()) {
                return false;
            }
            m0[d as usize..(d + n) as usize].fill(v as u8);
        }
        Op::Copy10(d, s, n) => {
            if !fits(d, n, m1.len()) || !fits(s, n, m0.len()) {
                return false;
            }
            m1[d as usize..(d + n) as usize].copy_from_slice(&m0[s as usize..(s + n) as usize]);
        }
    }
    true
}

fn ops() -> Vec<Op> {
    let len = PAGES * 65536;
    let mut r = Rng(0x9e37_79b9_7f4a_7c15);
    let mut ops = Vec::new();
    let mut sizes: Vec<u32> = (0..=300).collect();
    for b in [511, 512, 513, 1023, 1024, 2047, 2048, 2049, 4095, 4096, 65535, 65536, 65537] {
        sizes.push(b);
    }
    let chunk = 1u32 << 20;
    for b in [chunk - 1, chunk, chunk + 1, 2 * chunk - 1, 2 * chunk, 2 * chunk + 1, 3 * chunk + 77] {
        sizes.push(b);
    }
    for &n in &sizes {
        for delta in [-130i64, -65, -64, -63, -17, -1, 0, 1, 15, 16, 63, 64, 65, 129, 4096] {
            let base = r.below(u64::from(len - n.min(len)) + 1) as i64;
            let s = base;
            let d = s + delta;
            if d < 0 || d + i64::from(n) > i64::from(len) || s + i64::from(n) > i64::from(len) {
                continue;
            }
            ops.push(Op::Copy(d as u32, s as u32, n));
        }
        // Far apart, random.
        let max = u64::from(len - n.min(len));
        ops.push(Op::Copy(r.below(max + 1) as u32, r.below(max + 1) as u32, n));
        ops.push(Op::Fill(r.below(max + 1) as u32, r.next() as u32, n));
        if n <= 65536 {
            ops.push(Op::Copy10(r.below(u64::from(65536 - n) + 1) as u32, r.below(max + 1) as u32, n));
        }
    }
    // Bounds edges: last valid start, one past, end-of-memory zero length,
    // wrapping sums, huge lengths.
    for n in [0u32, 1, 2, 8, 16, 17, 64, 128, 129, 4096, 3 << 20] {
        let n = n.min(len);
        for (d, s) in [
            (len - n, 0),
            (0, len - n),
            (len - n + 1, 0),
            (0, len - n + 1),
            (len, len),
            (len + 1, 0),
            (0, len + 1),
            (u32::MAX, 0),
            (0, u32::MAX),
            (u32::MAX - n + 1, 0),
        ] {
            ops.push(Op::Copy(d, s, n));
            ops.push(Op::Fill(d, 0xab, n));
            ops.push(Op::Copy10(d.min(u32::MAX), s, n));
        }
    }
    for n in [u32::MAX, u32::MAX - 1, len + 1, 1 << 31] {
        ops.push(Op::Copy(0, 0, n));
        ops.push(Op::Copy(1, 0, n));
        ops.push(Op::Fill(0, 1, n));
        ops.push(Op::Fill(u32::MAX, 1, n));
    }
    for k in KS {
        for (d, s) in [(0, 1), (1, 0), (7, 300), (300, 7), (len - k, 0), (len - k + 1, 0), (0, len - k + 1)] {
            ops.push(Op::CopyK(d, s, k));
            ops.push(Op::FillK(d, s, k));
        }
    }
    ops
}

fn run(mode: u8, rep_min: u32, shared: bool, poll: bool) -> Result<()> {
    let mut config = Config::new();
    config.ix_bulk_inline(mode);
    config.ix_bulk_rep_min(rep_min);
    if poll {
        config.poll_interruption(true);
    } else {
        config.epoch_interruption(true);
    }
    if shared {
        config.wasm_threads(true);
        config.shared_memory(true);
    }
    let engine = Engine::new(&config)?;
    let module = Module::new(&engine, wat(shared))?;
    let mut store = Store::new(&engine, ());
    if !poll {
        store.set_epoch_deadline(u64::MAX / 2);
    }
    let instance = Instance::new(&mut store, &module, &[])?;
    let copy = instance.get_typed_func::<(u32, u32, u32), ()>(&mut store, "copy")?;
    let fill = instance.get_typed_func::<(u32, u32, u32), ()>(&mut store, "fill")?;
    let copy10 = instance.get_typed_func::<(u32, u32, u32), ()>(&mut store, "copy10")?;
    let m1 = instance.get_memory(&mut store, "m1").unwrap();

    let read_m0 = |store: &mut Store<()>| -> Vec<u8> {
        if shared {
            let m = instance.get_shared_memory(&mut *store, "m0").unwrap();
            m.data().iter().map(|c| unsafe { *c.get() }).collect()
        } else {
            instance.get_memory(&mut *store, "m0").unwrap().data(&*store).to_vec()
        }
    };

    // Seed both memories with a pattern.
    let mut r = Rng(12345);
    let seed0: Vec<u8> = (0..PAGES * 65536).map(|_| r.next() as u8).collect();
    let seed1: Vec<u8> = (0..65536).map(|_| r.next() as u8).collect();
    if shared {
        let m = instance.get_shared_memory(&mut store, "m0").unwrap();
        for (c, v) in m.data().iter().zip(&seed0) {
            unsafe { *c.get() = *v };
        }
    } else {
        instance.get_memory(&mut store, "m0").unwrap().data_mut(&mut store).copy_from_slice(&seed0);
    }
    m1.data_mut(&mut store).copy_from_slice(&seed1);
    let mut want0 = seed0;
    let mut want1 = seed1;

    for (i, op) in ops().into_iter().enumerate() {
        let ok = model(&mut want0, &mut want1, op);
        let got = match op {
            Op::Copy(d, s, n) => copy.call(&mut store, (d, s, n)),
            Op::Fill(d, v, n) => fill.call(&mut store, (d, v, n)),
            Op::Copy10(d, s, n) => copy10.call(&mut store, (d, s, n)),
            Op::CopyK(d, s, k) => instance
                .get_typed_func::<(u32, u32), ()>(&mut store, &format!("copy_{k}"))?
                .call(&mut store, (d, s)),
            Op::FillK(d, v, k) => instance
                .get_typed_func::<(u32, u32), ()>(&mut store, &format!("fill_{k}"))?
                .call(&mut store, (d, v)),
        };
        match (ok, got) {
            (true, Ok(())) => {}
            (false, Err(e)) => {
                assert_eq!(
                    e.downcast_ref::<Trap>(),
                    Some(&Trap::MemoryOutOfBounds),
                    "mode {mode} op {i} {op:?}: {e:?}"
                );
            }
            (ok, got) => panic!("mode {mode} op {i} {op:?}: model ok={ok}, wasm {got:?}"),
        }
        let got0 = read_m0(&mut store);
        if got0 != want0 {
            let at = got0.iter().zip(&want0).position(|(a, b)| a != b).unwrap();
            panic!(
                "mode {mode} rep_min {rep_min} shared {shared} op {i} {op:?}: memory 0 differs at {at}: got {} want {}",
                got0[at], want0[at]
            );
        }
        assert!(m1.data(&store) == &want1[..], "mode {mode} op {i} {op:?}: memory 1 differs");
    }
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore)]
fn ix_bulk_matches_model() -> Result<()> {
    for (mode, rep_min) in [(0, 2048), (1, 2048), (2, 2048), (2, 65), (2, 0), (3, 2048), (3, 65), (4, 2048)] {
        run(mode, rep_min, false, false)?;
    }
    // Poll interruption: the stepped loop's poll check (ops over 2 MiB).
    for mode in [1, 3, 4] {
        run(mode, 2048, false, true)?;
    }
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore)]
fn ix_bulk_shared_memory() -> Result<()> {
    for (mode, rep_min) in [(0, 2048), (1, 2048), (2, 65), (3, 65), (4, 65)] {
        run(mode, rep_min, true, false)?;
    }
    Ok(())
}

/// memory64 keeps the libcall path; check it still works.
#[test]
#[cfg_attr(miri, ignore)]
fn ix_bulk_memory64_fallback() -> Result<()> {
    let mut config = Config::new();
    config.wasm_memory64(true);
    let engine = Engine::new(&config)?;
    let module = Module::new(
        &engine,
        r#"(module (memory (export "m") i64 1)
             (func (export "copy") (param i64 i64 i64)
               local.get 0 local.get 1 local.get 2 memory.copy)
             (func (export "fill") (param i64 i32 i64)
               local.get 0 local.get 1 local.get 2 memory.fill))"#,
    )?;
    let mut store = Store::new(&engine, ());
    let i = Instance::new(&mut store, &module, &[])?;
    let fill = i.get_typed_func::<(u64, u32, u64), ()>(&mut store, "fill")?;
    let copy = i.get_typed_func::<(u64, u64, u64), ()>(&mut store, "copy")?;
    fill.call(&mut store, (10, 7, 300))?;
    copy.call(&mut store, (1000, 5, 400))?;
    let m = i.get_memory(&mut store, "m").unwrap();
    assert_eq!(m.data(&store)[1005], 7);
    assert_eq!(m.data(&store)[1004], 0);
    assert!(copy.call(&mut store, (u64::MAX, 0, 2)).is_err());
    Ok(())
}

/// A 3 GiB fill is interrupted after 5 ms (an epoch bump, or with `poll` a
/// poll-word interrupt) in the inline modes, and only finishes (then traps)
/// in mode 0. Returns (trapped, elapsed, last byte).
fn long_fill(mode: u8, poll: bool) -> Result<(bool, Duration, u8)> {
    let mut config = Config::new();
    config.ix_bulk_inline(mode);
    if poll {
        config.poll_interruption(true);
    } else {
        config.epoch_interruption(true);
    }
    let engine = Engine::new(&config)?;
    let module = Module::new(
        &engine,
        r#"(module (memory (export "m") 49152)
             (func (export "fill") (param i32 i32 i32)
               local.get 0 local.get 1 local.get 2 memory.fill))"#,
    )?;
    let mut store = Store::new(&engine, ());
    if !poll {
        store.set_epoch_deadline(1);
    }
    let i = Instance::new(&mut store, &module, &[])?;
    let fill = i.get_typed_func::<(u32, u32, u32), ()>(&mut store, "fill")?;
    let e2 = engine.clone();
    let handle = if poll { store.interrupt_handle() } else { None };
    assert_eq!(handle.is_some(), poll);
    let t = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(5));
        match handle {
            Some(h) => h.interrupt(),
            None => e2.increment_epoch(),
        }
    });
    let start = Instant::now();
    let r = fill.call(&mut store, (0, 1, 3 << 30));
    let el = start.elapsed();
    t.join().unwrap();
    let trapped = match r {
        Ok(()) => false,
        Err(e) => {
            assert_eq!(e.downcast_ref::<Trap>(), Some(&Trap::Interrupt), "{e:?}");
            true
        }
    };
    let m = i.get_memory(&mut store, "m").unwrap();
    let last = m.data(&store)[(3 << 30) - 1];
    Ok((trapped, el, last))
}

#[test]
#[cfg_attr(miri, ignore)]
fn ix_bulk_long_fill_preempts() -> Result<()> {
    long_fill_preempts(false)
}

#[test]
#[cfg_attr(miri, ignore)]
#[cfg(unix)]
fn ix_bulk_long_fill_preempts_poll() -> Result<()> {
    long_fill_preempts(true)
}

fn long_fill_preempts(poll: bool) -> Result<()> {
    let how = if poll { "poll" } else { "epoch" };
    for mode in [1, 2, 3, 4] {
        let (trapped, el, last) = long_fill(mode, poll)?;
        println!("ix_bulk long fill {how} mode {mode}: trapped={trapped} after {el:?}, last byte {last}");
        assert!(trapped, "{how} mode {mode}");
        assert_eq!(last, 0, "{how} mode {mode}: fill ran to the end before the interrupt");
    }
    let (trapped, el, last) = long_fill(0, poll)?;
    println!("ix_bulk long fill {how} mode 0: trapped={trapped} after {el:?}, last byte {last}");
    Ok(())
}

#[test]
fn ix_bulk_rejects_unknown_mode() {
    let mut config = Config::new();
    config.ix_bulk_inline(5);
    assert!(Engine::new(&config).is_err());
}
