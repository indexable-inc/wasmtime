/*
 * This file was initially derived from the files
 * `js/src/jit/BacktrackingAllocator.h` and
 * `js/src/jit/BacktrackingAllocator.cpp` in Mozilla Firefox, and was
 * originally licensed under the Mozilla Public License 2.0. We
 * subsequently relicensed it to Apache-2.0 WITH LLVM-exception (see
 * https://github.com/bytecodealliance/regalloc2/issues/7).
 *
 * Since the initial port, the design has been substantially evolved
 * and optimized.
 */

//! Spillslot allocation.

use super::{
    AllocRegResult, Env, LiveBundleIndex, LiveBundleVec, LiveRangeFlag, LiveRangeIndex,
    LiveRangeKey, LiveRangeList, LiveRangeListEntry, PRegIndex, RegTraversalIter, SpillSetIndex,
    SpillSlotData, SpillSlotIndex, UseList, VRegIndex,
};
use crate::{
    ion::data_structures::CodeRange, Allocation, Function, Inst, OperandConstraint, OperandKind,
    PReg, PRegSet, ProgPoint, SpillSlot,
};
use alloc::vec::Vec;

/// A register use of a spilled value outside its spill bundle:
/// (vreg, position, loop weight, LR index, absorbable). Absorbable uses are
/// single-use minimal LRs with a `Reg` or `Limit` constraint, which can
/// take the register of a piece that covers them instead of a move.
type UsePos = (u32, u32, u64, u32, bool);

/// Where the value comes from just before a piece of a spilled range.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum PieceEntry {
    /// The piece starts at the value's def: no move either way.
    Def,
    /// The previous piece (or the def's register) holds it in a register.
    Reg,
    /// The value is (or may be) only on the stack.
    Stack,
}

/// Per-LR state while splitting one spilled bundle into pieces.
struct PieceCursor {
    vreg: VRegIndex,
    /// The not-yet-placed tail of the original LR, still in the spill bundle.
    cur: LiveRangeIndex,
    /// End of the original LR.
    end: ProgPoint,
    /// Whether the value can flow out past `end` (LR reaches a block end).
    live_out: bool,
    /// True once `cur` has been handed over to a register piece entirely.
    consumed: bool,
    prev: PieceEntry,
    prev_reg: Option<PReg>,
}

impl<'a, F: Function> Env<'a, F> {
    pub fn try_allocating_regs_for_spilled_bundles(&mut self) {
        trace!("allocating regs for spilled bundles");
        let mut scratch = core::mem::take(&mut self.ctx.scratch_conflicts);
        // Piecewise second chance (`RegallocOptions::split_spill_bundles`):
        // instructions with clobbers, in order, and a deterministic work
        // budget linear in the function size.
        let split = self.ctx.split_spill_bundles;
        let mut calls: Vec<Inst> = Vec::new();
        let mut budget: usize = 0;
        if split {
            for i in 0..self.func.num_insts() {
                let inst = Inst::new(i);
                if self.func.inst_clobbers(inst) != PRegSet::empty() {
                    calls.push(inst);
                }
            }
            budget = 2 * self.func.num_insts() + 64;
        }
        for i in 0..self.ctx.spilled_bundles.len() {
            let bundle = self.ctx.spilled_bundles[i]; // don't borrow self

            if self.ctx.bundles[bundle].ranges.is_empty() {
                continue;
            }

            let class = self.ctx.spillsets[self.ctx.bundles[bundle].spillset].class;
            let hint = self.ctx.spillsets[self.ctx.bundles[bundle].spillset]
                .hint
                .as_valid();

            // This may be an empty-range bundle whose ranges are not
            // sorted; sort all range-lists again here.
            self.ctx.bundles[bundle]
                .ranges
                .sort_unstable_by_key(|entry| entry.range.from);

            let mut success = false;
            self.ctx.output.stats.spill_bundle_reg_probes += 1;
            let limit = self.bundles[bundle].limit.map(|l| l as usize);
            for preg in RegTraversalIter::new(self.env, class, None, hint, bundle.index(), limit) {
                trace!("trying bundle {:?} to preg {:?}", bundle, preg);
                let preg_idx = PRegIndex::new(preg.index());
                if let AllocRegResult::Allocated(_) =
                    self.try_to_allocate_bundle_to_reg(bundle, preg_idx, None, &mut scratch)
                {
                    self.ctx.output.stats.spill_bundle_reg_success += 1;
                    success = true;
                    break;
                }
            }

            if !success && split && budget > 0 {
                success = self.split_spilled_bundle(bundle, &calls, &mut budget, &mut scratch);
            }

            if !success {
                trace!(
                    "spilling bundle {:?}: marking spillset {:?} as required",
                    bundle,
                    self.ctx.bundles[bundle].spillset
                );
                self.ctx.spillsets[self.ctx.bundles[bundle].spillset].required = true;
            }
        }
        self.ctx.scratch_conflicts = scratch;
    }

