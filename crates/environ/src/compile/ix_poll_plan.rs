//! ix fork: where poll interruption may skip or must add a poll.
//!
//! With `Tunables::poll_interruption`, compiled code polls the store's poll
//! word at every function entry and loop header. This pass, run once per
//! module over the raw function bodies, decides per defined function:
//!
//! * whether its entry poll can be skipped (the function is *exempt*), and
//! * the operators before which an extra poll is placed.
//!
//! # The bound
//!
//! Hostile tenants must not run unbounded between two polls. Count cost in
//! wasm operators: every operator costs 1, a `block`/`loop`/`if`/`try_table`
//! costs 2 (it also pays for its `else`/`end`), and a call costs 1 plus what
//! the callee may run before it polls (below). Most operators lower to a
//! bounded number of machine instructions. Those that do not are unchanged
//! here: the chunked bulk libcalls run at most one chunk before they poll
//! (`bulk_preempt`); `table.fill` and the GC `array.new`/`array.fill` loops
//! that cannot use memset run `len` iterations and poll once after;
//! `memory.atomic.wait` blocks in the host; a GC allocation may collect.
//!
//! The pass keeps, at each point of a function, an upper bound `C` on the
//! cost this frame has run since the last poll, its own *local* cost. It is
//! a forward pass over the structured body: branches merge their `C` into
//! the label's continuation (max), a `loop` header polls (`C = 0`), and code
//! after a `br`/`return`/`unreachable`/`throw` is dead until the enclosing
//! `end` or `else`. Wasm has no other backward edge, so every path through a
//! function between two polls only moves forward in the body and this pass
//! sees all of it.
//!
//! What a call adds:
//!
//! * to a function that keeps its entry poll: 1, and `C` restarts at 1 after
//!   the call returns (the callee polled; what it ran since is accounted to
//!   it, see the chain below);
//! * to an exempt function `g` of this module: `1 + B(g)`, where `B(g)` is
//!   the most `g` (and anything it calls before a poll) can run, the maximum
//!   `C` this pass saw in `g`;
//! * to anything else (`call_indirect`, `call_ref`, an import): `1 +
//!   ESCAPING_LIMIT`. The callee either polls at entry, or is an exempt
//!   function some module exposed, and those have `B <= ESCAPING_LIMIT`.
//!
//! A function is exempt only if it is not on a cycle of direct calls
//! (Tarjan SCCs; a self-call is a cycle) and its `B` is at most
//! `ESCAPING_LIMIT` when it can be reached from outside (exported, in a
//! table, `ref.func`) or is the target of a `return_call`, or
//! `INTERNAL_LIMIT` otherwise. A function with any `call_indirect`,
//! `call_ref` or call to a non-intrinsic import has `B > ESCAPING_LIMIT`,
//! so it is exempt only if nothing outside the module can reach it and no
//! `return_call` names it; it is then only ever entered by a direct call
//! from this module, whose caller is charged its whole `B`. A cycle of
//! direct calls is an SCC, which polls; any other cycle passes through an
//! indirect call, `call_ref`, an import or a `return_call`, whose target
//! either polls at entry or has `B <= ESCAPING_LIMIT`, and a function on
//! the cycle makes that call, so its `B` (or its caller's, up the direct
//! chain to the first function that polls) exceeds that limit. A tail-call
//! cycle is the same.
//!
//! In a function that keeps its entry poll the pass adds a poll before any
//! operator that would take `C` past `SEGMENT_LIMIT`, and before any return
//! (`return`, the final `end`, a branch to the function's own label, a
//! `return_call*` whose callee does not poll at entry) that would leave with
//! `C > RETURN_LIMIT`. Exempt functions get no added polls: their whole
//! run is charged to the caller (`1 + B`), so the caller's limits cover it.
//!
//! The *chain*. When a callee returns, its caller runs on with no poll in
//! between, and so on up the stack: without the return limit a deep
//! recursion whose frames each run a long straight-line tail after the
//! recursive call ran `depth x tail` with no poll, the tail as long as
//! the function (true before this pass). Now each frame that polls at
//! entry returns with local `C <= RETURN_LIMIT` (an exempt callee's whole
//! run, and a `return_call`'s callee, are part of the caller's local cost).
//! So between two polls a thread runs at most
//!
//! ```text
//!     SEGMENT_LIMIT + RETURN_LIMIT x (D - 1)   wasm operators,
//! ```
//!
//! where `D` is the number of wasm frames returned through, at most the
//! frames that fit on the wasm stack. `RETURN_LIMIT` is `SEGMENT_LIMIT`
//! (4096 x D): a lower return limit adds a poll to every call of a function
//! with a long tail, and on Turso 256 cost more than the skipped entry polls
//! saved (`bench/results/2026-10-25-poll-cheap` in wasm-host). The machinery
//! stays so the per-frame constant can be lowered. An exception lands in a catch block
//! that polls (`FuncEnvironment::on_catch_block_entry`), so a throw adds at
//! most the thrower's local cost. Host code is not counted; entering wasm
//! from the host polls in the runtime (`invoke_wasm_and_catch_traps`), so
//! skipping entry polls never lets an interrupted store run wasm.
//!
//! Static bound per exempt function: `B(f) <= INTERNAL_LIMIT` operators
//! (`ESCAPING_LIMIT` if reachable from outside), whatever it calls.
//!
//! The pass reads unvalidated bodies. Anything the structured walk does not
//! understand (legacy exceptions, stack switching, an operator with custom
//! arity it does not know, a bad label or function index) makes it give up
//! on the whole module, which then gets the structure-free [`fallback`]
//! placement: nothing skipped, a poll after every operator that is not
//! straight-line, the same segment and return limits. Unknown operators
//! count as not straight-line, so a new control operator in wasmparser
//! cannot be miscounted. Only a body that does not parse gets no plan (and
//! the old placement), and function validation rejects such a module
//! before any of its code runs.

