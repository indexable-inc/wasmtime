//! Redundant-move elimination.

use crate::{Allocation, Block, FxHashMap, VReg};
use smallvec::{smallvec, SmallVec};

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum RedundantMoveState {
    Copy(Allocation, Option<VReg>),
    Orig(VReg),
    None,
}
#[derive(Clone, Debug, Default)]
pub struct RedundantMoveEliminator {
    allocs: FxHashMap<Allocation, RedundantMoveState>,
    reverse_allocs: FxHashMap<Allocation, SmallVec<[Allocation; 4]>>,
}
#[derive(Copy, Clone, Debug)]
pub struct RedundantMoveAction {
    pub elide: bool,
}

impl RedundantMoveEliminator {
    pub fn process_move(
        &mut self,
        from: Allocation,
        to: Allocation,
        to_vreg: Option<VReg>,
    ) -> RedundantMoveAction {
        // Look up the src and dest.
        let from_state = self
            .allocs
            .get(&from)
            .map(|&p| p)
            .unwrap_or(RedundantMoveState::None);
        let to_state = self
            .allocs
            .get(&to)
            .map(|&p| p)
            .unwrap_or(RedundantMoveState::None);

        trace!(
            "     -> redundant move tracker: from {} to {} to_vreg {:?}",
            from,
            to,
            to_vreg
        );
        trace!(
            "       -> from_state {:?} to_state {:?}",
            from_state,
            to_state
        );

        if from == to && to_vreg.is_some() {
            self.clear_alloc(to);
            self.allocs
                .insert(to, RedundantMoveState::Orig(to_vreg.unwrap()));
            return RedundantMoveAction { elide: true };
        }

        let src_vreg = match from_state {
            RedundantMoveState::Copy(_, opt_r) => opt_r,
            RedundantMoveState::Orig(r) => Some(r),
            _ => None,
        };
        trace!("      -> src_vreg {:?}", src_vreg);
        let dst_vreg = to_vreg.or(src_vreg);
        trace!("      -> dst_vreg {:?}", dst_vreg);
        let existing_dst_vreg = match to_state {
            RedundantMoveState::Copy(_, opt_r) => opt_r,
            RedundantMoveState::Orig(r) => Some(r),
            _ => None,
        };
        trace!("      -> existing_dst_vreg {:?}", existing_dst_vreg);

        let elide = match (from_state, to_state) {
            (_, RedundantMoveState::Copy(orig_alloc, _)) if orig_alloc == from => true,
            (RedundantMoveState::Copy(new_alloc, _), _) if new_alloc == to => true,
            _ => false,
        };
        trace!("      -> elide {}", elide);

        // Invalidate all existing copies of `to` if `to` actually changed value.
        if !elide {
            self.clear_alloc(to);
        }

        // Set up forward and reverse mapping. Don't track stack-to-stack copies.
        if from.is_reg() || to.is_reg() {
            trace!(
                "     -> create mapping {} -> {:?}",
                to,
                RedundantMoveState::Copy(from, dst_vreg)
            );
            // An elided move keeps `to`'s old mapping until here: move
            // it off its old source's list before linking it to `from`.
            self.unlink(to);
            self.allocs
                .insert(to, RedundantMoveState::Copy(from, dst_vreg));
            let copies = self.reverse_allocs.entry(from).or_insert_with(|| smallvec![]);
            if !copies.contains(&to) {
                copies.push(to);
            }
        }

        RedundantMoveAction { elide }
    }

    /// Tracked entries: allocations with a state plus allocations that
    /// have copies. `reverse_allocs` holds exactly the allocations whose
    /// state is `Copy(from, _)` under `from`, so its total length is at
    /// most `allocs.len()` and this bounds the whole state.
    pub fn len(&self) -> usize {
        self.allocs.len() + self.reverse_allocs.len()
    }

