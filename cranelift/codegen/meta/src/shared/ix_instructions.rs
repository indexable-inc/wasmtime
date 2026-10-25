//! Instructions for the `ix` intrinsic imports (indexable fork).
//!
//! Wasmtime lowers calls to a fixed set of imports in the reserved `ix`
//! module inline (see `crates/cranelift/src/ix_intrinsics.rs`). Each one
//! maps onto one of the instructions below, which name a single host
//! instruction. The x86 and aarch64 forms follow the hardware's own
//! semantics; the translator composes them into one portable meaning per
//! intrinsic. They are kept in this file so the fork's diff to upstream's
//! `instructions.rs` is one call.
#![expect(non_snake_case, reason = "DSL style here")]

use crate::cdsl::instructions::{
    AllInstructions, InstructionBuilder as Inst, InstructionGroupBuilder,
};
use crate::cdsl::operands::Operand;
use crate::cdsl::typevar::{TypeSetBuilder, TypeVar};
use crate::shared::formats::Formats;
use crate::shared::immediates::Immediates;

pub(crate) fn define(all_instructions: &mut AllInstructions, formats: &Formats, imm: &Immediates) {
    let ig = &mut InstructionGroupBuilder::new(all_instructions);
    // The crypto instructions treat a vector register as 128 raw bits; they
    // all use `i8x16` so the translator never needs a bitcast.
    let V = &TypeVar::new(
        "V",
        "A 128-bit vector, as 16 bytes",
        TypeSetBuilder::new()
            .ints(8..8)
            .simd_lanes(16..16)
            .includes_scalars(false)
            .build(),
    );
    let i32_ = &TypeVar::new(
        "i32_",
        "A 32-bit integer",
        TypeSetBuilder::new().ints(32..32).build(),
    );
    let CrcData = &TypeVar::new(
        "CrcData",
        "The integer a CRC step consumes (i8, i32 or i64)",
        TypeSetBuilder::new().ints(8..64).build(),
    );
    let iBits = &TypeVar::new(
        "iBits",
        "A 32- or 64-bit integer",
        TypeSetBuilder::new().ints(32..64).build(),
    );
    let i64_ = &TypeVar::new(
        "i64_",
        "A 64-bit integer",
        TypeSetBuilder::new().ints(64..64).build(),
    );

    let vv = |ig: &mut InstructionGroupBuilder, name: &'static str, doc: &'static str| {
        ig.push(
            Inst::new(name, doc, &formats.unary)
                .operands_in(&[Operand::new("x", V)])
                .operands_out(&[Operand::new("a", V)]),
        );
    };
    let vvv = |ig: &mut InstructionGroupBuilder, name: &'static str, doc: &'static str| {
        ig.push(
            Inst::new(name, doc, &formats.binary)
                .operands_in(&[Operand::new("x", V), Operand::new("y", V)])
                .operands_out(&[Operand::new("a", V)]),
        );
    };
    let vvvv = |ig: &mut InstructionGroupBuilder, name: &'static str, doc: &'static str| {
        ig.push(
            Inst::new(name, doc, &formats.ternary)
                .operands_in(&[
                    Operand::new("x", V),
                    Operand::new("y", V),
                    Operand::new("z", V),
                ])
                .operands_out(&[Operand::new("a", V)]),
        );
    };

    // x86 (AES-NI, PCLMULQDQ, SHA).
    vvv(ig, "x86_aesenc", "x86 `aesenc`: MixColumns(ShiftRows(SubBytes(x))) ^ y.");
    vvv(ig, "x86_aesenclast", "x86 `aesenclast`: ShiftRows(SubBytes(x)) ^ y.");
    vvv(ig, "x86_aesdec", "x86 `aesdec`: InvMixColumns(InvShiftRows(InvSubBytes(x))) ^ y.");
    vvv(ig, "x86_aesdeclast", "x86 `aesdeclast`: InvShiftRows(InvSubBytes(x)) ^ y.");
    vv(ig, "x86_aesimc", "x86 `aesimc`: InvMixColumns(x).");
    ig.push(
        Inst::new(
            "x86_pclmulqdq",
            r#"
        x86 `pclmulqdq`: carry-less product of one 64-bit lane of `x` (bit 0
        of `imm`) and one of `y` (bit 4 of `imm`), as a 128-bit value.
            "#,
            &formats.ternary_imm8,
        )
        .operands_in(&[
            Operand::new("x", V),
            Operand::new("y", V),
            Operand::new("imm", &imm.uimm8),
        ])
        .operands_out(&[Operand::new("a", V)]),
    );
    vvvv(
        ig,
        "x86_sha256rnds2",
        "x86 `sha256rnds2`: two rounds; x = CDGH, y = ABEF, z = WK (low 64 bits).",
    );
    vvv(ig, "x86_sha256msg1", "x86 `sha256msg1`.");
    vvv(ig, "x86_sha256msg2", "x86 `sha256msg2`.");

    // aarch64 (FEAT_AES, FEAT_PMULL, FEAT_SHA256).
    vvv(ig, "aarch64_aese", "aarch64 `aese`: ShiftRows(SubBytes(x ^ y)).");
    vvv(ig, "aarch64_aesd", "aarch64 `aesd`: InvShiftRows(InvSubBytes(x ^ y)).");
    vv(ig, "aarch64_aesmc", "aarch64 `aesmc`: MixColumns(x).");
    vv(ig, "aarch64_aesimc", "aarch64 `aesimc`: InvMixColumns(x).");
    vvv(ig, "aarch64_pmull", "aarch64 `pmull` (1Q): lane 0 of x times lane 0 of y.");
    vvv(ig, "aarch64_pmull2", "aarch64 `pmull2` (1Q): lane 1 of x times lane 1 of y.");
    vvvv(ig, "aarch64_sha256h", "aarch64 `sha256h`: x = ABCD, y = EFGH, z = WK.");
    vvvv(ig, "aarch64_sha256h2", "aarch64 `sha256h2`: x = EFGH, y = ABCD, z = WK.");
    vvv(ig, "aarch64_sha256su0", "aarch64 `sha256su0`.");
    vvvv(ig, "aarch64_sha256su1", "aarch64 `sha256su1`.");

    // The same meaning on both architectures.
    ig.push(
        Inst::new(
            "ix_crc32c",
            r#"
        One CRC-32C (Castagnoli) step: fold the bits of `x` into `acc`.

        This is the hardware instruction (x86 `crc32`, aarch64 `crc32c*`):
        no pre- or post-inversion.
            "#,
            &formats.binary,
        )
        .operands_in(&[Operand::new("x", CrcData), Operand::new("acc", i32_)])
        .operands_out(&[Operand::new("a", i32_)]),
    );
    ig.push(
        Inst::new(
            "ix_pext",
            "Parallel bit extract: gather the bits of `x` selected by `mask` into the low bits.",
            &formats.binary,
        )
        .operands_in(&[Operand::new("x", iBits), Operand::new("mask", iBits)])
        .operands_out(&[Operand::new("a", iBits)]),
    );
    ig.push(
        Inst::new(
            "ix_pdep",
            "Parallel bit deposit: scatter the low bits of `x` to the positions set in `mask`.",
            &formats.binary,
        )
        .operands_in(&[Operand::new("x", iBits), Operand::new("mask", iBits)])
        .operands_out(&[Operand::new("a", iBits)]),
    );
    ig.push(
        Inst::new(
            "ix_prefetch",
            r#"
        Prefetch hint for the cache line at host address `p`.

        `hint` bit 0 is set for a write, bits 1-2 give the locality (0 = no
        reuse, 3 = keep in all levels). It never faults; the caller makes
        sure `p` is inside the guest's memory.
            "#,
            &formats.binary_imm8,
        )
        .operands_in(&[Operand::new("p", i64_), Operand::new("hint", &imm.uimm8)])
        .other_side_effects(),
    );
}