use crate::prelude::*;
use crate::{DefinedFuncIndex, FuncIndex, FunctionBodyData, Module, PrimaryMap};
use crate::graphs::{Graph, StronglyConnectedComponents};
use cranelift_entity::{EntityRef, SecondaryMap};
use wasmparser::{FunctionBody, Operator};

/// Max `B` of an exempt function reachable from outside the module or by
/// `return_call`, and what an unknown callee is charged.
pub const IX_POLL_ESCAPING_LIMIT: u64 = 128;
/// Max `B` of other exempt functions.
pub const IX_POLL_INTERNAL_LIMIT: u64 = 1024;
/// Max local cost at a return of a function that polls at entry.
pub const IX_POLL_RETURN_LIMIT: u64 = 4096;
/// Max local cost anywhere in a function that polls at entry.
pub const IX_POLL_SEGMENT_LIMIT: u64 = 4096;

const _: () = {
    // An unknown tail callee (`1 + ESCAPING_LIMIT` after a poll) must fit
    // under the return limit, and an exempt callee under the segment limit.
    assert!(1 + IX_POLL_ESCAPING_LIMIT <= IX_POLL_RETURN_LIMIT);
    assert!(IX_POLL_RETURN_LIMIT <= IX_POLL_SEGMENT_LIMIT);
    assert!(1 + IX_POLL_INTERNAL_LIMIT <= IX_POLL_SEGMENT_LIMIT);
};

/// Poll placement for one defined function.
#[derive(Debug, Default, Clone)]
pub struct IxFuncPoll {
    /// Skip the entry poll.
    pub skip_entry: bool,
    /// Original byte offsets of the operators before which to poll, sorted.
    pub checks: Box<[usize]>,
    /// `B` (exempt functions only).
    pub bound: u64,
}

/// Poll placement for a module, from [`IxPollPlan::compute`].
#[derive(Debug, Default)]
pub struct IxPollPlan {
    /// Indexed by defined function.
    pub funcs: PrimaryMap<DefinedFuncIndex, IxFuncPoll>,
    /// Every function got the structure-free [`fallback`] placement.
    pub fallback: bool,
}

impl IxPollPlan {
    /// Statistics for logs and tests: (exempt functions, added polls).
    pub fn stats(&self) -> (usize, usize) {
        let exempt = self.funcs.values().filter(|f| f.skip_entry).count();
        let added = self.funcs.values().map(|f| f.checks.len()).sum();
        (exempt, added)
    }

