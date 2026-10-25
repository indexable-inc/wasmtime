//! ix bulk memory (indexable fork): `memory.copy` and `memory.fill` with a
//! length only known at run time (or a constant above what
//! `emit_inline_memcpy` takes), without upstream's single unbounded libcall.
//!
//! The caller has bounds-checked both ranges (in 64 bits, so the end cannot
//! overflow) and trapped once if either is out of bounds. This file emits:
//!
//! - **Spectre:** the length is replaced by 0 and both pointers by null with
//!   `select_spectre_guard` when the bounds check failed, so a mispredicted
//!   check runs the op on nothing (and the host calls get null pointers and
//!   a zero length). Every tail address `p + n - k` taken under a size class
//!   `n >= k` is clamped the same way (`max(n, k)` through
//!   `select_spectre_guard`), so a mispredicted class never reaches below
//!   `p`. Speculative accesses therefore stay in `[p, p + max(n, 64))` for
//!   an in-bounds `p` and in `[0, 64)` (below `mmap_min_addr`) otherwise, and
//!   this file is only used for memories whose reservation plus guard covers
//!   4 GiB + 64 KiB (`ix_bulk_enabled`), the bound ordinary loads and stores
//!   rely on.
//! - **Small (n <= 64):** a branch tree on `n` with overlapping head and tail
//!   accesses (1, 2, 4, 8 bytes, then 16-byte vectors for 17-32 and 33-64),
//!   like the small paths of a modern `memcpy`. Every load happens before any
//!   store, so overlapping ranges get `memmove` semantics.
//! - **Medium (64 < n < 2 MiB):** one call of upstream's
//!   `memory_copy`/`memory_fill` libcall, whose host side is a tail jump to
//!   the host's memmove/memset (faster than any inline loop measured above 64
//!   bytes, and small per site).
//! - **Large (n >= 2 MiB, or n >= the `rep` threshold in modes 2 and 3):** a cold
//!   loop around the `ix_bulk_copy`/`ix_bulk_fill` builtin. Each call does one
//!   step (the whole remainder if it is under 2 MiB, so at most 2 MiB - 1 B, else 1 MiB) and returns the bytes done
//!   so far; between steps the loop runs the epoch check and/or the poll
//!   check (whichever interruption is on), so a 4 GiB op can be preempted. The host picks the
//!   step order (memmove semantics) and, in modes 2 and 3, may use `rep movsb` /
//!   `rep stosb` on ERMS hosts (`crates/wasmtime/src/runtime/vm/libcalls.rs`).
//!
//! Per-site code size matters: the Turso guest has about 5900 bulk-op sites
//! (wasm-host bench/results/2026-10-25-bulk-inline).
//!
//! Mode 3 drops the small tree: every op is the medium or large path, which
//! keeps per-site code close to upstream's.
//!
//! Mode 4 (the aarch64 default) keeps upstream's lowering below 2 MiB and
//! adds one compare and branch to the cold stepped loop (`ix_bulk_split`),
//! so only long ops change: on Graviton the masked checks of modes 1-3 cost
//! about 0.3-0.9 ns per small op and nothing there pays it back.
//!
//! An interrupt taken between steps leaves the op partly done. That is
//! only visible if the store survives the interrupt (an async yield resumes
//! the same op; a trap ends the instance in ix-host).

use super::{CheckedEntity, FuncEnvironment};
use crate::translate::TargetEnvironment;
use crate::trap::TranslateTrap;
use cranelift_codegen::ir::condcodes::IntCC;
use cranelift_codegen::ir::types::{I8, I8X16, I16, I32, I64};
use cranelift_codegen::ir::{self, Endianness, InstBuilder, Value};
use cranelift_frontend::FunctionBuilder;
use wasmtime_environ::{MemoryIndex, WasmResult};

/// Largest length handled by the inline branch tree.
pub(super) const SMALL_MAX: u64 = 64;
/// Smallest op that runs as a loop of steps when `rep` is off.
const LOOP_MIN: u64 = 2 << 20;

#[derive(Copy, Clone)]
struct Flags {
    load: ir::MemFlagsData,
    store: ir::MemFlagsData,
}

