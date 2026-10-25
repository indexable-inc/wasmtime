//! Proof-carrying code. We attach "facts" to values and then check
//! that they remain true after compilation.
//!
//! A few key design principle of this approach are:
//!
//! - The producer of the IR provides the axioms. All "ground truth",
//!   such as what memory is accessible -- is meant to come by way of
//!   facts on the function arguments and global values. In some
//!   sense, all we are doing here is validating the "internal
//!   consistency" of the facts that are provided on values, and the
//!   actions performed on those values.
//!
//! - We do not derive and forward-propagate facts eagerly. Rather,
//!   the producer needs to provide breadcrumbs -- a "proof witness"
//!   of sorts -- to allow the checking to complete. That means that
//!   as an address is computed, or pointer chains are dereferenced,
//!   each intermediate value will likely have some fact attached.
//!
//!   This does create more verbose IR, but a significant positive
//!   benefit is that it avoids unnecessary work: we do not build up a
//!   knowledge base that effectively encodes the integer ranges of
//!   many or most values in the program. Rather, we only check
//!   specifically the memory-access sequences. In practice, each such
//!   sequence is likely to be a carefully-controlled sequence of IR
//!   operations from, e.g., a sandboxing compiler (such as
//!   Wasmtime) so adding annotations here to communicate
//!   intent (ranges, bounds-checks, and the like) is no problem.
//!
//! Facts are attached to SSA values in CLIF, and are maintained
//! through optimizations and through lowering. They are thus also
//! present on VRegs in the VCode. In theory, facts could be checked
//! at either level, though in practice it is most useful to check
//! them at the VCode level if the goal is an end-to-end verification
//! of certain properties (e.g., memory sandboxing).
//!
//! Checking facts entails visiting each instruction that defines a
//! value with a fact, and checking the result's fact against the
//! facts on arguments and the operand. For VCode, this is
//! fundamentally a question of the target ISA's semantics, so we call
//! into the `LowerBackend` for this. Note that during checking there
//! is also limited forward propagation / inference, but only within
//! an instruction: for example, an addressing mode commonly can
//! include an addition, multiplication/shift, or extend operation,
//! and there is no way to attach facts to the intermediate values
//! "inside" the instruction, so instead the backend can use
//! `FactContext::add()` and friends to forward-propagate facts.
//!
//! TODO:
//!
//! Deployment:
//! - Add to fuzzing
//! - Turn on during wasm spec-tests
//!
//! More checks:
//! - Check that facts on `vmctx` GVs are subsumed by the actual facts
//!   on the vmctx arg in block0 (function arg).
//!
//! Generality:
//! - facts on outputs (in func signature)?
//! - Implement checking at the CLIF level as well.
//! - Check instructions that can trap as well?
//!
//! Nicer errors:
//! - attach instruction index or some other identifier to errors
//!
//! Text format cleanup:
//! - make the bitwidth on `max` facts optional in the CLIF text
//!   format?
//! - make offset in `mem` fact optional in the text format?
//!
//! Bikeshed colors (syntax):
//! - Put fact bang-annotations after types?
//!   `v0: i64 ! fact(..)` vs. `v0 ! fact(..): i64`

use crate::ir;
use crate::ir::types::*;
use crate::isa::TargetIsa;
use crate::machinst::{BlockIndex, InsnIndex, LowerBackend, MachInst as _, VCode};
use crate::trace;
use core::fmt;
use regalloc2::Function as _;
use regalloc2::VReg;

#[cfg(feature = "enable-serde")]
use serde_derive::{Deserialize, Serialize};

/// The result of checking proof-carrying-code facts.
pub type PccResult<T> = core::result::Result<T, PccError>;

/// An error or inconsistency discovered when checking proof-carrying
/// code.
#[derive(Debug, Clone)]
pub enum PccError {
    /// An operation wraps around, invalidating the stated value
    /// range.
    Overflow,
    /// An input to an operator that produces a fact-annotated value
    /// does not have a fact describing it, and one is needed.
    MissingFact,
    /// A derivation of an output fact is unsupported (incorrect or
    /// not derivable).
    UnsupportedFact,
    /// A block parameter claims a fact that one of its predecessors
    /// does not support.
    UnsupportedBlockparam,
    /// A memory access is out of bounds.
    OutOfBounds,
    /// Proof-carrying-code checking is not implemented for a
    /// particular compiler backend.
    UnimplementedBackend,
    /// Proof-carrying-code checking is not implemented for a
    /// particular instruction that instruction-selection chose. This
    /// is an internal compiler error.
    UnimplementedInst,
    /// Access to an invalid or undefined field offset in a struct.
    InvalidFieldOffset,
    /// Access to a field via the wrong type.
    BadFieldType,
    /// Store to a read-only field.
    WriteToReadOnlyField,
    /// Store of data to a field with a fact that does not subsume the
    /// field's fact.
    InvalidStoredFact,
}

/// A fact on a value.
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
#[cfg_attr(feature = "enable-serde", derive(Serialize, Deserialize))]
pub enum Fact {
    /// A bitslice of a value (up to a bitwidth) is within the given
    /// integer range.
    ///
    /// The slicing behavior is needed because this fact can describe
    /// both an SSA `Value`, whose entire value is well-defined, and a
    /// `VReg` in VCode, whose bits beyond the type stored in that
    /// register are don't-care (undefined).
    Range {
        /// The bitwidth of bits we care about, from the LSB upward.
        bit_width: u16,
        /// The minimum value that the bitslice can take
        /// (inclusive). The range is unsigned: the specified bits of
        /// the actual value will be greater than or equal to this
        /// value, as evaluated by an unsigned integer comparison.
        min: u64,
        /// The maximum value that the bitslice can take
        /// (inclusive). The range is unsigned: the specified bits of
        /// the actual value will be less than or equal to this value,
        /// as evaluated by an unsigned integer comparison.
        max: u64,
    },

    /// A value bounded by a global value.
    ///
    /// The range is in `(min_GV + min_offset)..(max_GV +
    /// max_offset)`, inclusive on the lower and upper bound.
    DynamicRange {
        /// The bitwidth of bits we care about, from the LSB upward.
        bit_width: u16,
        /// The lower bound, inclusive.
        min: Expr,
        /// The upper bound, inclusive.
        max: Expr,
    },

    /// A pointer to a memory type.
    Mem {
        /// The memory type.
        ty: ir::MemoryType,
        /// The minimum offset into the memory type, inclusive.
        min_offset: u64,
        /// The maximum offset into the memory type, inclusive.
        max_offset: u64,
        /// This pointer can also be null.
        nullable: bool,
    },

    /// A pointer to a memory type, dynamically bounded. The pointer
    /// is within `(GV_min+offset_min)..(GV_max+offset_max)`
    /// (inclusive on both ends) in the memory type.
    DynamicMem {
        /// The memory type.
        ty: ir::MemoryType,
        /// The lower bound, inclusive.
        min: Expr,
        /// The upper bound, inclusive.
        max: Expr,
        /// This pointer can also be null.
        nullable: bool,
    },

    /// A definition of a value to be used as a symbol in
    /// BaseExprs. There can only be one of these per value number.
    ///
    /// Note that this differs from a `DynamicRange` specifying that
    /// some value in the program is the same as `value`. A `def(v1)`
    /// fact is propagated to machine code and serves as a source of
    /// truth: the value or location labeled with this fact *defines*
    /// what `v1` is, and any `dynamic_range(64, v1, v1)`-labeled
    /// values elsewhere are claiming to be equal to this value.
    ///
    /// This is necessary because we don't propagate SSA value labels
    /// down to machine code otherwise; so when referring symbolically
    /// to addresses and expressions derived from addresses, we need
    /// to introduce the symbol first.
    Def {
        /// The SSA value this value defines.
        value: ir::Value,
    },

    /// A comparison result between two dynamic values with a
    /// comparison of a certain kind.
    Compare {
        /// The kind of comparison.
        kind: ir::condcodes::IntCC,
        /// The left-hand side of the comparison.
        lhs: Expr,
        /// The right-hand side of the comparison.
        rhs: Expr,
    },

    /// A "conflict fact": this fact results from merging two other
    /// facts, and it can never be satisfied -- checking any value
    /// against this fact will fail.
    Conflict,
}

/// A bound expression.
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
#[cfg_attr(feature = "enable-serde", derive(Serialize, Deserialize))]
pub struct Expr {
    /// The dynamic (base) part.
    pub base: BaseExpr,
    /// The static (offset) part.
    pub offset: i64,
}

/// The base part of a bound expression.
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
#[cfg_attr(feature = "enable-serde", derive(Serialize, Deserialize))]
pub enum BaseExpr {
    /// No dynamic part (i.e., zero).
    None,
    /// A global value.
    GlobalValue(ir::GlobalValue),
    /// An SSA Value as a symbolic value. This can be referenced in
    /// facts even after we've lowered out of SSA: it becomes simply
    /// some symbolic value.
    Value(ir::Value),
    /// Top of the address space. This is "saturating": the offset
    /// doesn't matter.
    Max,
}

