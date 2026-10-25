//! ix fork: `memory.copy` / `memory.fill` of 2 MiB or more run in 1 MiB
//! chunks inside the libcall, with a poll / epoch check between chunks, so
//! a multi-GiB op is preemptible without any change at the compiled site.
#![cfg(all(unix, not(miri)))]

use std::time::{Duration, Instant};
use wasmtime::*;

const MIB: u64 = 1 << 20;
const GIB: u64 = 1 << 30;

fn trap(e: &Error) -> Trap {
    *e.downcast_ref::<Trap>()
        .unwrap_or_else(|| panic!("not a trap: {e:?}"))
}

#[derive(Clone, Copy, Debug)]
enum Mode {
    Poll,
    Epoch,
    /// Epoch interruption with Winch, the other compiler that calls these
    /// libcalls (poll interruption does not support Winch).
    EpochWinch,
}

fn engine(mode: Mode) -> Engine {
    let mut c = Config::new();
    match mode {
        Mode::Poll => {
            c.poll_interruption(true);
        }
        Mode::Epoch => {
            c.epoch_interruption(true);
        }
        Mode::EpochWinch => {
            c.epoch_interruption(true);
            c.strategy(Strategy::Winch);
        }
    }
    if !matches!(mode, Mode::EpochWinch) {
        c.wasm_threads(true);
        c.shared_memory(true);
    }
    Engine::new(&c).unwrap()
}

/// A 4 GiB memory (all of memory32) with `fill(dst, val, len)` and
/// `copy(dst, src, len)` exports.
const BULK: &str = r#"
(module
  (memory (export "mem") 65536)
  (func (export "fill") (param i32 i32 i32)
    (memory.fill (local.get 0) (local.get 1) (local.get 2)))
  (func (export "copy") (param i32 i32 i32)
    (memory.copy (local.get 0) (local.get 1) (local.get 2))))
"#;

/// One interrupted 3 GiB op: what it trapped with, how long it took to stop
/// after the interrupt, and whether it stopped part-way.
struct Outcome {
    trap: Trap,
    stopped: Duration,
    midway: bool,
}

/// Runs `op(dst, a, len)` and fires the store's interrupt (poll) or bumps the
/// epoch (epoch) 5 ms after the call starts. Marker bytes show whether the op
/// stopped part-way (so the trap came from inside the libcall, not from a
/// compiled check before it). Then checks the store still runs wasm,
/// including a chunked op, after the unwind out of the libcall.
fn interrupted(mode: Mode, op: &str, dst: u64, a: u64, len: u64) -> Outcome {
    let engine = engine(mode);
    let module = Module::new(&engine, BULK).unwrap();
    let mut store = Store::new(&engine, ());
    if let Mode::Epoch | Mode::EpochWinch = mode {
        store.set_epoch_deadline(1);
    }
    let i = Instance::new(&mut store, &module, &[]).unwrap();
    let f = i
        .get_typed_func::<(u32, u32, u32), ()>(&mut store, op)
        .unwrap();
    let mem = i.get_memory(&mut store, "mem").unwrap();
    // `first`: the byte the op writes first, `last`: the byte it writes last,
    // with the value each gets. Copies get nonzero source markers.
    let (first, last, want) = match op {
        "fill" => (dst, dst + len - 1, (a as u8, a as u8)),
        _ => {
            let m = mem.data_mut(&mut store);
            m[a as usize] = 1;
            m[(a + len - 1) as usize] = 2;
            if dst > a && dst < a + len {
                // Back to front: the last source byte is copied first.
                (dst + len - 1, dst, (2, 1))
            } else {
                (dst, dst + len - 1, (1, 2))
            }
        }
    };
    let fire: Box<dyn FnOnce() + Send> = match mode {
        Mode::Poll => {
            let h = store.interrupt_handle().unwrap();
            Box::new(move || h.interrupt())
        }
        Mode::Epoch | Mode::EpochWinch => {
            let e = engine.clone();
            Box::new(move || e.increment_epoch())
        }
    };
    let t = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(5));
        let t = Instant::now();
        fire();
        t
    });
    let r = f.call(&mut store, (dst as u32, a as u32, len as u32));
    let end = Instant::now();
    let fired = t.join().unwrap();
    let err = r.expect_err("a 3 GiB op finished before a 5 ms interrupt");
    let m = mem.data(&store);
    let midway = m[first as usize] == want.0 && m[last as usize] != want.1;
    let out = Outcome {
        trap: trap(&err),
        stopped: end.saturating_duration_since(fired),
        midway,
    };

    // The store is usable after the unwind: rearm, then a chunked fill.
    match mode {
        Mode::Poll => assert!(store.interrupt_handle().unwrap().rearm()),
        Mode::Epoch | Mode::EpochWinch => store.set_epoch_deadline(1 << 30),
    }
    let fill = i
        .get_typed_func::<(u32, u32, u32), ()>(&mut store, "fill")
        .unwrap();
    fill.call(&mut store, (64, 0x77, (5 * MIB) as u32)).unwrap();
    let m = mem.data(&store);
    assert!(m[64..64 + (5 * MIB) as usize].iter().all(|b| *b == 0x77));
    out
}

