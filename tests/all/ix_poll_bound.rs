//! ix fork: poll interruption with skipped entry polls (`IxPollPlan`) still
//! stops hostile code within the bound
//!
//! ```text
//!     SEGMENT + RETURN x (D - 1)   wasm operators between two polls,
//! ```
//!
//! `D` the wasm frames returned through (see
//! `crates/environ/src/compile/ix_poll_plan.rs`). Each guest here calls the
//! host import `trip`, which sets the poll word, then keeps doing hostile
//! work. A unit of work is `global.get; i64.const; i64.add; global.set` on
//! the exported counter `work` (4 operators). The units counted after
//! `trip` times 4 is a lower bound on the operators run before the next
//! poll, which must trap with `Trap::Interrupt`. Most shapes trip mid-run
//! (`$maybe`, once `work` reaches a threshold) so the polls on the path the
//! guest is on must stop it, not a poll it would hit anyway on the way in.
//! An exempt export called on an interrupted store is
//! `poll_interruption::interrupt_before_call_traps_at_entry_and_rearm_resumes`.
#![cfg(all(unix, not(miri)))]

use wasmtime::*;

/// `IX_POLL_SEGMENT_LIMIT` and `IX_POLL_RETURN_LIMIT`.
const SEGMENT: u64 = 4096;
const RETURN: u64 = 4096;

const WORK: &str = "(global.set $work (i64.add (global.get $work) (i64.const 1)))\n";

fn work(n: usize) -> String {
    WORK.repeat(n)
}

fn trap(e: &Error) -> Trap {
    *e.downcast_ref::<Trap>()
        .unwrap_or_else(|| panic!("not a trap: {e:?}"))
}

fn config() -> Config {
    let mut c = Config::new();
    c.poll_interruption(true)
        .wasm_tail_call(true)
        .wasm_function_references(true)
        .wasm_exceptions(true);
    // A deep recursion needs the stack (below the 2 MiB async stack).
    c.max_wasm_stack(1536 << 10);
    c
}

/// Run `run(arg)` in a module with `$trip` imported, `$work` the counter,
/// `$mark` (trip now) and `$maybe` (trip once `work >= 1000`) and `body`
/// the rest. Returns the work done after `trip`.
fn after_trip_with(c: &Config, body: &str, arg: i32) -> Result<u64> {
    let engine = Engine::new(c)?;
    let wat = format!(
        r#"(module
             (import "" "trip" (func $trip))
             (global $work (export "work") (mut i64) (i64.const 0))
             (global $at (export "at") (mut i64) (i64.const -1))
             (func $mark (global.set $at (global.get $work)) (call $trip))
             (func $maybe
               (if (i32.and (i64.ge_s (global.get $work) (i64.const 1000))
                            (i64.lt_s (global.get $at) (i64.const 0)))
                 (then (call $mark))))
             {body})"#
    );
    let module = Module::new(&engine, &wat)?;
    let mut store = Store::new(&engine, ());
    let h = store.interrupt_handle().unwrap();
    let trip = Func::wrap(&mut store, move || h.interrupt());
    let instance = Instance::new(&mut store, &module, &[trip.into()])?;
    let run = instance.get_typed_func::<i32, ()>(&mut store, "run")?;
    let err = run.call(&mut store, arg).unwrap_err();
    assert_eq!(trap(&err), Trap::Interrupt, "{err:?}");
    let get = |store: &mut Store<()>, name| match instance
        .get_global(&mut *store, name)
        .unwrap()
        .get(&mut *store)
    {
        Val::I64(v) => v,
        v => panic!("{v:?}"),
    };
    let at = get(&mut store, "at");
    assert!(at >= 0, "trip never ran");
    Ok(u64::try_from(get(&mut store, "work") - at).unwrap())
}

fn after_trip(body: &str, arg: i32) -> Result<u64> {
    after_trip_with(&config(), body, arg)
}

fn assert_within(name: &str, done: u64, frames: u64) {
    let bound = SEGMENT + RETURN * (frames - 1);
    println!("{name}: {done} units after trip ({} ops), bound {bound} ops", done * 4);
    assert!(
        done * 4 <= bound,
        "{name}: {done} units = {} ops > bound {bound}",
        done * 4
    );
}