impl Expr {
    /// Rename the SSA value this expression names, if any.
    pub fn map_symbol(&self, f: &impl Fn(ir::Value) -> ir::Value) -> Expr {
        match self.base {
            BaseExpr::Value(v) => Expr {
                base: BaseExpr::Value(f(v)),
                offset: self.offset,
            },
            _ => self.clone(),
        }
    }
}

impl Fact {
    /// Does this fact name a symbol (an SSA value) rather than only numbers
    /// and global values?
    pub fn is_symbolic(&self) -> bool {
        let named = |e: &Expr| matches!(e.base, BaseExpr::Value(_));
        match self {
            Fact::Def { .. } => true,
            Fact::DynamicRange { min, max, .. } | Fact::DynamicMem { min, max, .. } => {
                named(min) || named(max)
            }
            _ => false,
        }
    }

    /// Rename every SSA value this fact names as a symbol.
    pub fn map_symbols(&self, f: &impl Fn(ir::Value) -> ir::Value) -> Fact {
        match self {
            Fact::Range { .. } | Fact::Mem { .. } | Fact::Conflict => self.clone(),
            Fact::DynamicRange {
                bit_width,
                min,
                max,
            } => Fact::DynamicRange {
                bit_width: *bit_width,
                min: min.map_symbol(f),
                max: max.map_symbol(f),
            },
            Fact::DynamicMem {
                ty,
                min,
                max,
                nullable,
            } => Fact::DynamicMem {
                ty: *ty,
                min: min.map_symbol(f),
                max: max.map_symbol(f),
                nullable: *nullable,
            },
            Fact::Def { value } => Fact::Def { value: f(*value) },
            Fact::Compare { kind, lhs, rhs } => Fact::Compare {
                kind: *kind,
                lhs: lhs.map_symbol(f),
                rhs: rhs.map_symbol(f),
            },
        }
    }
}

impl BaseExpr {
    /// Is one base less than or equal to another? (We can't always
    /// know; in such cases, returns `false`.)
    fn le(lhs: &BaseExpr, rhs: &BaseExpr) -> bool {
        // (i) reflexivity; (ii) 0 <= x for all (unsigned) x; (iii) x <= max for all x.
        lhs == rhs || *lhs == BaseExpr::None || *rhs == BaseExpr::Max
    }

    /// Compute some BaseExpr that will be less than or equal to both
    /// inputs. This is a generalization of `min` (but looser).
    fn min(lhs: &BaseExpr, rhs: &BaseExpr) -> BaseExpr {
        if lhs == rhs {
            lhs.clone()
        } else if *lhs == BaseExpr::Max {
            rhs.clone()
        } else if *rhs == BaseExpr::Max {
            lhs.clone()
        } else {
            BaseExpr::None // zero is <= x for all (unsigned) x.
        }
    }

    /// Compute some BaseExpr that will be greater than or equal to
    /// both inputs.
    fn max(lhs: &BaseExpr, rhs: &BaseExpr) -> BaseExpr {
        if lhs == rhs {
            lhs.clone()
        } else if *lhs == BaseExpr::None {
            rhs.clone()
        } else if *rhs == BaseExpr::None {
            lhs.clone()
        } else {
            BaseExpr::Max
        }
    }
}

impl Expr {
    /// Constant value.
    pub fn constant(offset: i64) -> Self {
        Expr {
            base: BaseExpr::None,
            offset,
        }
    }

    /// The value of an SSA value.
    pub fn value(value: ir::Value) -> Self {
        Expr {
            base: BaseExpr::Value(value),
            offset: 0,
        }
    }

    /// The value of a global value.
    pub fn global_value(gv: ir::GlobalValue) -> Self {
        Expr {
            base: BaseExpr::GlobalValue(gv),
            offset: 0,
        }
    }

    /// Is one expression definitely less than or equal to another?
    /// (We can't always know; in such cases, returns `false`.)
    fn le(lhs: &Expr, rhs: &Expr) -> bool {
        if rhs.base == BaseExpr::Max {
            true
        } else {
            BaseExpr::le(&lhs.base, &rhs.base) && lhs.offset <= rhs.offset
        }
    }

    /// Generalization of `min`: compute some Expr that is less than
    /// or equal to both inputs.
    fn min(lhs: &Expr, rhs: &Expr) -> Expr {
        if lhs.base == BaseExpr::None && lhs.offset == 0 {
            lhs.clone()
        } else if rhs.base == BaseExpr::None && rhs.offset == 0 {
            rhs.clone()
        } else {
            Expr {
                base: BaseExpr::min(&lhs.base, &rhs.base),
                offset: core::cmp::min(lhs.offset, rhs.offset),
            }
        }
    }

    /// Generalization of `max`: compute some Expr that is greater
    /// than or equal to both inputs.
    fn max(lhs: &Expr, rhs: &Expr) -> Expr {
        if lhs.base == BaseExpr::None && lhs.offset == 0 {
            rhs.clone()
        } else if rhs.base == BaseExpr::None && rhs.offset == 0 {
            lhs.clone()
        } else {
            Expr {
                base: BaseExpr::max(&lhs.base, &rhs.base),
                offset: core::cmp::max(lhs.offset, rhs.offset),
            }
        }
    }

    /// Add one expression to another.
    fn add(lhs: &Expr, rhs: &Expr) -> Option<Expr> {
        // (Two equal non-zero bases do not combine: `s + s` is `2s`, which no
        // `Expr` expresses, so that case becomes `Max` below.)
        if lhs.base == BaseExpr::None {
            Some(Expr {
                base: rhs.base.clone(),
                offset: lhs.offset.checked_add(rhs.offset)?,
            })
        } else if rhs.base == BaseExpr::None {
            Some(Expr {
                base: lhs.base.clone(),
                offset: lhs.offset.checked_add(rhs.offset)?,
            })
        } else {
            Some(Expr {
                base: BaseExpr::Max,
                offset: 0,
            })
        }
    }

    /// Add a static offset to an expression.
    pub fn offset(lhs: &Expr, rhs: i64) -> Option<Expr> {
        let offset = lhs.offset.checked_add(rhs)?;
        Some(Expr {
            base: lhs.base.clone(),
            offset,
        })
    }

    /// Is this Expr a BaseExpr with no offset? Return it if so.
    pub fn without_offset(&self) -> Option<&BaseExpr> {
        if self.offset == 0 {
            Some(&self.base)
        } else {
            None
        }
    }
}

impl fmt::Display for BaseExpr {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            BaseExpr::None => Ok(()),
            BaseExpr::Max => write!(f, "max"),
            BaseExpr::GlobalValue(gv) => write!(f, "{gv}"),
            BaseExpr::Value(value) => write!(f, "{value}"),
        }
    }
}

impl BaseExpr {
    /// Does this dynamic_expression take an offset?
    pub fn is_some(&self) -> bool {
        match self {
            BaseExpr::None => false,
            _ => true,
        }
    }
}

impl fmt::Display for Expr {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.base)?;
        match self.offset {
            offset if offset > 0 && self.base.is_some() => write!(f, "+{offset:#x}"),
            offset if offset > 0 => write!(f, "{offset:#x}"),
            offset if offset < 0 => {
                let negative_offset = -i128::from(offset); // upcast to support i64::MIN.
                write!(f, "-{negative_offset:#x}")
            }
            0 if self.base.is_some() => Ok(()),
            0 => write!(f, "0"),
            _ => unreachable!(),
        }
    }
}

impl fmt::Display for Fact {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Fact::Range {
                bit_width,
                min,
                max,
            } => write!(f, "range({bit_width}, {min:#x}, {max:#x})"),
            Fact::DynamicRange {
                bit_width,
                min,
                max,
            } => {
                write!(f, "dynamic_range({bit_width}, {min}, {max})")
            }
            Fact::Mem {
                ty,
                min_offset,
                max_offset,
                nullable,
            } => {
                let nullable_flag = if *nullable { ", nullable" } else { "" };
                write!(
                    f,
                    "mem({ty}, {min_offset:#x}, {max_offset:#x}{nullable_flag})"
                )
            }
            Fact::DynamicMem {
                ty,
                min,
                max,
                nullable,
            } => {
                let nullable_flag = if *nullable { ", nullable" } else { "" };
                write!(f, "dynamic_mem({ty}, {min}, {max}{nullable_flag})")
            }
            Fact::Def { value } => write!(f, "def({value})"),
            Fact::Compare { kind, lhs, rhs } => {
                write!(f, "compare({kind}, {lhs}, {rhs})")
            }
            Fact::Conflict => write!(f, "conflict"),
        }
    }
}