    /// Plan every defined function of `module`. If any body is beyond the
    /// structured walk, every function gets the [`fallback`] placement
    /// instead. `None` only if a body does not parse (the module is
    /// invalid and is never compiled).
    pub fn compute(
        module: &Module,
        bodies: &PrimaryMap<DefinedFuncIndex, FunctionBodyData<'_>>,
    ) -> Option<IxPollPlan> {
        if let Some(plan) = Self::structured(module, bodies) {
            return Some(plan);
        }
        let mut funcs = PrimaryMap::with_capacity(bodies.len());
        for (_, data) in bodies.iter() {
            funcs.push(fallback(&data.body)?);
        }
        Some(IxPollPlan {
            funcs,
            fallback: true,
        })
    }

    fn structured(
        module: &Module,
        bodies: &PrimaryMap<DefinedFuncIndex, FunctionBodyData<'_>>,
    ) -> Option<IxPollPlan> {
        let n = bodies.len();
        // Pass 1: direct-call edges and `return_call` targets.
        let mut graph = CallGraph {
            edges: PrimaryMap::with_capacity(n),
        };
        let mut tail_called = SecondaryMap::<DefinedFuncIndex, bool>::with_capacity(n);
        for (_, data) in bodies.iter() {
            let body = &data.body;
            let mut succ = Vec::new();
            let mut ops = body.get_operators_reader().ok()?;
            while !ops.eof() {
                match ops.read().ok()? {
                    Operator::Call { function_index } => {
                        let f = FuncIndex::from_u32(function_index);
                        if f.index() >= module.functions.len() {
                            return None;
                        }
                        if let Some(d) = module.defined_func_index(f) {
                            succ.push(d);
                        }
                    }
                    Operator::ReturnCall { function_index } => {
                        let f = FuncIndex::from_u32(function_index);
                        if f.index() >= module.functions.len() {
                            return None;
                        }
                        if let Some(d) = module.defined_func_index(f) {
                            succ.push(d);
                            tail_called[d] = true;
                        }
                    }
                    _ => {}
                }
            }
            succ.sort_unstable();
            succ.dedup();
            graph.edges.push(succ);
        }

        // Pass 2: callees before callers (reverse topological SCC order).
        let mut funcs = SecondaryMap::<DefinedFuncIndex, IxFuncPoll>::with_capacity(n);
        let sccs = StronglyConnectedComponents::new(&graph);
        for (_, members) in sccs.iter() {
            let cyclic = members.len() > 1 || graph.edges[members[0]].contains(&members[0]);
            for &f in members {
                let limit = if module.functions[module.func_index(f)].is_escaping() || tail_called[f]
                {
                    IX_POLL_ESCAPING_LIMIT
                } else {
                    IX_POLL_INTERNAL_LIMIT
                };
                let body = &bodies[f].body;
                let mut plan = None;
                if !cyclic {
                    // Exempt if its `B` fits; members of the same SCC are
                    // not yet planned but cyclic SCCs never get here.
                    let b = Walk::run(module, &funcs, body, Mode::Exempt { limit })?;
                    if let Some(bound) = b.bound {
                        plan = Some(IxFuncPoll {
                            skip_entry: true,
                            checks: Box::new([]),
                            bound,
                        });
                    }
                }
                let plan = match plan {
                    Some(p) => p,
                    None => {
                        let w = Walk::run(module, &funcs, body, Mode::Polled)?;
                        IxFuncPoll {
                            skip_entry: false,
                            checks: w.checks.into_boxed_slice(),
                            bound: 0,
                        }
                    }
                };
                funcs[f] = plan;
            }
        }
        let mut out = PrimaryMap::with_capacity(n);
        for (f, _) in bodies.iter() {
            out.push(core::mem::take(&mut funcs[f]));
        }
        Some(IxPollPlan {
            funcs: out,
            fallback: false,
        })
    }
}

/// Whether `op` is straight-line: no branch, call, return, throw, block
/// boundary or stack switch.
fn is_straight_line(op: &Operator<'_>) -> bool {
    matches!(
        op,
        Operator::StructNew { .. }
            | Operator::StructNewDesc { .. }
            | Operator::ArrayNewFixed { .. }
            | Operator::TypedSelectMulti { .. }
    ) || op_is_plain(op)
}