    /// Loop-depth weight of a program point (4^depth, like spill weights).
    fn point_weight(&self, p: ProgPoint) -> u64 {
        let block = self.ctx.cfginfo.insn_block[p.inst().index()];
        let depth = core::cmp::min(10, self.ctx.cfginfo.approx_loop_depth[block.index()]);
        1u64 << (2 * depth)
    }

    /// Second chance, piecewise: the whole spilled bundle did not fit in
    /// any register, so cut each of its LRs at instructions with clobbers
    /// (calls) and try each piece on its own, without evicting anything.
    /// A piece is placed in a register only when the reloads it saves at
    /// uses of the value inside it outweigh the moves it adds at its ends
    /// (all weighted by loop depth). A call-free piece that does not fit
    /// is retried block by block. Pieces that stay behind remain in the
    /// spill bundle (on the stack). Every piece costs one unit of
    /// `budget`, so total work is linear in the function size.
    ///
    /// Returns true if every LR found a register.
    fn split_spilled_bundle(
        &mut self,
        bundle: LiveBundleIndex,
        calls: &[Inst],
        budget: &mut usize,
        scratch: &mut LiveBundleVec,
    ) -> bool {
        let spillset = self.ctx.bundles[bundle].spillset;
        trace!("piecewise second chance for spilled bundle {:?}", bundle);

        // Positions of register uses of these vregs outside this bundle:
        // with a stack home, each one is a reload. A vreg's LRs all belong
        // to one spillset, so this scan is linear over the whole function.
        let mut use_pos: Vec<UsePos> = Vec::new();
        let mut seen: Vec<VRegIndex> = Vec::new();
        for entry in self.ctx.bundles[bundle].ranges.iter() {
            let v = self.ctx.ranges[entry.index].vreg;
            if !seen.contains(&v) {
                seen.push(v);
            }
            if seen.len() > 64 {
                // Pathological merged bundle: don't go quadratic in `seen`.
                return false;
            }
        }
        for &v in &seen {
            for e in self.ctx.vregs[v].ranges.iter() {
                let r = &self.ctx.ranges[e.index];
                if r.bundle == bundle {
                    continue;
                }
                let minimal = r.uses.len() == 1
                    && self.ctx.bundles[r.bundle].ranges.len() == 1
                    && self.ctx.bundles[r.bundle].allocation.as_reg().is_some();
                for u in r.uses.iter() {
                    if u.operand.kind() == OperandKind::Use {
                        let absorb = minimal
                            && matches!(
                                u.operand.constraint(),
                                OperandConstraint::Reg | OperandConstraint::Limit(_)
                            );
                        use_pos.push((
                            v.raw_u32(),
                            u.pos.to_index(),
                            self.point_weight(u.pos),
                            e.index.raw_u32(),
                            absorb,
                        ));
                    }
                }
            }
        }
        use_pos.sort_unstable();

        let entries: Vec<LiveRangeListEntry> = self.ctx.bundles[bundle].ranges.iter().cloned().collect();
        let mut stay: LiveRangeList = LiveRangeList::new_in(self.ctx.bump());
        let mut spare_lr: Option<LiveRangeIndex> = None;
        let mut spare_bundle: Option<LiveBundleIndex> = None;

        for entry in entries {
            let lr = entry.index;
            let range = self.ctx.ranges[lr].range;
            let vreg = self.ctx.ranges[lr].vreg;
            let end_block = self.ctx.cfginfo.insn_block[range.to.prev().inst().index()];
            let live_out = range.to >= self.ctx.cfginfo.block_exit[end_block.index()].next();
            let prev = if self.ctx.ranges[lr].has_flag(LiveRangeFlag::StartsAtDef) {
                PieceEntry::Def
            } else if range.from == self.ctx.cfginfo.block_entry
                [self.ctx.cfginfo.insn_block[range.from.inst().index()].index()]
            {
                PieceEntry::Stack
            } else {
                // Right after the def's (register) minimal bundle, or after
                // a register bundle of the same value.
                PieceEntry::Reg
            };
            let mut c = PieceCursor {
                vreg,
                cur: lr,
                end: range.to,
                live_out,
                consumed: false,
                prev,
                prev_reg: None,
            };

            // Cut at calls: [from, Before(c)) [Before(c), Before(c+1)) ...
            let mut ci = calls.partition_point(|&k| ProgPoint::before(k.next()) <= range.from);
            let mut p_from = range.from;
            while *budget > 0 && !c.consumed {
                let call_piece = if ci < calls.len() && ProgPoint::before(calls[ci]) < range.to {
                    let k = calls[ci];
                    ci += 1;
                    let cs = core::cmp::max(ProgPoint::before(k), range.from);
                    let ce = core::cmp::min(ProgPoint::before(k.next()), range.to);
                    Some((cs, ce))
                } else {
                    None
                };
                let gap_to = call_piece.map(|(cs, _)| cs).unwrap_or(range.to);
                if gap_to > p_from {
                    self.place_piece_or_blocks(
                        &mut c, p_from, gap_to, bundle, &use_pos, budget, scratch,
                        &mut spare_lr, &mut spare_bundle, &mut stay,
                    );
                }
                match call_piece {
                    Some((cs, ce)) => {
                        if !c.consumed && *budget > 0 {
                            self.try_piece(
                                &mut c, cs, ce, bundle, &use_pos, budget, scratch,
                                &mut spare_lr, &mut spare_bundle, &mut stay,
                            );
                        }
                        p_from = ce;
                        if p_from >= range.to {
                            break;
                        }
                    }
                    None => break,
                }
            }
            if !c.consumed {
                let r = self.ctx.ranges[c.cur].range;
                stay.push(LiveRangeListEntry { range: r, index: c.cur });
            }
        }

        if let Some(b) = spare_bundle {
            self.ctx.bundles[b].ranges.clear();
        }
        let all_placed = stay.is_empty();
        self.ctx.bundles[bundle].ranges = stay;
        let _ = spillset;
        all_placed
    }