impl Fact {
    /// Create a range fact that specifies a single known constant value.
    pub fn constant(bit_width: u16, value: u64) -> Self {
        debug_assert!(value <= max_value_for_width(bit_width));
        // `min` and `max` are inclusive, so this specifies a range of
        // exactly one value.
        Fact::Range {
            bit_width,
            min: value,
            max: value,
        }
    }

    /// Create a dynamic range fact that points to the base of a dynamic memory.
    pub fn dynamic_base_ptr(ty: ir::MemoryType) -> Self {
        Fact::DynamicMem {
            ty,
            min: Expr::constant(0),
            max: Expr::constant(0),
            nullable: false,
        }
    }

    /// Create a fact that specifies the value is exactly an SSA value.
    ///
    /// Note that this differs from a `def` fact: it is not *defining*
    /// a symbol to have the value that this fact is attached to;
    /// rather it is claiming that this value is the same as whatever
    /// that symbol is. (In other words, the def should be elsewhere,
    /// and we are tying ourselves to it.)
    pub fn value(bit_width: u16, value: ir::Value) -> Self {
        Fact::DynamicRange {
            bit_width,
            min: Expr::value(value),
            max: Expr::value(value),
        }
    }

    /// Create a fact that specifies the value is exactly an SSA value plus some offset.
    pub fn value_offset(bit_width: u16, value: ir::Value, offset: i64) -> Self {
        Fact::DynamicRange {
            bit_width,
            min: Expr::offset(&Expr::value(value), offset).unwrap(),
            max: Expr::offset(&Expr::value(value), offset).unwrap(),
        }
    }

    /// Create a fact that specifies the value is exactly the value of a GV.
    pub fn global_value(bit_width: u16, gv: ir::GlobalValue) -> Self {
        Fact::DynamicRange {
            bit_width,
            min: Expr::global_value(gv),
            max: Expr::global_value(gv),
        }
    }

    /// Create a fact that specifies the value is exactly the value of a GV plus some offset.
    pub fn global_value_offset(bit_width: u16, gv: ir::GlobalValue, offset: i64) -> Self {
        Fact::DynamicRange {
            bit_width,
            min: Expr::offset(&Expr::global_value(gv), offset).unwrap(),
            max: Expr::offset(&Expr::global_value(gv), offset).unwrap(),
        }
    }

    /// Create a range fact that specifies the maximum range for a
    /// value of the given bit-width.
    pub const fn max_range_for_width(bit_width: u16) -> Self {
        match bit_width {
            bit_width if bit_width < 64 => Fact::Range {
                bit_width,
                min: 0,
                max: (1u64 << bit_width) - 1,
            },
            64 => Fact::Range {
                bit_width: 64,
                min: 0,
                max: u64::MAX,
            },
            _ => panic!("bit width too large!"),
        }
    }

    /// Create a range fact that specifies the maximum range for a
    /// value of the given bit-width, zero-extended into a wider
    /// width.
    pub const fn max_range_for_width_extended(from_width: u16, to_width: u16) -> Self {
        debug_assert!(from_width <= to_width);
        match from_width {
            from_width if from_width < 64 => Fact::Range {
                bit_width: to_width,
                min: 0,
                max: (1u64 << from_width) - 1,
            },
            64 => Fact::Range {
                bit_width: to_width,
                min: 0,
                max: u64::MAX,
            },
            _ => panic!("bit width too large!"),
        }
    }

    /// Try to infer a minimal fact for a value of the given IR type.
    pub fn infer_from_type(ty: ir::Type) -> Option<&'static Self> {
        static FACTS: [Fact; 4] = [
            Fact::max_range_for_width(8),
            Fact::max_range_for_width(16),
            Fact::max_range_for_width(32),
            Fact::max_range_for_width(64),
        ];
        match ty {
            I8 => Some(&FACTS[0]),
            I16 => Some(&FACTS[1]),
            I32 => Some(&FACTS[2]),
            I64 => Some(&FACTS[3]),
            _ => None,
        }
    }

    /// Does this fact "propagate" automatically, i.e., cause
    /// instructions that process it to infer their own output facts?
    /// Not all facts propagate automatically; otherwise, verification
    /// would be much slower.
    pub fn propagates(&self) -> bool {
        match self {
            Fact::Mem { .. } => true,
            _ => false,
        }
    }

    /// Is this a constant value of the given bitwidth? Return it as a
    /// `Some(value)` if so.
    pub fn as_const(&self, bits: u16) -> Option<u64> {
        match self {
            Fact::Range {
                bit_width,
                min,
                max,
            } if *bit_width == bits && min == max => Some(*min),
            _ => None,
        }
    }

    /// Is this fact a single-value range with a symbolic Expr?
    pub fn as_symbol(&self) -> Option<&Expr> {
        match self {
            Fact::DynamicRange { min, max, .. } if min == max => Some(min),
            _ => None,
        }
    }

    /// The symbol this fact names a value as: a single-point dynamic range,
    /// or the symbol a `Def` defines.
    pub fn symbol(&self) -> Option<Expr> {
        match self {
            Fact::Def { value } => Some(Expr::value(*value)),
            _ => self.as_symbol().cloned(),
        }
    }

    /// Merge two facts. We take the *intersection*: that is, we know
    /// both facts to be true, so we can intersect ranges. (This
    /// differs from the usual static analysis approach, where we are
    /// merging multiple possibilities into a generalized / widened
    /// fact. We want to narrow here.)
    pub fn intersect(a: &Fact, b: &Fact) -> Fact {
        match (a, b) {
            (
                Fact::Range {
                    bit_width: bw_lhs,
                    min: min_lhs,
                    max: max_lhs,
                },
                Fact::Range {
                    bit_width: bw_rhs,
                    min: min_rhs,
                    max: max_rhs,
                },
            ) if bw_lhs == bw_rhs && max_lhs >= min_rhs && max_rhs >= min_lhs => Fact::Range {
                bit_width: *bw_lhs,
                min: core::cmp::max(*min_lhs, *min_rhs),
                max: core::cmp::min(*max_lhs, *max_rhs),
            },

            (
                Fact::DynamicRange {
                    bit_width: bw_lhs,
                    min: min_lhs,
                    max: max_lhs,
                },
                Fact::DynamicRange {
                    bit_width: bw_rhs,
                    min: min_rhs,
                    max: max_rhs,
                },
            ) if bw_lhs == bw_rhs && Expr::le(min_rhs, max_lhs) && Expr::le(min_lhs, max_rhs) => {
                Fact::DynamicRange {
                    bit_width: *bw_lhs,
                    min: Expr::max(min_lhs, min_rhs),
                    max: Expr::min(max_lhs, max_rhs),
                }
            }

            (
                Fact::Mem {
                    ty: ty_lhs,
                    min_offset: min_offset_lhs,
                    max_offset: max_offset_lhs,
                    nullable: nullable_lhs,
                },
                Fact::Mem {
                    ty: ty_rhs,
                    min_offset: min_offset_rhs,
                    max_offset: max_offset_rhs,
                    nullable: nullable_rhs,
                },
            ) if ty_lhs == ty_rhs
                && max_offset_lhs >= min_offset_rhs
                && max_offset_rhs >= min_offset_lhs =>
            {
                Fact::Mem {
                    ty: *ty_lhs,
                    min_offset: core::cmp::max(*min_offset_lhs, *min_offset_rhs),
                    max_offset: core::cmp::min(*max_offset_lhs, *max_offset_rhs),
                    nullable: *nullable_lhs && *nullable_rhs,
                }
            }

            (
                Fact::DynamicMem {
                    ty: ty_lhs,
                    min: min_lhs,
                    max: max_lhs,
                    nullable: null_lhs,
                },
                Fact::DynamicMem {
                    ty: ty_rhs,
                    min: min_rhs,
                    max: max_rhs,
                    nullable: null_rhs,
                },
            ) if ty_lhs == ty_rhs && Expr::le(min_rhs, max_lhs) && Expr::le(min_lhs, max_rhs) => {
                Fact::DynamicMem {
                    ty: *ty_lhs,
                    min: Expr::max(min_lhs, min_rhs),
                    max: Expr::min(max_lhs, max_rhs),
                    nullable: *null_lhs && *null_rhs,
                }
            }

            // Constant zero and a nullable pointer (an out-of-bounds
            // `select_spectre_guard` the egraph folded to its null arm):
            // keep the constant. The zero is often one GVN'd `iconst 0`
            // shared by the null arms of several memories' guards, so a
            // pointer fact would be wrong for all but one of them; zero
            // satisfies every nullable pointer fact (`subsumes`, `union`).
            (
                r @ Fact::Range { min: 0, max: 0, .. },
                Fact::Mem { nullable: true, .. } | Fact::DynamicMem { nullable: true, .. },
            )
            | (
                Fact::Mem { nullable: true, .. } | Fact::DynamicMem { nullable: true, .. },
                r @ Fact::Range { min: 0, max: 0, .. },
            ) => r.clone(),

            // The base of a memory type and a symbolic pointer into it: the
            // egraph proved `base + index == base` (a constant-zero index
            // behind an explicit check). Keep the base's fact, which its def
            // (a load of the base) proves and which proves any access the
            // memory type has room for. Any other static/symbolic pair keeps
            // the symbolic fact (below): an explicit bounds check needs it.
            (
                m @ Fact::Mem {
                    ty: ty_m,
                    min_offset: 0,
                    max_offset: 0,
                    nullable: false,
                },
                Fact::DynamicMem { ty: ty_d, .. },
            )
            | (
                Fact::DynamicMem { ty: ty_d, .. },
                m @ Fact::Mem {
                    ty: ty_m,
                    min_offset: 0,
                    max_offset: 0,
                    nullable: false,
                },
            ) if ty_m == ty_d => m.clone(),

            // A compare result the egraph proved constant: keep the
            // constant (its def is a constant, which proves only that).
            (Fact::Compare { .. }, r @ Fact::Range { .. })
            | (r @ Fact::Range { .. }, Fact::Compare { .. }) => r.clone(),

            // A symbolic fact and a numeric one, on two values the egraph
            // merged: keep the symbolic one. The checker also derives the
            // numeric fact at the def (`check_output`) and falls back to it
            // when a use needs it, so the merged value can still justify
            // both uses.
            (a, b)
                if a.is_symbolic() != b.is_symbolic()
                    && *a != Fact::Conflict
                    && *b != Fact::Conflict =>
            {
                if a.is_symbolic() {
                    a.clone()
                } else {
                    b.clone()
                }
            }

            // Two values that the egraph merged are equal at runtime, so
            // either symbol names the merged value. Keep one, chosen
            // deterministically; facts that name the other symbol then fail
            // to prove (a false positive, never unsound).
            (Fact::Def { value: lhs }, Fact::Def { value: rhs }) => Fact::Def {
                value: core::cmp::min(*lhs, *rhs),
            },

            _ => Fact::Conflict,
        }
    }
}