/// The placement for a module the structured walk gives up on (legacy
/// exceptions, stack switching, an operator it does not know). It needs no
/// structure: the entry polls, and so does the operator after every
/// operator that is not straight-line, so every control transfer lands on
/// a poll (a branch target follows an `end`, `else`, `catch` or `loop`; a
/// call, `resume` or `suspend` continues after it; a callee starts at its
/// entry poll). What runs between two polls is one straight-line run,
/// capped at `SEGMENT_LIMIT` operators, and a frame leaves after at most
/// `RETURN_LIMIT` since its last poll, as in the structured placement.
/// Nothing is skipped, and it polls far more often; only modules using
/// those features pay for it.
fn fallback(body: &FunctionBody<'_>) -> Option<IxFuncPoll> {
    let mut checks = Vec::new();
    let mut run = 0u64;
    let mut after_transfer = false;
    let mut ops = body.get_operators_reader().ok()?;
    while !ops.eof() {
        let pos = usize::try_from(ops.original_position()).ok()?;
        let op = ops.read().ok()?;
        let straight = is_straight_line(&op);
        if after_transfer
            || run >= IX_POLL_SEGMENT_LIMIT
            || (!straight && run >= IX_POLL_RETURN_LIMIT)
        {
            checks.push(pos);
            run = 0;
        }
        run += 1;
        after_transfer = !straight;
    }
    Some(IxFuncPoll {
        skip_entry: false,
        checks: checks.into_boxed_slice(),
        bound: 0,
    })
}

struct CallGraph {
    edges: PrimaryMap<DefinedFuncIndex, Vec<DefinedFuncIndex>>,
}

impl Graph<DefinedFuncIndex> for CallGraph {
    type NodesIter<'a> = cranelift_entity::Keys<DefinedFuncIndex>;
    fn nodes(&self) -> Self::NodesIter<'_> {
        self.edges.keys()
    }
    type SuccessorsIter<'a> = core::iter::Copied<core::slice::Iter<'a, DefinedFuncIndex>>;
    fn successors(&self, node: DefinedFuncIndex) -> Self::SuccessorsIter<'_> {
        self.edges[node].iter().copied()
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    /// Trial as an exempt function: no added polls; fail if `C` passes
    /// `limit`.
    Exempt { limit: u64 },
    /// The function polls at entry: add polls to keep the limits.
    Polled,
}

#[derive(Clone, Copy, PartialEq)]
enum FrameKind {
    Func,
    Block,
    Loop,
    If { has_else: bool },
}

struct Frame {
    kind: FrameKind,
    /// Max `C` over branches to this frame's end (and, for an `if` without
    /// `else`, the implicit else arm). `None`: no branch reaches it.
    label: Option<u64>,
    /// `C` after the `if` condition, for the `else` arm.
    else_c: Option<u64>,
}

fn join(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (a, None) => a,
        (None, b) => b,
    }
}

/// What a call costs the caller.
enum Callee {
    /// Polls at entry: 1, then `C` restarts.
    Polls,
    /// Runs at most this before returning or polling.
    Runs(u64),
}

struct Walk<'a> {
    module: &'a Module,
    funcs: &'a SecondaryMap<DefinedFuncIndex, IxFuncPoll>,
    mode: Mode,
    /// `None`: unreachable.
    c: Option<u64>,
    max: u64,
    frames: Vec<Frame>,
    checks: Vec<usize>,
    /// The exempt trial went past its limit.
    over: bool,
}

struct WalkResult {
    /// Exempt mode: `Some(B)` if exempt.
    bound: Option<u64>,
    checks: Vec<usize>,
}

impl<'a> Walk<'a> {
    fn run(
        module: &'a Module,
        funcs: &'a SecondaryMap<DefinedFuncIndex, IxFuncPoll>,
        body: &FunctionBody<'_>,
        mode: Mode,
    ) -> Option<WalkResult> {
        let mut w = Walk {
            module,
            funcs,
            mode,
            c: Some(0),
            max: 0,
            frames: vec![Frame {
                kind: FrameKind::Func,
                label: None,
                else_c: None,
            }],
            checks: Vec::new(),
            over: false,
        };
        let mut ops = body.get_operators_reader().ok()?;
        while !ops.eof() {
            let pos = usize::try_from(ops.original_position()).ok()?;
            let op = ops.read().ok()?;
            if w.frames.is_empty() {
                return None; // operators after the final `end`
            }
            w.op(pos, &op)?;
            if w.over {
                return Some(WalkResult {
                    bound: None,
                    checks: Vec::new(),
                });
            }
        }
        if !w.frames.is_empty() {
            return None;
        }
        Some(WalkResult {
            bound: match mode {
                Mode::Exempt { .. } => Some(w.max),
                Mode::Polled => None,
            },
            checks: w.checks,
        })
    }