    /// Try a call-free piece; if it does not fit in any register, retry it
    /// one block at a time.
    fn place_piece_or_blocks(
        &mut self,
        c: &mut PieceCursor,
        from: ProgPoint,
        to: ProgPoint,
        bundle: LiveBundleIndex,
        use_pos: &[UsePos],
        budget: &mut usize,
        scratch: &mut LiveBundleVec,
        spare_lr: &mut Option<LiveRangeIndex>,
        spare_bundle: &mut Option<LiveBundleIndex>,
        stay: &mut LiveRangeList,
    ) {
        let saved = (c.prev, c.prev_reg);
        if self.try_piece(c, from, to, bundle, use_pos, budget, scratch, spare_lr, spare_bundle, stay)
        {
            return;
        }
        if c.consumed {
            return;
        }
        let first_block = self.ctx.cfginfo.insn_block[from.inst().index()];
        let last_block = self.ctx.cfginfo.insn_block[to.prev().inst().index()];
        if first_block == last_block {
            return;
        }
        (c.prev, c.prev_reg) = saved;
        let mut p = from;
        let mut b = first_block;
        while p < to && *budget > 0 && !c.consumed {
            let next = core::cmp::min(self.ctx.cfginfo.block_exit[b.index()].next(), to);
            self.try_piece(c, p, next, bundle, use_pos, budget, scratch, spare_lr, spare_bundle, stay);
            p = next;
            b = b.next();
        }
    }