macro_rules! ensure {
    ( $condition:expr, $err:tt $(,)? ) => {
        if !$condition {
            return Err(PccError::$err);
        }
    };
}

macro_rules! bail {
    ( $err:tt ) => {{
        return Err(PccError::$err);
    }};
}

/// The two kinds of inequalities: "strict" (`<`, `>`) and "loose"
/// (`<=`, `>=`), the latter of which admit equality.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InequalityKind {
    /// Strict inequality: {less,greater}-than.
    Strict,
    /// Loose inequality: {less,greater}-than-or-equal.
    Loose,
}

/// A "context" in which we can evaluate and derive facts. This
/// context carries environment/global properties, such as the machine
/// pointer width.
pub struct FactContext<'a> {
    function: &'a ir::Function,
    pointer_width: u16,
}

impl<'a> FactContext<'a> {
    /// Create a new "fact context" in which to evaluate facts.
    pub fn new(function: &'a ir::Function, pointer_width: u16) -> Self {
        FactContext {
            function,
            pointer_width,
        }
    }

    /// Computes whether `lhs` "subsumes" (implies) `rhs`.
    pub fn subsumes(&self, lhs: &Fact, rhs: &Fact) -> bool {
        match (lhs, rhs) {
            // Reflexivity.
            (l, r) if l == r => true,

            (
                Fact::Range {
                    bit_width: bw_lhs,
                    min: min_lhs,
                    max: max_lhs,
                },
                Fact::Range {
                    bit_width: bw_rhs,
                    min: min_rhs,
                    max: max_rhs,
                },
            ) => {
                // If the bitwidths we're claiming facts about are the
                // same, or the left-hand-side makes a claim about a
                // wider bitwidth, and if the right-hand-side range is
                // larger than the left-hand-side range, than the LHS
                // subsumes the RHS.
                //
                // In other words, we can always expand the claimed
                // possible value range.
                bw_lhs >= bw_rhs && max_lhs <= max_rhs && min_lhs >= min_rhs
            }

            (
                Fact::DynamicRange {
                    bit_width: bw_lhs,
                    min: min_lhs,
                    max: max_lhs,
                },
                Fact::DynamicRange {
                    bit_width: bw_rhs,
                    min: min_rhs,
                    max: max_rhs,
                },
            ) => {
                // Nearly same as above, but with dynamic-expression
                // comparisons. Note that we require equal bitwidths
                // here: unlike in the static case, we don't have
                // fixed values for min and max, so we can't lean on
                // the well-formedness requirements of the static
                // ranges fitting within the bit-width max.
                bw_lhs == bw_rhs && Expr::le(max_lhs, max_rhs) && Expr::le(min_rhs, min_lhs)
            }

            (
                Fact::Mem {
                    ty: ty_lhs,
                    min_offset: min_offset_lhs,
                    max_offset: max_offset_lhs,
                    nullable: nullable_lhs,
                },
                Fact::Mem {
                    ty: ty_rhs,
                    min_offset: min_offset_rhs,
                    max_offset: max_offset_rhs,
                    nullable: nullable_rhs,
                },
            ) => {
                ty_lhs == ty_rhs
                    && max_offset_lhs <= max_offset_rhs
                    && min_offset_lhs >= min_offset_rhs
                    && (*nullable_lhs || !*nullable_rhs)
            }

            (
                Fact::DynamicMem {
                    ty: ty_lhs,
                    min: min_lhs,
                    max: max_lhs,
                    nullable: nullable_lhs,
                },
                Fact::DynamicMem {
                    ty: ty_rhs,
                    min: min_rhs,
                    max: max_rhs,
                    nullable: nullable_rhs,
                },
            ) => {
                ty_lhs == ty_rhs
                    && Expr::le(max_lhs, max_rhs)
                    && Expr::le(min_rhs, min_lhs)
                    && (*nullable_lhs || !*nullable_rhs)
            }

            // Constant zero subsumes nullable pointers.
            (
                Fact::Range {
                    bit_width,
                    min: 0,
                    max: 0,
                },
                Fact::Mem { nullable: true, .. },
            ) if *bit_width == self.pointer_width => true,

            // Constant zero subsumes nullable DynamicMem pointers.
            (
                Fact::Range {
                    bit_width,
                    min: 0,
                    max: 0,
                },
                Fact::DynamicMem { nullable: true, .. },
            ) if *bit_width == self.pointer_width => true,

            // Any fact subsumes a Def, because the Def makes no
            // claims about the actual value (it ties a symbol to that
            // value, but the value is fed to the symbol, not the
            // other way around).
            (_, Fact::Def { .. }) => true,

            _ => false,
        }
    }

    /// Computes whether the optional fact `lhs` subsumes (implies)
    /// the optional fact `lhs`. A `None` never subsumes any fact, and
    /// is always subsumed by any fact at all (or no fact).
    pub fn subsumes_fact_optionals(&self, lhs: Option<&Fact>, rhs: Option<&Fact>) -> bool {
        match (lhs, rhs) {
            (None, None) => true,
            (Some(_), None) => true,
            (None, Some(_)) => false,
            (Some(lhs), Some(rhs)) => self.subsumes(lhs, rhs),
        }
    }