impl FuncEnvironment<'_> {
    /// Whether ops on memory `index` may be compiled this way.
    ///
    /// Requires a 64-bit host, signal-based traps, a base that never moves,
    /// and a reservation plus guard of at least 4 GiB + 64 KiB, so that every
    /// address the small path can form (also speculatively) lies inside the
    /// memory's own reservation or its guard.
    pub(super) fn ix_bulk_enabled(&mut self, func: &mut ir::Function, index: MemoryIndex) -> bool {
        if self.tunables.ix_bulk_inline == 0
            || self.ix_bulk_bypass
            || self.pointer_type() != I64
            || !self.clif_memory_traps_enabled()
            || !self.isa.supports_vector_load_store(I8X16)
            || !matches!(
                self.isa.triple().architecture,
                target_lexicon::Architecture::X86_64 | target_lexicon::Architecture::Aarch64(_)
            )
        {
            return false;
        }
        let heap = self.get_or_create_heap(func, index);
        let heap = &self.heaps()[heap];
        let mt = heap.memory_tunables(self.tunables);
        let page_log2 = self.target_config().page_size_align_log2;
        heap.memory.can_elide_bounds_check(&mt, page_log2)
            && !heap.memory.memory_may_move(&mt)
            && mt.reservation().saturating_add(mt.guard_size()) >= (1 << 32) + 0x1_0000
    }

    fn ix_flags(&mut self, func: &mut ir::Function, dst: MemoryIndex, src: MemoryIndex) -> Flags {
        let src_region = self.bulk_copy_alias_region(func, CheckedEntity::Memory(src));
        let dst_region = self.bulk_copy_alias_region(func, CheckedEntity::Memory(dst));
        // `trusted()` (notrap + aligned) is sound: the ranges are checked and
        // the backends emit unaligned moves for these types regardless.
        Flags {
            load: ir::MemFlagsData::trusted()
                .with_endianness(Endianness::Little)
                .with_alias_region(Some(src_region)),
            store: ir::MemFlagsData::trusted()
                .with_endianness(Endianness::Little)
                .with_alias_region(Some(dst_region)),
        }
    }

    /// `p` if the bounds check passed, else null, through
    /// `select_spectre_guard`. Besides pointing a mispredicted op at the
    /// unmapped null page, the select hides `p = heap_base + index` from
    /// instruction selection, which would otherwise fold it into
    /// `[base, index]` addressing and keep both alive (and spill the loop
    /// around a small op; aarch64 data in bench/results/2026-10-25-bulk-inline).
    fn ix_mask_ptr(b: &mut FunctionBuilder<'_>, oob: Value, p: Value) -> Value {
        let null = b.ins().iconst(I64, 0);
        b.ins().select_spectre_guard(oob, null, p)
    }

    /// `max(n, k)` given `ok == (n >= k)`, through `select_spectre_guard`.
    fn ix_at_least(b: &mut FunctionBuilder<'_>, ok: Value, n: Value, k: i64) -> Value {
        let k = b.ins().iconst(I64, k);
        b.ins().select_spectre_guard(ok, n, k)
    }

    /// `memory.copy` of `n` bytes from host address `s` to host address `d`.
    /// `const_n` is the length when it is a compile-time constant.
    pub(super) fn ix_bulk_copy(
        &mut self,
        b: &mut FunctionBuilder<'_>,
        dst_mem: MemoryIndex,
        src_mem: MemoryIndex,
        d: Value,
        s: Value,
        n: Value,
        oob: Value,
        const_n: Option<u64>,
    ) -> WasmResult<()> {
        let flags = self.ix_flags(b.func, dst_mem, src_mem);
        let d = Self::ix_mask_ptr(b, oob, d);
        let s = Self::ix_mask_ptr(b, oob, s);
        self.ix_bulk_op(b, true, d, s, n, oob, const_n, |this, b, n, done| {
            this.ix_copy_small(b, flags, d, s, n, done)
        });
        Ok(())
    }

    /// `memory.fill` of `n` bytes at host address `d` with the low byte of `val`.
    pub(super) fn ix_bulk_fill(
        &mut self,
        b: &mut FunctionBuilder<'_>,
        mem: MemoryIndex,
        d: Value,
        val: Value,
        n: Value,
        oob: Value,
        const_n: Option<u64>,
    ) -> WasmResult<()> {
        let flags = self.ix_flags(b.func, mem, mem);
        let d = Self::ix_mask_ptr(b, oob, d);
        self.ix_bulk_op(b, false, d, val, n, oob, const_n, |this, b, n, done| {
            this.ix_fill_small(b, flags, d, val, n, done)
        });
        Ok(())
    }

    fn ix_bulk_op(
        &mut self,
        b: &mut FunctionBuilder<'_>,
        copy: bool,
        d: Value,
        s_or_val: Value,
        n: Value,
        oob: Value,
        const_n: Option<u64>,
        small: impl FnOnce(&mut Self, &mut FunctionBuilder<'_>, Value, ir::Block),
    ) {
        let zero = b.ins().iconst(I64, 0);
        let n = b.ins().select_spectre_guard(oob, zero, n);
        let done = b.create_block();
        if self.tunables.ix_bulk_inline >= 3 || const_n.is_some_and(|k| k > SMALL_MAX) {
            // Mode 3 (compact), mode 4 (only ops of LOOP_MIN or more get
            // here) or known large: no small tree.
            self.ix_large(b, copy, d, s_or_val, n, const_n, done);
        } else {
            let small_b = b.create_block();
            let large_b = b.create_block();
            let is_small = b
                .ins()
                .icmp_imm_u(IntCC::UnsignedLessThanOrEqual, n, SMALL_MAX as i64);
            b.ins().brif(is_small, small_b, &[], large_b, &[]);
            b.seal_block(small_b);
            b.seal_block(large_b);
            b.switch_to_block(small_b);
            small(self, b, n, done);
            b.switch_to_block(large_b);
            self.ix_large(b, copy, d, s_or_val, n, const_n, done);
        }
        b.seal_block(done);
        b.switch_to_block(done);
    }

    /// `n > 64` (any `n` in mode 3). Below `loop_min` (2 MiB, or the `rep` threshold when `rep`
    /// is allowed): one call of upstream's `memory_copy`/`memory_fill`, whose
    /// host side is a tail jump to memmove/memset. From `loop_min`:
    /// `done = 0; loop { done = ix_bulk_*(d, s_or_val, n, done, rep_min);
    /// if done == n { break } epoch/poll check }`. Then jump to `exit`.
    fn ix_large(
        &mut self,
        b: &mut FunctionBuilder<'_>,
        copy: bool,
        d: Value,
        s_or_val: Value,
        n: Value,
        const_n: Option<u64>,
        exit: ir::Block,
    ) {
        let rep_min = if matches!(self.tunables.ix_bulk_inline, 2 | 3) {
            u64::from(self.tunables.ix_bulk_rep_min)
        } else {
            0
        };
        // Ops below two steps take one step anyway (libcalls.rs), so only a
        // possible `rep` needs the step builtin below 2 MiB.
        let rep_from = if copy { rep_min } else { rep_min.max(1 << 20) };
        let loop_min = if rep_min == 0 {
            LOOP_MIN
        } else {
            rep_from.min(LOOP_MIN)
        };
        match const_n {
            Some(k) if k < loop_min => return self.ix_plain(b, copy, d, s_or_val, n, exit),
            Some(_) => {}
            // Mode 4: `ix_bulk_split` only sends `n >= LOOP_MIN` here.
            None if self.tunables.ix_bulk_inline == 4 => {}
            None => {
                let plain = b.create_block();
                let stepped = b.create_block();
                b.set_cold_block(stepped);
                let big = b.ins().icmp_imm_u(
                    IntCC::UnsignedGreaterThanOrEqual,
                    n,
                    i64::try_from(loop_min).unwrap(),
                );
                b.ins().brif(big, stepped, &[], plain, &[]);
                b.seal_block(plain);
                b.seal_block(stepped);
                b.switch_to_block(plain);
                self.ix_plain(b, copy, d, s_or_val, n, exit);
                b.switch_to_block(stepped);
            }
        }
        let rep_min = i64::try_from(rep_min).unwrap();
        let head = b.create_block();
        let more = b.create_block();
        b.set_cold_block(head);
        b.set_cold_block(more);
        let progress = b.append_block_param(head, I64);
        let zero = b.ins().iconst(I64, 0);
        b.ins().jump(head, &[zero.into()]);

        b.switch_to_block(head);
        let vmctx = self.vmctx_val(&mut b.cursor());
        let rep_min = b.ins().iconst(I64, rep_min);
        let f = if copy {
            self.builtin_functions.ix_bulk_copy(b.func)
        } else {
            self.builtin_functions.ix_bulk_fill(b.func)
        };
        let call = b
            .ins()
            .call(f, &[vmctx, d, s_or_val, n, progress, rep_min]);
        let next = b.func.dfg.first_result(call);
        let finished = b.ins().icmp(IntCC::Equal, next, n);
        b.ins().brif(finished, exit, &[], more, &[]);
        b.seal_block(more);

        b.switch_to_block(more);
        // The same interrupt checks as a loop header, so a long op preempts
        // under either scheme (ix-host defaults to poll, epoch off).
        if self.tunables.epoch_interruption {
            self.epoch_check(b);
        }
        if self.tunables.poll_interruption {
            self.poll_check(b);
        }
        b.ins().jump(head, &[next.into()]);
        b.seal_block(head);
    }

    /// Mode 4 (the aarch64 default): upstream's lowering, except that ops of
    /// `LOOP_MIN` bytes or more take the preemptible stepped loop. `op`
    /// translates the whole op (the caller's own translate function): once
    /// with `ix_bulk_in_split` set, where the ix path goes straight to the
    /// loop, and once with `ix_bulk_bypass` set, where the gate is off and
    /// upstream's code runs. A dynamic `len` costs one compare and branch.
    pub(super) fn ix_bulk_split(
        &mut self,
        b: &mut FunctionBuilder<'_>,
        len: Value,
        const_n: Option<u64>,
        mut op: impl FnMut(&mut Self, &mut FunctionBuilder<'_>) -> WasmResult<()>,
    ) -> WasmResult<()> {
        let mut arm = |this: &mut Self, b: &mut FunctionBuilder<'_>, big: bool| {
            if big {
                this.ix_bulk_in_split = true;
            } else {
                this.ix_bulk_bypass = true;
            }
            let r = op(this, b);
            this.ix_bulk_in_split = false;
            this.ix_bulk_bypass = false;
            r
        };
        if let Some(k) = const_n {
            return arm(self, b, k >= LOOP_MIN);
        }
        let big = b.create_block();
        let small = b.create_block();
        let join = b.create_block();
        b.set_cold_block(big);
        // Compare the zero-extended length: the same pure `uextend` the bounds
        // checks and upstream's lowering compute, so the egraph merges them
        // and the split keeps no extra value live (on aarch64 one more value
        // than upstream's spills a tight loop's state to the stack).
        let len64 = if b.func.dfg.value_type(len) == I64 {
            len
        } else {
            b.ins().uextend(I64, len)
        };
        let is_big = b.ins().icmp_imm_u(
            IntCC::UnsignedGreaterThanOrEqual,
            len64,
            i64::try_from(LOOP_MIN).unwrap(),
        );
        b.ins().brif(is_big, big, &[], small, &[]);
        b.seal_block(big);
        b.seal_block(small);
        b.switch_to_block(big);
        arm(self, b, true)?;
        b.ins().jump(join, &[]);
        b.switch_to_block(small);
        arm(self, b, false)?;
        b.ins().jump(join, &[]);
        b.seal_block(join);
        b.switch_to_block(join);
        Ok(())
    }

    /// Whether this op must go through [`Self::ix_bulk_split`] first.
    pub(super) fn ix_bulk_wants_split(&self) -> bool {
        self.tunables.ix_bulk_inline == 4 && !self.ix_bulk_in_split
    }

    /// One call of upstream's `memory_copy(d, s, n)` / `memory_fill(d, val,
    /// n)`, then jump to `exit`.
    fn ix_plain(
        &mut self,
        b: &mut FunctionBuilder<'_>,
        copy: bool,
        d: Value,
        s_or_val: Value,
        n: Value,
        exit: ir::Block,
    ) {
        let vmctx = self.vmctx_val(&mut b.cursor());
        let f = if copy {
            self.builtin_functions.memory_copy(b.func)
        } else {
            self.builtin_functions.memory_fill(b.func)
        };
        b.ins().call(f, &[vmctx, d, s_or_val, n]);
        b.ins().jump(exit, &[]);
    }

    // ---------------------------------------------------------------- small

    /// Branch tree for `n <= 64`, jumping to `done`.
    fn ix_copy_small(
        &mut self,
        b: &mut FunctionBuilder<'_>,
        f: Flags,
        d: Value,
        s: Value,
        n: Value,
        done: ir::Block,
    ) {
        let le16 = b.ins().icmp_imm_u(IntCC::UnsignedLessThanOrEqual, n, 16);
        let (b_le16, b_gt16) = (b.create_block(), b.create_block());
        b.ins().brif(le16, b_le16, &[], b_gt16, &[]);
        b.seal_block(b_le16);
        b.seal_block(b_gt16);

        b.switch_to_block(b_le16);
        for (k, ty) in [(8, I64), (4, I32), (2, I16)] {
            let ge = b.ins().icmp_imm_u(IntCC::UnsignedGreaterThanOrEqual, n, k);
            let (hit, miss) = (b.create_block(), b.create_block());
            b.ins().brif(ge, hit, &[], miss, &[]);
            b.seal_block(hit);
            b.seal_block(miss);
            b.switch_to_block(hit);
            let e = Self::ix_at_least(b, ge, n, k);
            let se = b.ins().iadd(s, e);
            let de = b.ins().iadd(d, e);
            let k32 = i32::try_from(k).unwrap();
            let head = b.ins().load(ty, f.load, s, 0);
            let tail = b.ins().load(ty, f.load, se, -k32);
            b.ins().store(f.store, head, d, 0);
            b.ins().store(f.store, tail, de, -k32);
            b.ins().jump(done, &[]);
            b.switch_to_block(miss);
        }
        // n is 0 or 1.
        let nz = b.ins().icmp_imm_u(IntCC::NotEqual, n, 0);
        let one = b.create_block();
        b.ins().brif(nz, one, &[], done, &[]);
        b.seal_block(one);
        b.switch_to_block(one);
        let v = b.ins().load(I8, f.load, s, 0);
        b.ins().store(f.store, v, d, 0);
        b.ins().jump(done, &[]);

        // 17..=64.
        b.switch_to_block(b_gt16);
        let le32 = b.ins().icmp_imm_u(IntCC::UnsignedLessThanOrEqual, n, 32);
        let (b_le32, b_gt32) = (b.create_block(), b.create_block());
        b.ins().brif(le32, b_le32, &[], b_gt32, &[]);
        b.seal_block(b_le32);
        b.seal_block(b_gt32);
        b.switch_to_block(b_le32);
        let gt16 = b.ins().icmp_imm_u(IntCC::UnsignedGreaterThan, n, 16);
        Self::ix_copy_groups(b, f, d, s, n, gt16, 1);
        b.ins().jump(done, &[]);
        b.switch_to_block(b_gt32);
        let gt32 = b.ins().icmp_imm_u(IntCC::UnsignedGreaterThan, n, 32);
        Self::ix_copy_groups(b, f, d, s, n, gt32, 2);
        b.ins().jump(done, &[]);
    }

    /// Copy `n` bytes, `16 * vecs < n <= 32 * vecs`, as `vecs` vectors from
    /// the start and `vecs` vectors ending at `n`: all loads, then all stores.
    /// `ok` is `n > 16 * vecs`; it clamps the tail end to at least `16 * vecs`,
    /// so a mispredicted size class never addresses below `s` or `d`.
    fn ix_copy_groups(
        b: &mut FunctionBuilder<'_>,
        f: Flags,
        d: Value,
        s: Value,
        n: Value,
        ok: Value,
        vecs: i32,
    ) {
        let e = Self::ix_at_least(b, ok, n, i64::from(16 * vecs));
        let se = b.ins().iadd(s, e);
        let de = b.ins().iadd(d, e);
        let mut vals = Vec::with_capacity(2 * vecs as usize);
        for i in 0..vecs {
            vals.push(b.ins().load(I8X16, f.load, s, 16 * i));
        }
        for i in 0..vecs {
            vals.push(b.ins().load(I8X16, f.load, se, -16 * (vecs - i)));
        }
        for i in 0..vecs {
            b.ins().store(f.store, vals[i as usize], d, 16 * i);
        }
        for i in 0..vecs {
            b.ins()
                .store(f.store, vals[(vecs + i) as usize], de, -16 * (vecs - i));
        }
    }

    /// Branch tree for `n <= 64`, jumping to `done`.
    fn ix_fill_small(
        &mut self,
        b: &mut FunctionBuilder<'_>,
        f: Flags,
        d: Value,
        val: Value,
        n: Value,
        done: ir::Block,
    ) {
        let le16 = b.ins().icmp_imm_u(IntCC::UnsignedLessThanOrEqual, n, 16);
        let (b_le16, b_gt16) = (b.create_block(), b.create_block());
        b.ins().brif(le16, b_le16, &[], b_gt16, &[]);
        b.seal_block(b_le16);
        b.seal_block(b_gt16);

        b.switch_to_block(b_le16);
        let byte = b.ins().band_imm_u(val, 0xff);
        let byte64 = b.ins().uextend(I64, byte);
        let v64 = b.ins().imul_imm_u(byte64, 0x0101_0101_0101_0101_u64 as i64);
        for (k, ty) in [(8, I64), (4, I32), (2, I16)] {
            let ge = b.ins().icmp_imm_u(IntCC::UnsignedGreaterThanOrEqual, n, k);
            let (hit, miss) = (b.create_block(), b.create_block());
            b.ins().brif(ge, hit, &[], miss, &[]);
            b.seal_block(hit);
            b.seal_block(miss);
            b.switch_to_block(hit);
            let v = if ty == I64 { v64 } else { b.ins().ireduce(ty, v64) };
            let e = Self::ix_at_least(b, ge, n, k);
            let de = b.ins().iadd(d, e);
            b.ins().store(f.store, v, d, 0);
            b.ins().store(f.store, v, de, -i32::try_from(k).unwrap());
            b.ins().jump(done, &[]);
            b.switch_to_block(miss);
        }
        let nz = b.ins().icmp_imm_u(IntCC::NotEqual, n, 0);
        let one = b.create_block();
        b.ins().brif(nz, one, &[], done, &[]);
        b.seal_block(one);
        b.switch_to_block(one);
        let v8 = b.ins().ireduce(I8, val);
        b.ins().store(f.store, v8, d, 0);
        b.ins().jump(done, &[]);

        b.switch_to_block(b_gt16);
        let v8 = b.ins().ireduce(I8, val);
        let vec = b.ins().splat(I8X16, v8);
        let le32 = b.ins().icmp_imm_u(IntCC::UnsignedLessThanOrEqual, n, 32);
        let (b_le32, b_gt32) = (b.create_block(), b.create_block());
        b.ins().brif(le32, b_le32, &[], b_gt32, &[]);
        b.seal_block(b_le32);
        b.seal_block(b_gt32);
        b.switch_to_block(b_le32);
        let gt16 = b.ins().icmp_imm_u(IntCC::UnsignedGreaterThan, n, 16);
        Self::ix_fill_groups(b, f, d, vec, n, gt16, 1);
        b.ins().jump(done, &[]);
        b.switch_to_block(b_gt32);
        let gt32 = b.ins().icmp_imm_u(IntCC::UnsignedGreaterThan, n, 32);
        Self::ix_fill_groups(b, f, d, vec, n, gt32, 2);
        b.ins().jump(done, &[]);
    }

    fn ix_fill_groups(
        b: &mut FunctionBuilder<'_>,
        f: Flags,
        d: Value,
        vec: Value,
        n: Value,
        ok: Value,
        vecs: i32,
    ) {
        let e = Self::ix_at_least(b, ok, n, i64::from(16 * vecs));
        let de = b.ins().iadd(d, e);
        for i in 0..vecs {
            b.ins().store(f.store, vec, d, 16 * i);
        }
        for i in 0..vecs {
            b.ins().store(f.store, vec, de, -16 * (vecs - i));
        }
    }
}