    /// Try to put [from, to) of the cursor's LR in a register. The piece
    /// must start at or after the cursor's current LR start and end at or
    /// before the LR end.
    fn try_piece(
        &mut self,
        c: &mut PieceCursor,
        from: ProgPoint,
        to: ProgPoint,
        bundle: LiveBundleIndex,
        use_pos: &[UsePos],
        budget: &mut usize,
        scratch: &mut LiveBundleVec,
        spare_lr: &mut Option<LiveRangeIndex>,
        spare_bundle: &mut Option<LiveBundleIndex>,
        stay: &mut LiveRangeList,
    ) -> bool {
        if *budget == 0 || c.consumed || from >= to {
            return false;
        }
        *budget -= 1;
        let cur_from = self.ctx.ranges[c.cur].range.from;
        debug_assert!(from >= cur_from && to <= c.end);

        // Reloads saved: register uses strictly inside the piece (a use at
        // `from` would load next to the piece's own entry load anyway).
        let v = c.vreg.raw_u32();
        let lo = use_pos.partition_point(|&(uv, up, ..)| (uv, up) <= (v, from.to_index()));
        let hi = use_pos.partition_point(|&(uv, up, ..)| (uv, up) < (v, to.to_index()));
        // Only uses that can take the piece's register save an instruction;
        // the others trade a reload for a register move.
        let uses_w: u64 = use_pos[lo..hi]
            .iter()
            .filter(|&&(.., absorb)| absorb)
            .map(|&(_, _, w, ..)| w)
            .sum();
        let w_in = self.point_weight(from);
        let exits = to < c.end || c.live_out;
        let w_out = if exits { self.point_weight(to.prev()) } else { 0 };
        let cost_reg = if c.prev == PieceEntry::Stack { w_in } else { 0 } + w_out;
        let cost_stack = if c.prev == PieceEntry::Reg { w_in } else { 0 } + uses_w;
        if cost_reg >= cost_stack {
            c.prev = PieceEntry::Stack;
            return false;
        }

        let whole = from == cur_from && to == c.end;
        let trial = if whole {
            c.cur
        } else {
            let t = match *spare_lr {
                Some(t) => t,
                None => {
                    let t = self
                        .ctx
                        .ranges
                        .add(CodeRange { from, to }, self.ctx.scratch_bump.clone());
                    *spare_lr = Some(t);
                    t
                }
            };
            self.ctx.ranges[t].range = CodeRange { from, to };
            self.ctx.ranges[t].vreg = c.vreg;
            t
        };
        let nb = match *spare_bundle {
            Some(b) => b,
            None => {
                let b = self.ctx.bundles.add(self.ctx.scratch_bump.clone());
                *spare_bundle = Some(b);
                b
            }
        };
        let spillset = self.ctx.bundles[bundle].spillset;
        self.ctx.bundles[nb].spillset = spillset;
        self.ctx.bundles[nb].limit = self.ctx.bundles[bundle].limit;
        self.ctx.bundles[nb].allocation = Allocation::none();
        self.ctx.bundles[nb].ranges.clear();
        self.ctx.bundles[nb].ranges.push(LiveRangeListEntry {
            range: CodeRange { from, to },
            index: trial,
        });

        let class = self.ctx.spillsets[spillset].class;
        let hint = c.prev_reg.or(self.ctx.spillsets[spillset].hint.as_valid());
        let limit = self.ctx.bundles[bundle].limit.map(|l| l as usize);
        let mut got: Option<PReg> = None;
        for preg in RegTraversalIter::new(self.env, class, None, hint, nb.index(), limit) {
            let preg_idx = PRegIndex::new(preg.index());
            if let AllocRegResult::Allocated(_) =
                self.try_to_allocate_bundle_to_reg(nb, preg_idx, None, scratch)
            {
                got = Some(preg);
                break;
            }
        }
        let Some(preg) = got else {
            self.ctx.bundles[nb].ranges.clear();
            c.prev = PieceEntry::Stack;
            return false;
        };
        trace!(
            " -> piece {:?}..{:?} of v{} in {:?}",
            from,
            to,
            c.vreg.index(),
            preg
        );
        self.ctx.output.stats.spill_pieces_placed += 1;
        *spare_bundle = None;
        self.ctx.ranges[trial].bundle = nb;

        if whole {
            c.consumed = true;
        } else {
            *spare_lr = None;
            let piece = trial;
            // Hand the uses inside [from, to) over to the piece.
            let empty = UseList::new_in(self.ctx.bump());
            let uses = core::mem::replace(&mut self.ctx.ranges[c.cur].uses, empty);
            let mut head = UseList::new_in(self.ctx.bump());
            let mut mid = UseList::new_in(self.ctx.bump());
            let mut tail = UseList::new_in(self.ctx.bump());
            for u in uses {
                if u.pos < from {
                    head.push(u);
                } else if u.pos < to {
                    mid.push(u);
                } else {
                    tail.push(u);
                }
            }
            self.ctx.ranges[piece].uses = mid;
            if from == cur_from {
                // The piece takes the front of `cur`; `cur` keeps the rest.
                debug_assert!(to < c.end);
                debug_assert!(head.is_empty());
                if self.ctx.ranges[c.cur].has_flag(LiveRangeFlag::StartsAtDef) {
                    self.ctx.ranges[c.cur].clear_flag(LiveRangeFlag::StartsAtDef);
                    self.ctx.ranges[piece].set_flag(LiveRangeFlag::StartsAtDef);
                }
                self.ctx.ranges[c.cur].uses = tail;
                self.ctx.ranges[c.cur].range.from = to;
            } else {
                self.ctx.ranges[c.cur].uses = head;
                self.ctx.ranges[c.cur].range.to = from;
                let r = self.ctx.ranges[c.cur].range;
                stay.push(LiveRangeListEntry { range: r, index: c.cur });
                if to < c.end {
                    let t = self
                        .ctx
                        .ranges
                        .add(CodeRange { from: to, to: c.end }, self.ctx.scratch_bump.clone());
                    self.ctx.ranges[t].vreg = c.vreg;
                    self.ctx.ranges[t].bundle = bundle;
                    self.ctx.ranges[t].uses = tail;
                    self.recompute_range_properties(t);
                    self.ctx.vregs[c.vreg].ranges.push(LiveRangeListEntry {
                        range: CodeRange { from: to, to: c.end },
                        index: t,
                    });
                    c.cur = t;
                } else {
                    debug_assert!(tail.is_empty());
                    c.consumed = true;
                }
            }
            self.recompute_range_properties(piece);
            self.ctx.vregs[c.vreg].ranges.push(LiveRangeListEntry {
                range: CodeRange { from, to },
                index: piece,
            });
        }
        // Absorb the covered minimal use LRs: they take the piece's register
        // (free over the piece, which holds the same value) instead of a
        // register of their own, so no move is needed at the use.
        // (A "stack" preg -- a fixed stack slot -- cannot serve a `Reg` use.)
        let absorb_ok = !self.ctx.pregs[preg.index()].is_stack;
        for &(_, _, _, lr_raw, absorb) in &use_pos[lo..hi] {
            if !absorb || !absorb_ok {
                continue;
            }
            let m = LiveRangeIndex::new(lr_raw as usize);
            let mr = self.ctx.ranges[m].range;
            if mr.from < from || mr.to > to {
                continue;
            }
            let ok = self.ctx.ranges[m].uses.iter().all(|u| match u.operand.constraint() {
                OperandConstraint::Reg => true,
                OperandConstraint::Limit(n) => preg.hw_enc() < n,
                _ => false,
            });
            if !ok {
                continue;
            }
            let mb = self.ctx.ranges[m].bundle;
            let Some(old) = self.ctx.bundles[mb].allocation.as_reg() else {
                continue;
            };
            if old == preg {
                continue;
            }
            let removed = self.ctx.pregs[old.index()]
                .allocations
                .btree
                .remove(&LiveRangeKey::from_range(&mr));
            debug_assert_eq!(removed, Some(m));
            self.ctx.bundles[mb].allocation = Allocation::reg(preg);
            self.ctx.output.stats.spill_uses_absorbed += 1;
        }
        c.prev = PieceEntry::Reg;
        c.prev_reg = Some(preg);
        true
    }