    /// Computes whatever fact can be known about the sum of two
    /// values with attached facts. The add is performed to the given
    /// bit-width. Note that this is distinct from the machine or
    /// pointer width: e.g., many 64-bit machines can still do 32-bit
    /// adds that wrap at 2^32.
    pub fn add(&self, lhs: &Fact, rhs: &Fact, add_width: u16) -> Option<Fact> {
        let result = match (lhs, rhs) {
            (
                Fact::Range {
                    bit_width: bw_lhs,
                    min: min_lhs,
                    max: max_lhs,
                },
                Fact::Range {
                    bit_width: bw_rhs,
                    min: min_rhs,
                    max: max_rhs,
                },
            ) if bw_lhs == bw_rhs && add_width >= *bw_lhs => {
                let computed_min = min_lhs.checked_add(*min_rhs)?;
                let computed_max = max_lhs.checked_add(*max_rhs)?;
                let width_max = max_value_for_width(add_width);
                if computed_max > width_max {
                    // The add can wrap at `add_width`, so a wrapped result
                    // can be anywhere below the width's maximum: clamping
                    // only the max would keep a wrong (too high) min.
                    Some(Fact::Range {
                        bit_width: *bw_lhs,
                        min: 0,
                        max: width_max,
                    })
                } else {
                    Some(Fact::Range {
                        bit_width: *bw_lhs,
                        min: computed_min,
                        max: computed_max,
                    })
                }
            }

            (
                Fact::Range {
                    bit_width: bw_max,
                    min,
                    max,
                },
                Fact::Mem {
                    ty,
                    min_offset,
                    max_offset,
                    nullable,
                },
            )
            | (
                Fact::Mem {
                    ty,
                    min_offset,
                    max_offset,
                    nullable,
                },
                Fact::Range {
                    bit_width: bw_max,
                    min,
                    max,
                },
            ) if *bw_max >= self.pointer_width
                && add_width >= *bw_max
                && (!*nullable || *max == 0) =>
            {
                let min_offset = min_offset.checked_add(*min)?;
                let max_offset = max_offset.checked_add(*max)?;
                Some(Fact::Mem {
                    ty: *ty,
                    min_offset,
                    max_offset,
                    nullable: false,
                })
            }

            (
                Fact::Range {
                    bit_width: bw_static,
                    min: min_static,
                    max: max_static,
                },
                Fact::DynamicRange {
                    bit_width: bw_dynamic,
                    min: min_dynamic,
                    max: max_dynamic,
                },
            )
            | (
                Fact::DynamicRange {
                    bit_width: bw_dynamic,
                    min: min_dynamic,
                    max: max_dynamic,
                },
                Fact::Range {
                    bit_width: bw_static,
                    min: min_static,
                    max: max_static,
                },
            ) if bw_static == bw_dynamic && add_width >= *bw_dynamic => {
                let min = Expr::offset(min_dynamic, i64::try_from(*min_static).ok()?)?;
                let max = Expr::offset(max_dynamic, i64::try_from(*max_static).ok()?)?;
                Some(Fact::DynamicRange {
                    bit_width: *bw_dynamic,
                    min,
                    max,
                })
            }

            (
                Fact::DynamicMem {
                    ty,
                    min: min_mem,
                    max: max_mem,
                    nullable: false,
                },
                Fact::DynamicRange {
                    bit_width,
                    min: min_range,
                    max: max_range,
                },
            )
            | (
                Fact::DynamicRange {
                    bit_width,
                    min: min_range,
                    max: max_range,
                },
                Fact::DynamicMem {
                    ty,
                    min: min_mem,
                    max: max_mem,
                    nullable: false,
                },
            ) if *bit_width == self.pointer_width && add_width >= self.pointer_width => {
                let min = Expr::add(min_mem, min_range)?;
                let max = Expr::add(max_mem, max_range)?;
                Some(Fact::DynamicMem {
                    ty: *ty,
                    min,
                    max,
                    nullable: false,
                })
            }

            (
                Fact::Mem {
                    ty,
                    min_offset,
                    max_offset,
                    nullable: false,
                },
                Fact::DynamicRange {
                    bit_width,
                    min: min_range,
                    max: max_range,
                },
            )
            | (
                Fact::DynamicRange {
                    bit_width,
                    min: min_range,
                    max: max_range,
                },
                Fact::Mem {
                    ty,
                    min_offset,
                    max_offset,
                    nullable: false,
                },
            ) if *bit_width == self.pointer_width => {
                let min = Expr::offset(min_range, i64::try_from(*min_offset).ok()?)?;
                let max = Expr::offset(max_range, i64::try_from(*max_offset).ok()?)?;
                Some(Fact::DynamicMem {
                    ty: *ty,
                    min,
                    max,
                    nullable: false,
                })
            }

            (
                Fact::Range {
                    bit_width: bw_static,
                    min: min_static,
                    max: max_static,
                },
                Fact::DynamicMem {
                    ty,
                    min: min_dynamic,
                    max: max_dynamic,
                    nullable,
                },
            )
            | (
                Fact::DynamicMem {
                    ty,
                    min: min_dynamic,
                    max: max_dynamic,
                    nullable,
                },
                Fact::Range {
                    bit_width: bw_static,
                    min: min_static,
                    max: max_static,
                },
            ) if *bw_static == self.pointer_width && (!*nullable || *max_static == 0) => {
                let min = Expr::offset(min_dynamic, i64::try_from(*min_static).ok()?)?;
                let max = Expr::offset(max_dynamic, i64::try_from(*max_static).ok()?)?;
                Some(Fact::DynamicMem {
                    ty: *ty,
                    min,
                    max,
                    nullable: false,
                })
            }

            _ => None,
        };

        trace!("add: {lhs:?} + {rhs:?} -> {result:?}");
        result
    }

    /// Computes the `uextend` of a value with the given facts.
    pub fn uextend(&self, fact: &Fact, from_width: u16, to_width: u16) -> Option<Fact> {
        if from_width == to_width {
            return Some(fact.clone());
        }

        let result = match fact {
            // If the claim is already for a same-or-wider value and the min
            // and max are within range of the narrower value, we can
            // claim the same range.
            Fact::Range {
                bit_width,
                min,
                max,
            } if *bit_width >= from_width
                && *min <= max_value_for_width(from_width)
                && *max <= max_value_for_width(from_width) =>
            {
                Some(Fact::Range {
                    bit_width: to_width,
                    min: *min,
                    max: *max,
                })
            }

            // If the claim is a dynamic range for the from-width, we
            // can extend to the to-width.
            Fact::DynamicRange {
                bit_width,
                min,
                max,
            } if *bit_width == from_width => Some(Fact::DynamicRange {
                bit_width: to_width,
                min: min.clone(),
                max: max.clone(),
            }),

            // If the claim is a definition of a value, we can say
            // that the output has a range of exactly that value.
            Fact::Def { value } => Some(Fact::value(to_width, *value)),

            // Otherwise, we can at least claim that the value is
            // within the range of `from_width`.
            Fact::Range { .. } => Some(Fact::max_range_for_width_extended(from_width, to_width)),

            _ => None,
        };
        trace!("uextend: fact {fact:?} from {from_width} to {to_width} -> {result:?}");
        result
    }

    /// Computes the `sextend` of a value with the given facts.
    pub fn sextend(&self, fact: &Fact, from_width: u16, to_width: u16) -> Option<Fact> {
        match fact {
            // If we have a defined value in bits 0..bit_width, and
            // the MSB w.r.t. `from_width` is *not* set, then we can
            // do the same as `uextend`.
            Fact::Range {
                bit_width,
                // We can ignore `min`: it is always <= max in
                // unsigned terms, and we check max's LSB below.
                min: _,
                max,
            } if *bit_width == from_width && (*max & (1 << (*bit_width - 1)) == 0) => {
                self.uextend(fact, from_width, to_width)
            }
            _ => None,
        }
    }

    /// Computes the bit-truncation of a value with the given fact.
    pub fn truncate(&self, fact: &Fact, from_width: u16, to_width: u16) -> Option<Fact> {
        if from_width == to_width {
            return Some(fact.clone());
        }

        trace!(
            "truncate: fact {:?} from {} to {}",
            fact, from_width, to_width
        );

        match fact {
            Fact::Range {
                bit_width,
                min,
                max,
            } if *bit_width == from_width => {
                let max_val = (1u64 << to_width) - 1;
                if *min <= max_val && *max <= max_val {
                    Some(Fact::Range {
                        bit_width: to_width,
                        min: *min,
                        max: *max,
                    })
                } else {
                    Some(Fact::Range {
                        bit_width: to_width,
                        min: 0,
                        max: max_val,
                    })
                }
            }
            _ => None,
        }
    }

    /// Scales a value with a fact by a known constant.
    pub fn scale(&self, fact: &Fact, width: u16, factor: u32) -> Option<Fact> {
        let result = match fact {
            x if factor == 1 => Some(x.clone()),

            Fact::Range {
                bit_width,
                min,
                max,
            } if *bit_width == width => {
                let min = min.checked_mul(u64::from(factor))?;
                let max = max.checked_mul(u64::from(factor))?;
                if *bit_width < 64 && max > max_value_for_width(width) {
                    return None;
                }
                Some(Fact::Range {
                    bit_width: *bit_width,
                    min,
                    max,
                })
            }
            _ => None,
        };
        trace!("scale: {fact:?} * {factor} at width {width} -> {result:?}");
        result
    }

    /// Left-shifts a value with a fact by a known constant.
    pub fn shl(&self, fact: &Fact, width: u16, amount: u16) -> Option<Fact> {
        if amount >= 32 {
            return None;
        }
        let factor: u32 = 1 << amount;
        self.scale(fact, width, factor)
    }