    /// Charge `cost` for the operator at `pos`. `ret`: it leaves the
    /// function (the return limit applies to `C + cost`).
    fn charge(&mut self, pos: usize, cost: u64, ret: bool) {
        let Some(mut c) = self.c else { return };
        match self.mode {
            Mode::Polled => {
                let limit = if ret {
                    IX_POLL_RETURN_LIMIT
                } else {
                    IX_POLL_SEGMENT_LIMIT
                };
                if c.saturating_add(cost) > limit {
                    if self.checks.last() != Some(&pos) {
                        self.checks.push(pos);
                    }
                    c = 0;
                }
            }
            Mode::Exempt { limit } => {
                if c.saturating_add(cost) > limit {
                    self.over = true;
                }
            }
        }
        c = c.saturating_add(cost);
        self.max = self.max.max(c);
        self.c = Some(c);
    }

    fn callee(&self, function_index: u32) -> Option<Callee> {
        let f = FuncIndex::from_u32(function_index);
        if f.index() >= self.module.functions.len() {
            return None;
        }
        Some(match self.module.defined_func_index(f) {
            None if self.module.ix_intrinsic(f).is_some() => Callee::Runs(0),
            None => Callee::Runs(IX_POLL_ESCAPING_LIMIT),
            Some(d) => {
                let p = &self.funcs[d];
                if p.skip_entry {
                    Callee::Runs(p.bound)
                } else {
                    Callee::Polls
                }
            }
        })
    }

    /// The frame `depth` labels out.
    fn target(&self, depth: u32) -> Option<usize> {
        let depth = usize::try_from(depth).ok()?;
        self.frames.len().checked_sub(depth + 1)
    }

    /// A branch to `depth` with the current `C` (already charged).
    fn branch(&mut self, depth: u32) -> Option<()> {
        let t = self.target(depth)?;
        let c = self.c;
        match self.frames[t].kind {
            FrameKind::Loop => {} // the header polls
            _ => self.frames[t].label = join(self.frames[t].label, c),
        }
        Some(())
    }

    /// Whether a branch to any of `depths` leaves the function.
    fn returns(&self, depths: impl IntoIterator<Item = u32>) -> Option<bool> {
        let mut r = false;
        for d in depths {
            r |= self.target(d)? == 0;
        }
        Some(r)
    }