    pub fn spillslot_can_fit_spillset(
        &mut self,
        spillslot: SpillSlotIndex,
        spillset: SpillSetIndex,
    ) -> bool {
        !self.ctx.spillslots[spillslot.index()]
            .ranges
            .btree
            .contains_key(&LiveRangeKey::from_range(
                &self.ctx.spillsets[spillset].range,
            ))
    }

    pub fn allocate_spillset_to_spillslot(
        &mut self,
        spillset: SpillSetIndex,
        spillslot: SpillSlotIndex,
    ) {
        self.ctx.spillsets[spillset].slot = spillslot;

        let res = self.ctx.spillslots[spillslot.index()].ranges.btree.insert(
            LiveRangeKey::from_range(&self.ctx.spillsets[spillset].range),
            spillset,
        );

        debug_assert!(res.is_none());
    }

    pub fn allocate_spillslots(&mut self) {
        const MAX_ATTEMPTS: usize = 10;

        for spillset in 0..self.ctx.spillsets.len() {
            trace!("allocate spillslot: {}", spillset);
            let spillset = SpillSetIndex::new(spillset);
            if !self.ctx.spillsets[spillset].required {
                continue;
            }
            let class = self.ctx.spillsets[spillset].class as usize;
            // Try a few existing spillslots.
            let mut i = self.ctx.slots_by_class[class].probe_start;
            let mut success = false;
            // Never probe the same element more than once: limit the
            // attempt count to the number of slots in existence.
            for _attempt in
                0..core::cmp::min(self.ctx.slots_by_class[class].slots.len(), MAX_ATTEMPTS)
            {
                // Note: this indexing of `slots` is always valid
                // because either the `slots` list is empty and the
                // iteration limit above consequently means we don't
                // run this loop at all, or else `probe_start` is
                // in-bounds (because it is made so below when we add
                // a slot, and it always takes on the last index `i`
                // after this loop).
                let spillslot = self.ctx.slots_by_class[class].slots[i];

                if self.spillslot_can_fit_spillset(spillslot, spillset) {
                    self.allocate_spillset_to_spillslot(spillset, spillslot);
                    success = true;
                    self.ctx.slots_by_class[class].probe_start = i;
                    break;
                }

                i = self.ctx.slots_by_class[class].next_index(i);
            }

            if !success {
                // Allocate a new spillslot.
                let spillslot = SpillSlotIndex::new(self.ctx.spillslots.len());
                self.ctx.spillslots.push(SpillSlotData {
                    ranges: self.ctx.scratch_spillset_pool.pop().unwrap_or_default(),
                    alloc: Allocation::none(),
                    slots: self.func.spillslot_size(self.ctx.spillsets[spillset].class) as u32,
                });
                self.ctx.slots_by_class[class].slots.push(spillslot);
                self.ctx.slots_by_class[class].probe_start =
                    self.ctx.slots_by_class[class].slots.len() - 1;

                self.allocate_spillset_to_spillslot(spillset, spillslot);
            }
        }

        // Assign actual slot indices to spillslots.
        for i in 0..self.ctx.spillslots.len() {
            self.ctx.spillslots[i].alloc = self.allocate_spillslot(self.ctx.spillslots[i].slots);
        }

        trace!("spillslot allocator done");
    }

    pub fn allocate_spillslot(&mut self, size: u32) -> Allocation {
        let mut offset = self.ctx.output.num_spillslots as u32;
        // Align up to `size`.
        debug_assert!(size.is_power_of_two());
        offset = (offset + size - 1) & !(size - 1);
        let slot = if self.func.multi_spillslot_named_by_last_slot() {
            offset + size - 1
        } else {
            offset
        };
        offset += size;
        self.ctx.output.num_spillslots = offset as _;
        Allocation::stack(SpillSlot::new(slot as usize))
    }
}