    /// A copy sized to the contents (a hashbrown `clone` keeps the
    /// source's bucket count, however large it once grew), or `None`
    /// when more than `cap` entries are tracked.
    pub fn snapshot(&self, cap: usize) -> Option<Self> {
        if self.len() > cap {
            return None;
        }
        Some(Self {
            allocs: self.allocs.iter().map(|(&k, &v)| (k, v)).collect(),
            reverse_allocs: self
                .reverse_allocs
                .iter()
                .map(|(&k, v)| (k, v.clone()))
                .collect(),
        })
    }

    pub fn clear(&mut self) {
        trace!("   redundant move eliminator cleared");
        // `clear` keeps the tables' buckets and costs time in their
        // number; drop tables that grew large in an earlier block.
        if self.allocs.capacity() > 256 || self.reverse_allocs.capacity() > 256 {
            *self = Self::default();
        } else {
            self.allocs.clear();
            self.reverse_allocs.clear();
        }
    }

    /// Remove `alloc` from the copy list of the allocation it is a copy
    /// of, if any.
    fn unlink(&mut self, alloc: Allocation) {
        if let Some(RedundantMoveState::Copy(src, _)) = self.allocs.get(&alloc).copied() {
            if let Some(copies) = self.reverse_allocs.get_mut(&src) {
                copies.retain(|a| *a != alloc);
                if copies.is_empty() {
                    self.reverse_allocs.remove(&src);
                }
            }
        }
    }

    pub fn clear_alloc(&mut self, alloc: Allocation) {
        trace!("   redundant move eliminator: clear {:?}", alloc);
        // Every allocation on `alloc`'s list is `Copy(alloc, _)`: forget
        // those mappings (their own copy lists stay valid, since their
        // contents did not change).
        if let Some(existing_copies) = self.reverse_allocs.remove(&alloc) {
            for to_inval in existing_copies {
                trace!("     -> clear existing copy: {:?}", to_inval);
                if to_inval != alloc {
                    self.allocs.remove(&to_inval);
                }
            }
        }
        self.unlink(alloc);
        self.allocs.remove(&alloc);
    }
}

/// Exit states of blocks whose single-predecessor successors come later
/// in instruction order, each kept until its last such successor takes
/// it. Saving is bounded: a state is saved only as a snapshot of at most
/// [`BlockExitStates::CAP`] entries, and only for successors at most
/// [`BlockExitStates::WINDOW`] blocks later, so at most `WINDOW` states
/// of `CAP` entries are ever held (every block is visited in order, and
/// a successor in the window consumes its share when it is reached).
#[derive(Default)]
pub struct BlockExitStates {
    saved: FxHashMap<Block, (RedundantMoveEliminator, u32)>,
}

impl BlockExitStates {
    pub const CAP: usize = 64;
    pub const WINDOW: usize = 64;

    /// Whether `succ` of `block` would start from `block`'s exit state.
    pub fn inherits(block: Block, succ: Block, succ_preds: usize) -> bool {
        succ.index() > block.index()
            && succ.index() - block.index() <= Self::WINDOW
            && succ_preds == 1
    }

    /// Save `state` as `block`'s exit state for `consumers` later blocks.
    pub fn save(&mut self, block: Block, state: &RedundantMoveEliminator, consumers: u32) {
        if consumers == 0 {
            return;
        }
        if let Some(snapshot) = state.snapshot(Self::CAP) {
            self.saved.insert(block, (snapshot, consumers));
        }
    }

    /// The exit state of `pred` for one of its consumers, removed on the
    /// last one. `None` when nothing was saved.
    pub fn take(&mut self, pred: Block) -> Option<RedundantMoveEliminator> {
        use hashbrown::hash_map::Entry;
        match self.saved.entry(pred) {
            Entry::Occupied(mut e) => {
                if e.get().1 <= 1 {
                    Some(e.remove().0)
                } else {
                    e.get_mut().1 -= 1;
                    Some(e.get().0.clone())
                }
            }
            Entry::Vacant(_) => None,
        }
    }