    fn op(&mut self, pos: usize, op: &Operator<'_>) -> Option<()> {
        match op {
            Operator::Block { .. } => {
                self.charge(pos, 2, false);
                self.push(FrameKind::Block);
            }
            Operator::TryTable { try_table } => {
                self.charge(pos, 2, false);
                // A catch lands in a block that polls, then branches to its
                // label (resolved outside the `try_table`).
                let landed = self.c.map(|_| 1);
                for catch in &try_table.catches {
                    let label = match *catch {
                        wasmparser::Catch::One { label, .. }
                        | wasmparser::Catch::OneRef { label, .. }
                        | wasmparser::Catch::All { label }
                        | wasmparser::Catch::AllRef { label } => label,
                    };
                    let t = self.target(label)?;
                    if self.frames[t].kind != FrameKind::Loop {
                        self.frames[t].label = join(self.frames[t].label, landed);
                    }
                }
                self.push(FrameKind::Block);
            }
            Operator::Loop { .. } => {
                self.charge(pos, 2, false);
                self.push(FrameKind::Loop);
                if self.c.is_some() {
                    self.c = Some(0);
                }
            }
            Operator::If { .. } => {
                self.charge(pos, 2, false);
                let c = self.c;
                self.push(FrameKind::If { has_else: false });
                self.frames.last_mut()?.else_c = c;
            }
            Operator::Else => {
                let c = self.c;
                let f = self.frames.last_mut()?;
                let FrameKind::If { has_else: false } = f.kind else {
                    return None;
                };
                f.kind = FrameKind::If { has_else: true };
                f.label = join(f.label, c);
                self.c = f.else_c;
            }
            Operator::End => {
                if self.frames.len() == 1 {
                    // The function's end: falling off returns.
                    self.charge(pos, 0, true);
                    self.frames.pop();
                    return Some(());
                }
                let f = self.frames.pop()?;
                match f.kind {
                    FrameKind::Func => return None,
                    FrameKind::Loop => {}
                    FrameKind::Block | FrameKind::If { has_else: true } => {
                        self.c = join(self.c, f.label);
                    }
                    FrameKind::If { has_else: false } => {
                        self.c = join(join(self.c, f.label), f.else_c);
                    }
                }
            }
            Operator::Br { relative_depth } => {
                let ret = self.returns([*relative_depth])?;
                self.charge(pos, 1, ret);
                self.branch(*relative_depth)?;
                self.c = None;
            }
            Operator::BrIf { relative_depth }
            | Operator::BrOnNull { relative_depth }
            | Operator::BrOnNonNull { relative_depth }
            | Operator::BrOnCast { relative_depth, .. }
            | Operator::BrOnCastFail { relative_depth, .. } => {
                let ret = self.returns([*relative_depth])?;
                self.charge(pos, 1, ret);
                self.branch(*relative_depth)?;
            }
            Operator::BrTable { targets } => {
                let mut depths = Vec::new();
                for t in targets.targets() {
                    depths.push(t.ok()?);
                }
                depths.push(targets.default());
                let ret = self.returns(depths.iter().copied())?;
                self.charge(pos, 1, ret);
                for d in depths {
                    self.branch(d)?;
                }
                self.c = None;
            }
            Operator::Return => {
                self.charge(pos, 1, true);
                self.c = None;
            }
            Operator::Unreachable | Operator::Throw { .. } | Operator::ThrowRef => {
                self.charge(pos, 1, false);
                self.c = None;
            }
            Operator::Call { function_index } => match self.callee(*function_index)? {
                Callee::Polls => {
                    self.charge(pos, 1, false);
                    if self.c.is_some() {
                        self.c = Some(1);
                    }
                }
                Callee::Runs(b) => self.charge(pos, 1 + b, false),
            },
            Operator::CallIndirect { .. } | Operator::CallRef { .. } => {
                self.charge(pos, 1 + IX_POLL_ESCAPING_LIMIT, false);
            }
            Operator::ReturnCall { function_index } => {
                match self.callee(*function_index)? {
                    // The callee polls before running: nothing of this
                    // frame is left to account for.
                    Callee::Polls => self.charge(pos, 1, false),
                    Callee::Runs(b) => self.charge(pos, 1 + b, true),
                }
                self.c = None;
            }
            Operator::ReturnCallIndirect { .. } | Operator::ReturnCallRef { .. } => {
                self.charge(pos, 1 + IX_POLL_ESCAPING_LIMIT, true);
                self.c = None;
            }
            // Custom arity, but straight-line.
            Operator::StructNew { .. }
            | Operator::StructNewDesc { .. }
            | Operator::ArrayNewFixed { .. }
            | Operator::TypedSelectMulti { .. } => self.charge(pos, 1, false),
            other if op_is_plain(other) => self.charge(pos, 1, false),
            _ => return None,
        }
        Some(())
    }

    fn push(&mut self, kind: FrameKind) {
        self.frames.push(Frame {
            kind,
            label: None,
            else_c: None,
        });
    }
}

/// Whether wasmparser lists the operator with a fixed arity, outside the
/// legacy-exceptions and stack-switching proposals: such an operator does
/// not branch, call or return.
macro_rules! ix_poll_plain {
    (@stack_switching ($($ann:tt)*)) => {
        false
    };
    (@legacy_exceptions ($($ann:tt)*)) => {
        false
    };
    (@$proposal:ident (arity custom)) => {
        false
    };
    (@$proposal:ident ($($ann:tt)*)) => {
        true
    };
}

macro_rules! ix_poll_define_plain {
    ($( @$proposal:ident $op:ident $({ $($arg:ident: $argty:ty),* })? => $visit:ident ($($ann:tt)*))*) => {
        fn op_is_plain(op: &Operator<'_>) -> bool {
            match op {
                $(
                    Operator::$op { .. } => ix_poll_plain!(@$proposal ($($ann)*)),
                )*
                #[allow(unreachable_patterns, reason = "Operator is non_exhaustive")]
                _ => false,
            }
        }
    };
}
wasmparser::for_each_operator!(ix_poll_define_plain);