/// The brief's bound: a trap within about 10 ms of the interrupt. Each
/// attempt must trap with `Interrupt`, part-way through the op, within 100 ms;
/// one of three must stop within 10 ms (so a descheduled test thread on a
/// loaded host does not fail it).
fn check_prompt(mode: Mode, op: &str, dst: u64, a: u64, len: u64) {
    let mut best = Duration::MAX;
    for attempt in 0..3 {
        let o = interrupted(mode, op, dst, a, len);
        eprintln!(
            "ix_bulk_preempt {mode:?} {op} dst={dst:#x} a={a:#x} len={len:#x}: {:?} midway={} {:?} after the interrupt (attempt {attempt})",
            o.trap, o.midway, o.stopped
        );
        assert_eq!(o.trap, Trap::Interrupt);
        assert!(o.midway, "the op did not stop part-way");
        assert!(o.stopped < Duration::from_millis(100), "took {:?} to stop", o.stopped);
        best = best.min(o.stopped);
        if best < Duration::from_millis(10) {
            return;
        }
    }
    panic!("best of 3 took {best:?} to stop");
}

#[test]
fn fill_3gib_poll() {
    check_prompt(Mode::Poll, "fill", 0, 0xab, 3 * GIB);
}

#[test]
fn fill_3gib_epoch() {
    check_prompt(Mode::Epoch, "fill", 0, 0xab, 3 * GIB);
}

#[test]
fn copy_3gib_forward_poll() {
    check_prompt(Mode::Poll, "copy", 0, GIB, 3 * GIB);
}

#[test]
fn copy_3gib_forward_epoch() {
    check_prompt(Mode::Epoch, "copy", 0, GIB, 3 * GIB);
}

#[test]
#[cfg(target_arch = "x86_64")]
fn fill_3gib_epoch_winch() {
    check_prompt(Mode::EpochWinch, "fill", 0, 0xab, 3 * GIB);
}

#[test]
#[cfg(target_arch = "x86_64")]
fn copy_3gib_backward_epoch_winch() {
    check_prompt(Mode::EpochWinch, "copy", GIB, 0, 3 * GIB);
}

// Non-overlapping: the 64 MiB far-copy chunks on x86_64.
#[test]
fn copy_2gib_far_poll() {
    check_prompt(Mode::Poll, "copy", 0, 2 * GIB, 2 * GIB);
}

#[test]
fn copy_2gib_far_epoch() {
    check_prompt(Mode::Epoch, "copy", 0, 2 * GIB, 2 * GIB);
}

// dst > src, overlapping: the chunks run back to front.
#[test]
fn copy_3gib_backward_poll() {
    check_prompt(Mode::Poll, "copy", GIB, 0, 3 * GIB);
}

#[test]
fn copy_3gib_backward_epoch() {
    check_prompt(Mode::Epoch, "copy", GIB, 0, 3 * GIB);
}