    /// Offsets a value with a fact by a known amount.
    pub fn offset(&self, fact: &Fact, width: u16, offset: i64) -> Option<Fact> {
        if offset == 0 {
            return Some(fact.clone());
        }

        let compute_offset = |base: u64| -> Option<u64> {
            // `unsigned_abs`, not `-offset`: a hostile constant can be
            // `i64::MIN`, whose negation overflows.
            if offset >= 0 {
                base.checked_add(offset.unsigned_abs())
            } else {
                base.checked_sub(offset.unsigned_abs())
            }
        };

        let result = match fact {
            Fact::Range {
                bit_width,
                min,
                max,
            } if *bit_width == width => {
                let min = compute_offset(*min)?;
                let max = compute_offset(*max)?;
                // An offset that leaves the width wraps at it.
                if max > max_value_for_width(width) {
                    return None;
                }
                Some(Fact::Range {
                    bit_width: *bit_width,
                    min,
                    max,
                })
            }
            Fact::DynamicRange {
                bit_width,
                min,
                max,
            } if *bit_width == width => {
                let min = Expr::offset(min, offset)?;
                let max = Expr::offset(max, offset)?;
                Some(Fact::DynamicRange {
                    bit_width: *bit_width,
                    min,
                    max,
                })
            }
            Fact::Mem {
                ty,
                min_offset: mem_min_offset,
                max_offset: mem_max_offset,
                nullable: false,
            } if width >= self.pointer_width => {
                let min_offset = compute_offset(*mem_min_offset)?;
                let max_offset = compute_offset(*mem_max_offset)?;
                Some(Fact::Mem {
                    ty: *ty,
                    min_offset,
                    max_offset,
                    nullable: false,
                })
            }
            Fact::DynamicMem {
                ty,
                min,
                max,
                nullable: false,
            } if width >= self.pointer_width => {
                let min = Expr::offset(min, offset)?;
                let max = Expr::offset(max, offset)?;
                Some(Fact::DynamicMem {
                    ty: *ty,
                    min,
                    max,
                    nullable: false,
                })
            }
            _ => None,
        };
        trace!("offset: {fact:?} + {offset} in width {width} -> {result:?}");
        result
    }

    /// Check that accessing memory via a pointer with this fact, with
    /// a memory access of the given size, is valid.
    ///
    /// If valid, returns the memory type and offset into that type
    /// that this address accesses, if known, or `None` if the range
    /// doesn't constrain the access to exactly one location.
    fn check_address(&self, fact: &Fact, size: u32) -> PccResult<Option<(ir::MemoryType, u64)>> {
        trace!("check_address: fact {:?} size {}", fact, size);
        match fact {
            Fact::Mem {
                ty,
                min_offset,
                max_offset,
                nullable: _,
            } => {
                let end_offset: u64 = max_offset
                    .checked_add(u64::from(size))
                    .ok_or(PccError::Overflow)?;
                match &self.function.memory_types[*ty] {
                    ir::MemoryTypeData::Struct { size, .. }
                    | ir::MemoryTypeData::Memory { size } => {
                        ensure!(end_offset <= *size, OutOfBounds)
                    }
                    ir::MemoryTypeData::DynamicMemory { .. } => bail!(OutOfBounds),
                    ir::MemoryTypeData::Empty => bail!(OutOfBounds),
                }
                let specific_ty_and_offset = if min_offset == max_offset {
                    Some((*ty, *min_offset))
                } else {
                    None
                };
                Ok(specific_ty_and_offset)
            }
            // (The lower bound must be at or above the memory's start.)
            Fact::DynamicMem {
                ty,
                min,
                max:
                    Expr {
                        base: BaseExpr::GlobalValue(max_gv),
                        offset: max_offset,
                    },
                nullable: _,
            } if Expr::le(&Expr::constant(0), min) => match &self.function.memory_types[*ty] {
                ir::MemoryTypeData::DynamicMemory {
                    gv,
                    size: mem_static_size,
                } if gv == max_gv => {
                    let end_offset = max_offset
                        .checked_add(i64::from(size))
                        .ok_or(PccError::Overflow)?;
                    let mem_static_size =
                        i64::try_from(*mem_static_size).map_err(|_| PccError::Overflow)?;
                    ensure!(end_offset <= mem_static_size, OutOfBounds);
                    Ok(None)
                }
                _ => bail!(OutOfBounds),
            },
            // The null pointer itself: the egraph can prove a Spectre guard
            // always selects its null arm (a constant index that is always
            // out of bounds), leaving a load from address 0. The same
            // assumption that admits `nullable` pointers above admits it: a
            // small access at address 0 traps, because the first pages of
            // the address space are never mapped (Linux `mmap_min_addr`,
            // 64 KiB by default; Wasmtime's load-from-zero trap relies on it).
            Fact::Range {
                bit_width,
                min: 0,
                max: 0,
            } if *bit_width == self.pointer_width && size <= 4096 => Ok(None),
            _ => bail!(OutOfBounds),
        }
    }

    /// Get the access struct field, if any, by a pointer with the
    /// given fact and an access of the given type.
    pub fn struct_field<'b>(
        &'b self,
        fact: &Fact,
        access_ty: ir::Type,
    ) -> PccResult<Option<&'b ir::MemoryTypeField>> {
        let (ty, offset) = match self.check_address(fact, access_ty.bytes())? {
            Some((ty, offset)) => (ty, offset),
            None => return Ok(None),
        };

        if let ir::MemoryTypeData::Struct { fields, .. } = &self.function.memory_types[ty] {
            let field = fields
                .iter()
                .find(|field| field.offset == offset)
                .ok_or(PccError::InvalidFieldOffset)?;
            if field.ty != access_ty {
                bail!(BadFieldType);
            }
            Ok(Some(field))
        } else {
            // Access to valid memory, but not a struct: no facts can be attached to the result.
            Ok(None)
        }
    }

    /// Check a load, and determine what fact, if any, the result of the load might have.
    pub fn load<'b>(&'b self, fact: &Fact, access_ty: ir::Type) -> PccResult<Option<&'b Fact>> {
        Ok(self
            .struct_field(fact, access_ty)?
            .and_then(|field| field.fact()))
    }

    /// Check a store.
    pub fn store(
        &self,
        fact: &Fact,
        access_ty: ir::Type,
        data_fact: Option<&Fact>,
    ) -> PccResult<()> {
        if let Some(field) = self.struct_field(fact, access_ty)? {
            // If it's a read-only field, disallow.
            if field.readonly {
                bail!(WriteToReadOnlyField);
            }
            // Check that the fact on the stored data subsumes the field's fact.
            if !self.subsumes_fact_optionals(data_fact, field.fact()) {
                bail!(InvalidStoredFact);
            }
        }
        Ok(())
    }

    /// Apply a known inequality to rewrite dynamic bounds using transitivity, if possible.
    ///
    /// Given that `lhs >= rhs` (if not `strict`) or `lhs > rhs` (if
    /// `strict`), update `fact`.
    pub fn apply_inequality(
        &self,
        fact: &Fact,
        lhs: &Fact,
        rhs: &Fact,
        kind: InequalityKind,
    ) -> Fact {
        let result = match (
            lhs.as_symbol(),
            lhs.as_const(self.pointer_width)
                .and_then(|k| i64::try_from(k).ok()),
            rhs.as_symbol(),
            fact,
        ) {
            (
                Some(lhs),
                None,
                Some(rhs),
                Fact::DynamicMem {
                    ty,
                    min,
                    max,
                    nullable,
                },
            ) if rhs.base == max.base => {
                let strict_offset = match kind {
                    InequalityKind::Strict => 1,
                    InequalityKind::Loose => 0,
                };
                if let Some(offset) = max
                    .offset
                    .checked_add(lhs.offset)
                    .and_then(|x| x.checked_sub(rhs.offset))
                    .and_then(|x| x.checked_sub(strict_offset))
                {
                    let new_max = Expr {
                        base: lhs.base.clone(),
                        offset,
                    };
                    Fact::DynamicMem {
                        ty: *ty,
                        min: min.clone(),
                        max: new_max,
                        nullable: *nullable,
                    }
                } else {
                    fact.clone()
                }
            }

            // The pointer's lower bound must be provably at or above the
            // memory's start: a constant or symbol plus a non-negative
            // offset (the result keeps that constant lower bound).
            (
                None,
                Some(lhs_const),
                Some(rhs),
                Fact::DynamicMem {
                    ty,
                    min,
                    max,
                    nullable,
                },
            ) if rhs.base == max.base
                && min.offset >= 0
                && matches!(
                    min.base,
                    BaseExpr::None | BaseExpr::Value(_) | BaseExpr::GlobalValue(_)
                ) =>
            {
                let min_offset = match min.base {
                    BaseExpr::None => u64::try_from(min.offset).unwrap(),
                    _ => 0,
                };
                let strict_offset = match kind {
                    InequalityKind::Strict => 1,
                    InequalityKind::Loose => 0,
                };
                if let Some(offset) = max
                    .offset
                    .checked_add(lhs_const)
                    .and_then(|x| x.checked_sub(rhs.offset))
                    .and_then(|x| x.checked_sub(strict_offset))
                    && let Ok(max_offset) = u64::try_from(offset)
                    && min_offset <= max_offset
                {
                    Fact::Mem {
                        ty: *ty,
                        min_offset,
                        max_offset,
                        nullable: *nullable,
                    }
                } else {
                    fact.clone()
                }
            }

            _ => fact.clone(),
        };
        trace!("apply_inequality({fact:?}, {lhs:?}, {rhs:?}, {kind:?} -> {result:?}");
        result
    }

    /// Compute the union of two facts, if possible.
    pub fn union(&self, lhs: &Fact, rhs: &Fact) -> Option<Fact> {
        let result = match (lhs, rhs) {
            (lhs, rhs) if lhs == rhs => Some(lhs.clone()),

            (
                Fact::Mem {
                    ty: ty_lhs,
                    min_offset: min_lhs,
                    max_offset: max_lhs,
                    nullable: nullable_lhs,
                },
                Fact::Mem {
                    ty: ty_rhs,
                    min_offset: min_rhs,
                    max_offset: max_rhs,
                    nullable: nullable_rhs,
                },
            ) if ty_lhs == ty_rhs => Some(Fact::Mem {
                ty: *ty_lhs,
                min_offset: core::cmp::min(*min_lhs, *min_rhs),
                max_offset: core::cmp::max(*max_lhs, *max_rhs),
                nullable: *nullable_lhs || *nullable_rhs,
            }),

            (
                Fact::Range {
                    bit_width: bw_lhs,
                    min: min_lhs,
                    max: max_lhs,
                },
                Fact::Range {
                    bit_width: bw_rhs,
                    min: min_rhs,
                    max: max_rhs,
                },
            ) if bw_lhs == bw_rhs => Some(Fact::Range {
                bit_width: *bw_lhs,
                min: core::cmp::min(*min_lhs, *min_rhs),
                max: core::cmp::max(*max_lhs, *max_rhs),
            }),

            (
                Fact::DynamicMem {
                    ty: ty_lhs,
                    min: min_lhs,
                    max: max_lhs,
                    nullable: nullable_lhs,
                },
                Fact::DynamicMem {
                    ty: ty_rhs,
                    min: min_rhs,
                    max: max_rhs,
                    nullable: nullable_rhs,
                },
            ) if ty_lhs == ty_rhs => Some(Fact::DynamicMem {
                ty: *ty_lhs,
                min: Expr::min(min_lhs, min_rhs),
                max: Expr::max(max_lhs, max_rhs),
                nullable: *nullable_lhs || *nullable_rhs,
            }),

            (
                Fact::Range {
                    bit_width: bw_const,
                    min: 0,
                    max: 0,
                },
                Fact::DynamicMem {
                    ty,
                    min,
                    max,
                    nullable: _,
                },
            )
            | (
                Fact::DynamicMem {
                    ty,
                    min,
                    max,
                    nullable: _,
                },
                Fact::Range {
                    bit_width: bw_const,
                    min: 0,
                    max: 0,
                },
            ) if *bw_const == self.pointer_width => Some(Fact::DynamicMem {
                ty: *ty,
                min: min.clone(),
                max: max.clone(),
                nullable: true,
            }),

            (
                Fact::Range {
                    bit_width: bw_const,
                    min: 0,
                    max: 0,
                },
                Fact::Mem {
                    ty,
                    min_offset,
                    max_offset,
                    nullable: _,
                },
            )
            | (
                Fact::Mem {
                    ty,
                    min_offset,
                    max_offset,
                    nullable: _,
                },
                Fact::Range {
                    bit_width: bw_const,
                    min: 0,
                    max: 0,
                },
            ) if *bw_const == self.pointer_width => Some(Fact::Mem {
                ty: *ty,
                min_offset: *min_offset,
                max_offset: *max_offset,
                nullable: true,
            }),

            _ => None,
        };
        trace!("union({lhs:?}, {rhs:?}) -> {result:?}");
        result
    }
}