#[test]
fn long_straight_line() -> Result<()> {
    // 100k units in one loop-free function: before, nothing polled until
    // it returned. The second leaves through `br 0` to the function label.
    for exit in ["", "(br 0)"] {
        let done = after_trip(
            &format!(
                r#"(func (export "run") (param i32) {} (call $mark) {} {exit})"#,
                work(10),
                work(100_000)
            ),
            0,
        )?;
        assert_within("straight line", done, 1);
    }
    Ok(())
}

#[test]
fn many_calls_to_an_exempt_leaf() -> Result<()> {
    // The leaf skips its entry poll; its caller is charged for each call.
    let calls = "(call $leaf)\n".repeat(50_000);
    let done = after_trip(
        &format!(
            r#"(func $leaf {} (call $maybe))
               (func (export "run") (param i32) {calls})"#,
            work(3)
        ),
        0,
    )?;
    assert_within("leaf calls", done, 1);
    Ok(())
}

#[test]
fn exponential_call_dag() -> Result<()> {
    // f0 calls f1 8 times, ... f7 calls the leaf: 8^8 leaf runs with no
    // loop and no cycle, so the plan must stop exempting where `B` grows.
    let mut body = format!("(func $f8 {} (call $maybe))\n", work(1));
    for i in (0..8).rev() {
        let calls = format!("(call $f{})\n", i + 1).repeat(8);
        body += &format!("(func $f{i} {} {calls})\n", work(1));
    }
    body += r#"(func (export "run") (param i32) (call $f0))"#;
    let done = after_trip(&body, 0)?;
    // Trips 9 frames deep.
    assert_within("call dag", done, 10);
    Ok(())
}

#[test]
fn deep_recursion_with_long_tails() -> Result<()> {
    // `trip` at the bottom, then every frame runs a 1500-unit (6000-op)
    // tail on the way back up. Before, the whole unwind (depth x 1500
    // units) ran with no poll; now each frame polls inside its tail.
    let depth = 5000;
    let done = after_trip(
        &format!(
            r#"(func $rec (param $n i32)
                 (if (local.get $n)
                   (then (call $rec (i32.sub (local.get $n) (i32.const 1))))
                   (else (call $mark)))
                 {})
               (func (export "run") (param i32) (call $rec (local.get 0)))"#,
            work(1500)
        ),
        depth,
    )?;
    assert_within("recursion tails", done, u64::try_from(depth).unwrap() + 2);
    // Tighter: part of the bottom frame's tail, nothing more.
    assert!(done * 4 <= SEGMENT, "{done}");
    Ok(())
}

#[test]
fn binary_recursion() -> Result<()> {
    // 2^40 calls, no loop: only polls on the call path can stop it.
    let done = after_trip(
        &format!(
            r#"(func $f (param $n i32)
                 {} (call $maybe)
                 (if (local.get $n) (then
                   (call $f (i32.sub (local.get $n) (i32.const 1)))
                   (call $f (i32.sub (local.get $n) (i32.const 1))))))
               (func (export "run") (param i32) (call $f (local.get 0)))"#,
            work(2)
        ),
        40,
    )?;
    assert_within("binary recursion", done, 42);
    Ok(())
}

#[test]
fn mutual_recursion_through_a_table() -> Result<()> {
    // Two tiny functions that only call each other through a table: each
    // would be a leaf-sized exempt candidate but for the indirect call.
    let done = after_trip(
        &format!(
            r#"(type $t (func (param i32)))
               (table 2 funcref)
               (elem (i32.const 0) $a $b)
               (func $a (type $t) {w} (call $maybe)
                 (call_indirect (type $t) (local.get 0) (i32.const 1))
                 (call_indirect (type $t) (local.get 0) (i32.const 1)))
               (func $b (type $t) {w}
                 (call_indirect (type $t) (local.get 0) (i32.const 0)))
               (func (export "run") (param i32) (call $a (local.get 0)))"#,
            w = work(2)
        ),
        0,
    )?;
    assert_within("table recursion", done, 3);
    Ok(())
}