/// Guards against the libcall trapping or running the epoch callback mid-op
/// when a callback is installed (a callback may grow, and so move, the
/// memory): the op completes and the callback runs only at the compiled
/// epoch checks (function entry and the deferred check right after the op).
#[test]
fn epoch_callback_never_runs_mid_op() -> Result<()> {
    let engine = engine(Mode::Epoch);
    let module = Module::new(
        &engine,
        r#"
(module
  (memory (export "mem") 1024)
  (func (export "fill") (param i32 i32 i32)
    (memory.fill (local.get 0) (local.get 1) (local.get 2))))
"#,
    )?;
    let mut store = Store::new(&engine, 0u32);
    store.set_epoch_deadline(1);
    store.epoch_deadline_callback(|mut cx| {
        *cx.data_mut() += 1;
        // Deadline = the current epoch: still passed while the op runs.
        Ok(UpdateDeadline::Continue(0))
    });
    let i = Instance::new(&mut store, &module, &[])?;
    let f = i.get_typed_func::<(u32, u32, u32), ()>(&mut store, "fill")?;
    // The deadline has passed before the op starts its chunks.
    engine.increment_epoch();
    engine.increment_epoch();
    // Entry check: callback 1. The libcall sees the deadline passed between
    // every chunk but must neither trap nor run the callback. Post-op
    // check: callback 2.
    f.call(&mut store, (0, 0x5a, (64 * MIB) as u32))?;
    assert_eq!(*store.data(), 2);
    let mem = i.get_memory(&mut store, "mem").unwrap();
    assert!(mem.data(&store)[..(64 * MIB) as usize].iter().all(|b| *b == 0x5a));
    Ok(())
}

/// A small memory with the same exports, for correctness checks.
fn small(pages: u32, shared: bool) -> (Store<()>, Instance, TypedFunc<(u32, u32, u32), ()>, TypedFunc<(u32, u32, u32), ()>) {
    let engine = engine(Mode::Poll);
    let mem = if shared {
        format!("(memory (export \"mem\") {pages} {pages} shared)")
    } else {
        format!("(memory (export \"mem\") {pages})")
    };
    let wat = format!(
        r#"(module {mem}
  (func (export "fill") (param i32 i32 i32)
    (memory.fill (local.get 0) (local.get 1) (local.get 2)))
  (func (export "copy") (param i32 i32 i32)
    (memory.copy (local.get 0) (local.get 1) (local.get 2))))"#
    );
    let module = Module::new(&engine, wat).unwrap();
    let mut store = Store::new(&engine, ());
    let i = Instance::new(&mut store, &module, &[]).unwrap();
    let fill = i.get_typed_func(&mut store, "fill").unwrap();
    let copy = i.get_typed_func(&mut store, "copy").unwrap();
    (store, i, fill, copy)
}

fn pattern(n: usize) -> Vec<u8> {
    // Not periodic in any power of two, so a misplaced chunk shows.
    (0..n).map(|i| (i.wrapping_mul(2654435761) >> 13) as u8).collect()
}

/// Overlapping copies in both directions, with offsets and lengths around
/// the 1 MiB chunk and the 2 MiB threshold, against `copy_within`.
#[test]
fn overlap_both_directions_across_chunks() {
    let size = 16 * MIB as usize;
    let (mut store, i, _fill, copy) = small((size >> 16) as u32, false);
    let mem = i.get_memory(&mut store, "mem").unwrap();
    let m = MIB as usize;
    let lens = [2 * m - 1, 2 * m, 2 * m + 1, 5 * m + 123, 7 * m];
    let deltas = [1, 7, 4096, m - 1, m, m + 1, 3 * m + 5];
    let base = 4 * m + 3;
    for &len in &lens {
        for &d in &deltas {
            for backward in [false, true] {
                let (dst, src) = if backward { (base + d, base) } else { (base, base + d) };
                let mut want = pattern(size);
                mem.data_mut(&mut store).copy_from_slice(&want);
                want.copy_within(src..src + len, dst);
                copy.call(&mut store, (dst as u32, src as u32, len as u32)).unwrap();
                assert!(
                    mem.data(&store) == &want[..],
                    "copy dst={dst} src={src} len={len} mismatched"
                );
            }
        }
    }
    // dst == src and a far (non-overlapping) copy.
    for (dst, src, len) in [(base, base, 5 * m), (0, 8 * m, 6 * m + 1), (8 * m, 1, 6 * m)] {
        let mut want = pattern(size);
        mem.data_mut(&mut store).copy_from_slice(&want);
        want.copy_within(src..src + len, dst);
        copy.call(&mut store, (dst as u32, src as u32, len as u32)).unwrap();
        assert!(mem.data(&store) == &want[..], "copy dst={dst} src={src} len={len}");
    }
}