fn max_value_for_width(bits: u16) -> u64 {
    assert!(bits <= 64);
    if bits == 64 {
        u64::MAX
    } else {
        (1u64 << bits) - 1
    }
}

/// Structural equality of VCode registers, for symbols defined more than
/// once. A register's shape is its defining instruction's text, with the
/// defined register written `DEF` and each input register replaced by the
/// input's own shape, to a bounded depth. An instruction that may touch
/// memory (its text has an address operand: `(...)` without `<-` on x64,
/// other than `lea` and constant-pool `(%rip)` reads; `[...]` on aarch64),
/// defines more than one register, or is past the
/// depth bound, has its register's own name as its shape, so it is equal
/// only to itself.
#[derive(Default)]
struct StructEq {
    def_inst: Option<crate::FxHashMap<usize, InsnIndex>>,
    /// The shape of each vreg seen so far, as an interned id (`#n`) or, for
    /// a vreg whose definition is not known pure, its own name.
    memo: crate::FxHashMap<usize, alloc::string::String>,
    /// Shape text (with inputs written as their shapes) to its id, so that
    /// shapes stay short however deep or shared the expression is.
    interned: crate::FxHashMap<alloc::string::String, usize>,
}

impl StructEq {
    /// Recursion bound; past it a vreg is named by itself (never equal to
    /// another), which is conservative.
    const DEPTH: u8 = 48;

    fn equal<I: crate::machinst::VCodeInst>(&mut self, vcode: &VCode<I>, a: VReg, b: VReg) -> bool {
        a == b
            || self.shape(vcode, a.vreg(), Self::DEPTH) == self.shape(vcode, b.vreg(), Self::DEPTH)
    }

    fn shape<I: crate::machinst::VCodeInst>(
        &mut self,
        vcode: &VCode<I>,
        v: usize,
        depth: u8,
    ) -> alloc::string::String {
        if let Some(s) = self.memo.get(&v) {
            return s.clone();
        }
        let def_inst = self.def_inst.get_or_insert_with(|| {
            let mut m = crate::FxHashMap::default();
            for i in 0..vcode.num_insts() {
                let i = InsnIndex::new(i);
                for op in vcode.inst_operands(i) {
                    if op.kind() == regalloc2::OperandKind::Def {
                        m.insert(op.vreg().vreg(), i);
                    }
                }
            }
            m
        });
        let own = alloc::format!("%v{v}");
        let shape = match def_inst.get(&v).copied() {
            Some(inst) if depth > 0 => match self.inst_shape(vcode, inst, Some(v), depth) {
                Some(text) => {
                    let next = self.interned.len();
                    let id = *self.interned.entry(text).or_insert(next);
                    alloc::format!("#{id}")
                }
                None => own,
            },
            _ => own,
        };
        // A shape cut off by the depth bound is the vreg's own name, which
        // is safe to remember: it is equal to nothing else.
        self.memo.insert(v, shape.clone());
        shape
    }

    /// The shape of instruction `inst` (with `def` written `DEF`), or `None`
    /// if it may read anything but its register operands. A flag reader
    /// (`cmov`, `set`) is included with the `cmp` or `test` right before it,
    /// which is where its flags come from.
    fn inst_shape<I: crate::machinst::VCodeInst>(
        &mut self,
        vcode: &VCode<I>,
        inst: InsnIndex,
        def: Option<usize>,
        depth: u8,
    ) -> Option<alloc::string::String> {
        let ops = vcode.inst_operands(inst);
        // A value's instruction may define other registers too (a widening
        // multiply's high half): those are written `ODEF`.
        let other_defs: alloc::vec::Vec<usize> = ops
            .iter()
            .filter(|o| o.kind() == regalloc2::OperandKind::Def)
            .map(|o| o.vreg().vreg())
            .filter(|&d| Some(d) != def)
            .collect();
        let has_def = ops
            .iter()
            .any(|o| o.kind() == regalloc2::OperandKind::Def && Some(o.vreg().vreg()) == def);
        if has_def != def.is_some() || (def.is_none() && !other_defs.is_empty()) {
            return None;
        }
        let text = alloc::format!("{:?}", vcode[inst]);
        // Plain integer ALU and move instructions read nothing but their
        // register operands; `cmp`/`test` (no defs) only feed a flag reader;
        // `cmov`/`set` read the flags, handled below. (x64 `lea` has an
        // address operand but reads no memory.)
        const PURE: &[&str] = &[
            "mov", "lea", "add", "sub", "and", "or", "xor", "shl", "shr", "sar", "rol", "ror",
            "imul", "neg", "not", "eor", "lsl", "lsr", "asr", "ubfx", "sbfx", "uxt", "sxt",
            "tzcnt", "lzcnt", "popcnt", "bswap", "clz", "rbit", "mul",
        ];
        const FLAGS_IN: &[&str] = &["cmov", "set"];
        const FLAGS_OUT: &[&str] = &["cmp", "test"];
        let mnemonic = text.trim_start();
        let starts = |set: &[&str]| set.iter().any(|p| mnemonic.starts_with(p));
        let flags_in = starts(FLAGS_IN);
        let known = if def.is_some() {
            starts(PURE) || flags_in
        } else {
            starts(FLAGS_OUT)
        };
        let is_lea = mnemonic.starts_with("lea");
        let touches_memory = text.contains('[')
            || (!is_lea
                && text
                    .split('(')
                    .skip(1)
                    .map(|g| g.split(')').next().unwrap_or(""))
                    .any(|g| !g.contains("<-") && g != "%rip"));
        // `bsf`/`bsr` (not listed) leave the destination undefined on a
        // zero input. A physical register in the text (`movq %rsp, %v1`, a
        // pinned register) is read at a point in time, so two such reads
        // are not the same value.
        if !known || touches_memory || mentions_preg(&text) {
            return None;
        }
        let uses: alloc::vec::Vec<usize> = ops
            .iter()
            .filter(|o| o.kind() == regalloc2::OperandKind::Use)
            .map(|o| o.vreg().vreg())
            .collect();
        // `xor r, r` is zero whatever `r` holds (the zeroing idiom, whose
        // input is often an `uninit` vreg).
        // Only the 32- and 64-bit forms zero the whole register: `xorb` and
        // `xorw` keep the upper bits.
        let zeroing = ["xorl", "xorq", "eor"].iter().any(|p| mnemonic.starts_with(p));
        if zeroing && uses.len() == 2 && uses[0] == uses[1] {
            return Some(alloc::string::String::from("ZERO"));
        }
        let mut text = match def {
            Some(v) => replace_vreg(&text, v, "DEF"),
            None => text,
        };
        for &d in &other_defs {
            text = replace_vreg(&text, d, "ODEF");
        }
        // Mark the inputs first, then substitute their shapes, so that a
        // register named inside an input's shape is never replaced again.
        for (k, &u) in uses.iter().enumerate() {
            text = replace_vreg(&text, u, &alloc::format!("\u{1}{k}\u{1}"));
        }
        for (k, &u) in uses.iter().enumerate() {
            let inner = self.shape(vcode, u, depth - 1);
            text = text.replace(
                &alloc::format!("\u{1}{k}\u{1}"),
                &alloc::format!("{{{inner}}}"),
            );
        }
        if flags_in {
            // The flags come from the instruction right before, which must
            // be a compare (a block starts after a branch, never a compare).
            let prev = InsnIndex::new(inst.index().checked_sub(1)?);
            let flags = self.inst_shape(vcode, prev, None, depth)?;
            text.push_str(" |flags ");
            text.push_str(&flags);
        }
        Some(text)
    }
}