#[test]
fn tail_call_cycles() -> Result<()> {
    let w = work(2);
    let cases = [
        (
            "self",
            format!(
                r#"(func $f (param i32) {w} (call $maybe) (return_call $f (local.get 0)))
                   (func (export "run") (param i32) (call $f (local.get 0)))"#
            ),
        ),
        (
            "mutual",
            format!(
                r#"(func $f (param i32) {w} (call $maybe) (return_call $g (local.get 0)))
                   (func $g (param i32) {w} (return_call $f (local.get 0)))
                   (func (export "run") (param i32) (call $f (local.get 0)))"#
            ),
        ),
        (
            "indirect",
            format!(
                r#"(type $t (func (param i32)))
                   (table 2 funcref)
                   (elem (i32.const 0) $f $g)
                   (func $f (type $t) {w} (call $maybe)
                     (return_call_indirect (type $t) (local.get 0) (i32.const 1)))
                   (func $g (type $t) {w}
                     (return_call_indirect (type $t) (local.get 0) (i32.const 0)))
                   (func (export "run") (param i32) (call $f (local.get 0)))"#
            ),
        ),
        (
            "ref",
            format!(
                r#"(type $t (func (param i32)))
                   (elem declare func $f)
                   (func $f (type $t) {w} (call $maybe)
                     (return_call_ref $t (local.get 0) (ref.func $f)))
                   (func (export "run") (param i32) (call $f (local.get 0)))"#
            ),
        ),
        (
            // An exempt-sized function tail-calls into a cycle of two.
            "into a cycle",
            format!(
                r#"(func $e (param i32) {w} (call $maybe) (return_call $f (local.get 0)))
                   (func $f (param i32) {w} (return_call $g (local.get 0)))
                   (func $g (param i32) {w} (return_call $e (local.get 0)))
                   (func (export "run") (param i32) (call $e (local.get 0)))"#
            ),
        ),
    ];
    for (name, body) in cases {
        let done = after_trip(&body, 0)?;
        assert_within(name, done, 2);
    }
    Ok(())
}

#[test]
fn recursion_through_catch_landings() -> Result<()> {
    // Each frame recurses inside a `try_table` and the bottom throws: the
    // unwind lands in a catch block in every frame, each followed by work.
    let done = after_trip(
        &format!(
            r#"(tag $e)
               (func $f (param $n i32)
                 (block $caught
                   (try_table (catch_all $caught)
                     (if (local.get $n)
                       (then (call $f (i32.sub (local.get $n) (i32.const 1))))
                       (else (call $mark)))
                     (throw $e)))
                 {})
               (func (export "run") (param i32) (call $f (local.get 0)))"#,
            work(1500)
        ),
        2000,
    )?;
    assert_within("catch landings", done, 2002);
    assert!(done * 4 <= SEGMENT, "{done}");
    Ok(())
}

#[test]
fn stack_switching_gets_the_fallback_placement() -> Result<()> {
    // One stack-switching operator makes the structured walk give up on
    // the module; the fallback must still stop a long straight line and
    // deep tails. (Legacy exceptions, the other trigger, are not
    // supported by Cranelift here, so no module can use them.)
    let mut c = config();
    c.wasm_stack_switching(true);
    let other = r#"(type $ft (func)) (type $ct (cont $ft)) (elem declare func $other)
                   (func $other (drop (cont.new $ct (ref.func $other))))"#;
    let straight = format!(
        r#"{other} (func (export "run") (param i32) (call $mark) {})"#,
        work(100_000)
    );
    let done = match after_trip_with(&c, &straight, 0) {
        Err(e) if format!("{e:?}").contains("not supported") => {
            println!("stack switching unsupported here, skipped: {e}");
            return Ok(());
        }
        r => r?,
    };
    assert_within("fallback straight line", done, 1);
    let done = after_trip_with(
        &c,
        &format!(
            r#"{other}
               (func $rec (param $n i32)
                 (if (local.get $n)
                   (then (call $rec (i32.sub (local.get $n) (i32.const 1))))
                   (else (call $mark)))
                 {})
               (func (export "run") (param i32) (call $rec (local.get 0)))"#,
            work(1500)
        ),
        2000,
    )?;
    assert_within("fallback recursion tails", done, 2002);
    assert!(done * 4 <= SEGMENT, "{done}");
    Ok(())
}