/// Non-overlapping copies past one 64 MiB far-copy chunk, both orders.
#[test]
fn far_copy_across_big_chunks() {
    let size = 256 * MIB as usize;
    let (mut store, i, _fill, copy) = small((size >> 16) as u32, false);
    let mem = i.get_memory(&mut store, "mem").unwrap();
    let m = MIB as usize;
    let len = 100 * m + 13;
    for (dst, src) in [(3, 128 * m + 5), (128 * m + 5, 3)] {
        let mut want = pattern(size);
        mem.data_mut(&mut store).copy_from_slice(&want);
        want.copy_within(src..src + len, dst);
        copy.call(&mut store, (dst as u32, src as u32, len as u32)).unwrap();
        assert!(mem.data(&store) == &want[..], "copy dst={dst} src={src} len={len}");
    }
}

#[test]
fn fill_across_chunks() {
    let size = 16 * MIB as usize;
    let (mut store, i, fill, _copy) = small((size >> 16) as u32, false);
    let mem = i.get_memory(&mut store, "mem").unwrap();
    let m = MIB as usize;
    for (dst, len) in [(3, 2 * m - 1), (5, 2 * m), (m - 1, 2 * m + 1), (12345, 9 * m + 7)] {
        let mut want = pattern(size);
        mem.data_mut(&mut store).copy_from_slice(&want);
        want[dst..dst + len].fill(0x1f);
        // The value is truncated to a byte, as upstream.
        fill.call(&mut store, (dst as u32, 0x7731f, len as u32)).unwrap();
        assert!(mem.data(&store) == &want[..], "fill dst={dst} len={len}");
    }
}

/// Out of bounds traps with nothing written, also for a chunked length.
#[test]
fn out_of_bounds_writes_nothing() {
    let size = 8 * MIB as usize;
    let (mut store, i, fill, copy) = small((size >> 16) as u32, false);
    let mem = i.get_memory(&mut store, "mem").unwrap();
    let m = MIB as u32;
    let s = size as u32;
    let want = pattern(size);
    let cases: [(&str, u32, u32, u32); 6] = [
        ("fill", m, 0, s - m + 1),
        ("fill", s - 1, 0, 4 * m),
        ("fill", 1, 0, u32::MAX),
        ("copy", 0, m, s - m + 1), // src past the end
        ("copy", m, 0, s - m + 1), // dst past the end
        ("copy", 2 * m, m, u32::MAX),
    ];
    for (op, dst, a, len) in cases {
        mem.data_mut(&mut store).copy_from_slice(&want);
        let err = if op == "fill" {
            fill.call(&mut store, (dst, a, len))
        } else {
            copy.call(&mut store, (dst, a, len))
        }
        .unwrap_err();
        assert_eq!(trap(&err), Trap::MemoryOutOfBounds, "{op} {dst} {a} {len}");
        assert!(mem.data(&store) == &want[..], "{op} {dst} {a} {len} wrote before trapping");
    }
}

/// A shared memory takes the same chunked path.
#[test]
fn shared_memory_chunked() {
    let size = 16 * MIB as usize;
    let (mut store, i, fill, copy) = small((size >> 16) as u32, true);
    let mem = i.get_shared_memory(&mut store, "mem").unwrap();
    let m = MIB as usize;
    fill.call(&mut store, (1, 0x42, (5 * m) as u32)).unwrap();
    copy.call(&mut store, ((m + 9) as u32, 0, (6 * m) as u32)).unwrap();
    let mut want = vec![0u8; size];
    want[1..1 + 5 * m].fill(0x42);
    want.copy_within(0..6 * m, m + 9);
    let got: Vec<u8> = mem.data().iter().map(|c| unsafe { *c.get() }).collect();
    assert!(got == want);
}