/// `text` with each register token `%v<v>` replaced by `with`.
/// Does a pre-regalloc instruction's text name a physical register? Virtual
/// registers print as `%v<n>`. x64 physical registers print with `%`
/// (`%rsp`; `%rip` is a constant address); aarch64 ones bare (`x21`, `w3`,
/// `sp`, `fp`, `lr`, `xzr`).
fn mentions_preg(text: &str) -> bool {
    let x64 = text.match_indices('%').any(|(i, _)| {
        let rest = &text[i + 1..];
        !(rest.starts_with('v') && rest[1..].starts_with(|c: char| c.is_ascii_digit()))
            && !rest.starts_with("rip")
    });
    let aarch64 = text
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '%')
        .any(|t| {
            matches!(t, "sp" | "fp" | "lr" | "xzr" | "wzr")
                || (t.len() >= 2
                    && (t.starts_with('x') || t.starts_with('w'))
                    && t[1..].chars().all(|c| c.is_ascii_digit()))
        });
    x64 || aarch64
}

fn replace_vreg(text: &str, v: usize, with: &str) -> alloc::string::String {
    let name = alloc::format!("%v{v}");
    let mut out = alloc::string::String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find(&name) {
        let after = &rest[i + name.len()..];
        out.push_str(&rest[..i]);
        if after.starts_with(|c: char| c.is_ascii_digit()) {
            out.push_str(&name);
        } else {
            out.push_str(with);
        }
        rest = after;
    }
    out.push_str(rest);
    out
}

/// Top-level entry point after compilation: this checks the facts in
/// VCode.
pub fn check_vcode_facts<B: LowerBackend + TargetIsa>(
    f: &ir::Function,
    vcode: &mut VCode<B::MInst>,
    backend: &B,
) -> PccResult<()> {
    let ctx = FactContext::new(f, backend.triple().pointer_width().unwrap().bits().into());
    let mut first_err = None;
    if log::log_enabled!(log::Level::Debug) {
        let checked = f
            .layout
            .blocks()
            .flat_map(|b| f.layout.block_insts(b))
            .filter(|&i| {
                f.dfg.insts[i]
                    .memflags_data(&f.dfg)
                    .is_some_and(|m| m.checked())
            })
            .count();
        log::debug!("pcc: {} has {checked} checked CLIF memory ops", f.name);
    }

    // Each symbol (a `Def` fact) must name one value. Several registers may
    // define the same symbol (the egraph elaborates or rematerializes a pure
    // value more than once and copies its facts), but only when their
    // definitions are structurally equal: the same pure instruction over
    // inputs that are themselves structurally equal (see `StructEq`).
    // Otherwise a compare on one could justify an access through another.
    let mut symbol_defs: crate::FxHashMap<ir::Value, VReg> = Default::default();
    let mut structural = StructEq::default();

    // Check that individual instructions are valid according to input
    // facts, and support the stated output facts.
    for block in 0..vcode.num_blocks() {
        let block = BlockIndex::new(block);
        let mut flow_state = B::FactFlowState::default();
        for inst in vcode.block_insns(block).iter() {
            for vreg in vcode.inst_defs_with_facts(inst) {
                let Some(Fact::Def { value }) = vcode.vreg_fact(vreg) else {
                    continue;
                };
                let value = *value;
                match symbol_defs.get(&value) {
                    None => {
                        symbol_defs.insert(value, vreg);
                    }
                    Some(&first) if structural.equal(vcode, first, vreg) => {}
                    Some(&first) => {
                        log::info!(
                            "PCC error SymbolRedefined in {}: {value} by {:?}, first by {first:?}",
                            f.name,
                            vcode[inst]
                        );
                        log::debug!(
                            "pcc: shapes {} vs {}",
                            structural.shape(vcode, first.vreg(), StructEq::DEPTH),
                            structural.shape(vcode, vreg.vreg(), StructEq::DEPTH)
                        );
                        first_err.get_or_insert(PccError::UnsupportedFact);
                    }
                }
            }

            // `Args` defines the entry block's parameters, whose CLIF facts
            // are trusted axioms. Accept a fact on such a register only if
            // the CLIF fact recorded at lowering implies it: a vreg alias
            // can merge a stronger fact into it later, and that one is not
            // trusted.
            if vcode[inst].is_args() {
                for vreg in vcode.inst_defs_with_facts(inst) {
                    let fact = vcode.vreg_fact(vreg).unwrap();
                    let ok = vcode
                        .pcc_entry_facts
                        .get(&vreg)
                        .is_some_and(|trusted| ctx.subsumes(trusted, fact));
                    if !ok {
                        log::info!(
                            "PCC error UnsupportedFact in {}: untrusted fact {fact} on argument {vreg:?}",
                            f.name
                        );
                        first_err.get_or_insert(PccError::UnsupportedFact);
                    }
                }
            }

            // Check any output facts on this inst. If that fails, retry
            // with every `Def` input read as the fact derived at its def
            // instead of its symbol: a value can carry a symbol (for a
            // bounds-check comparison) and also be used where only its
            // numeric range proves the access (an access whose check was
            // elided), once the egraph merges the two uses.
            let saved = flow_state.clone();
            if let Err(e) = backend.check_fact(&ctx, vcode, inst, &mut flow_state) {
                vcode.pcc_numeric = true;
                let mut retry_state = saved;
                let retry = backend.check_fact(&ctx, vcode, inst, &mut retry_state);
                vcode.pcc_numeric = false;
                match retry {
                    Ok(()) => flow_state = retry_state,
                    Err(_) => {
                        // Keep going so the log lists every failing
                        // instruction in the function, then report the
                        // first error.
                        log::info!("PCC error {e:?} in {}: {:?}", f.name, vcode[inst]);
                        first_err.get_or_insert(e);
                    }
                }
            }

            // If this is a branch, check that all block arguments subsume
            // the assumed facts on the blockparams of successors.
            if vcode.is_branch(inst) {
                for (succ_idx, succ) in vcode.block_succs(block).iter().enumerate() {
                    for (arg, param) in vcode
                        .branch_blockparams(block, inst, succ_idx)
                        .iter()
                        .zip(vcode.block_params(*succ).iter())
                    {
                        let arg_fact = vcode.vreg_fact(*arg);
                        let param_fact = vcode.vreg_fact(*param);
                        if !ctx.subsumes_fact_optionals(arg_fact, param_fact) {
                            log::info!(
                                "PCC error UnsupportedBlockparam in {}: {arg_fact:?} -> {param_fact:?}",
                                f.name
                            );
                            first_err.get_or_insert(PccError::UnsupportedBlockparam);
                        }
                    }
                }
            }
        }
    }
    if first_err.is_some() {
        log::debug!(
            "pcc: VCode of {} after a failed check:\n{:?}",
            f.name,
            vcode
        );
    }
    first_err.map_or(Ok(()), Err)
}