    /// States held, for tests.
    #[cfg(test)]
    pub fn held(&self) -> usize {
        self.saved.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PReg, RegClass, SpillSlot};

    fn reg(i: usize) -> Allocation {
        Allocation::reg(PReg::new(i, RegClass::Int))
    }
    fn slot(i: usize) -> Allocation {
        Allocation::stack(SpillSlot::new(i))
    }

    /// A slot reloaded into a register that is then clobbered, over and
    /// over (a value spilled once and reloaded before every call), and
    /// an elided reload repeated: the state must stay constant-size.
    #[test]
    fn reloads_and_elided_moves_do_not_grow_state() {
        let mut rm = RedundantMoveEliminator::default();
        let v = VReg::new(1, RegClass::Int);
        for _ in 0..100_000 {
            assert!(!rm.process_move(slot(0), reg(0), Some(v)).elide);
            assert!(rm.process_move(slot(0), reg(0), Some(v)).elide);
            assert!(rm.process_move(reg(0), slot(0), Some(v)).elide);
            rm.clear_alloc(reg(0));
            assert!(rm.len() <= 2, "state grew to {}", rm.len());
            // Moving the copy elsewhere and back.
            assert!(!rm.process_move(slot(0), reg(1), Some(v)).elide);
            assert!(!rm.process_move(reg(2), reg(1), Some(v)).elide);
            assert!(rm.len() <= 4, "state grew to {}", rm.len());
            rm.clear_alloc(reg(1));
            rm.clear_alloc(reg(2));
        }
        assert!(rm.reverse_allocs.values().all(|c| c.len() <= 1));
    }

    /// Writing the source still forgets every copy of it.
    #[test]
    fn writing_the_source_forgets_copies() {
        let mut rm = RedundantMoveEliminator::default();
        let v = VReg::new(1, RegClass::Int);
        rm.process_move(slot(0), reg(0), Some(v));
        rm.process_move(slot(0), reg(1), Some(v));
        rm.clear_alloc(slot(0));
        assert!(!rm.process_move(slot(0), reg(0), Some(v)).elide);
        assert!(!rm.process_move(slot(0), reg(1), Some(v)).elide);
        // A remapped copy follows its new source only.
        rm.process_move(reg(2), reg(1), Some(v));
        rm.clear_alloc(slot(0));
        assert!(rm.process_move(reg(2), reg(1), Some(v)).elide);
    }

    /// A chain of blocks each saving its exit state for the next: states
    /// are freed on use, oversized ones are not saved, and the snapshot
    /// is sized to its contents.
    #[test]
    fn exit_states_are_freed_and_capped() {
        let v = VReg::new(1, RegClass::Int);
        let mut saved = BlockExitStates::default();
        let mut rm = RedundantMoveEliminator::default();
        for b in 0..50_000 {
            rm.process_move(slot(b % 7), reg(b % 5), Some(v));
            saved.save(Block::new(b), &rm, 1);
            assert!(saved.held() <= 1);
            rm = saved.take(Block::new(b)).unwrap_or_default();
            assert!(rm.len() <= BlockExitStates::CAP);
        }
        assert_eq!(saved.take(Block::new(0)).map(|s| s.len()), None);

        // Over the cap: not saved, so the successor starts empty.
        let mut big = RedundantMoveEliminator::default();
        for i in 0..=BlockExitStates::CAP {
            big.process_move(reg(i % 16), slot(i), Some(v));
        }
        assert!(big.len() > BlockExitStates::CAP);
        saved.save(Block::new(1), &big, 1);
        assert!(saved.take(Block::new(1)).is_none());

        // Two consumers: the second take removes it.
        saved.save(Block::new(2), &rm, 2);
        assert!(saved.take(Block::new(2)).is_some());
        assert_eq!(saved.held(), 1);
        assert!(saved.take(Block::new(2)).is_some());
        assert_eq!(saved.held(), 0);
    }
}
