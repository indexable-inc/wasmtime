//! Instructions behind the `ix` intrinsic imports (indexable fork): AES-NI,
//! PCLMULQDQ, SHA-256, CRC32, BMI2 PEXT/PDEP and prefetch.

use crate::dsl::{Feature::*, Inst, Length::*, Location::*};
use crate::dsl::{align, fmt, inst, r, rex, rw, vex, w};

#[rustfmt::skip] // Keeps instructions on a single line.
pub fn list() -> Vec<Inst> {
    vec![
        inst("aesenc", fmt("A", [rw(xmm1), r(align(xmm_m128))]), rex([0x66, 0x0F, 0x38, 0xDC]), (_64b | compat) & aes),
        inst("aesenclast", fmt("A", [rw(xmm1), r(align(xmm_m128))]), rex([0x66, 0x0F, 0x38, 0xDD]), (_64b | compat) & aes),
        inst("aesdec", fmt("A", [rw(xmm1), r(align(xmm_m128))]), rex([0x66, 0x0F, 0x38, 0xDE]), (_64b | compat) & aes),
        inst("aesdeclast", fmt("A", [rw(xmm1), r(align(xmm_m128))]), rex([0x66, 0x0F, 0x38, 0xDF]), (_64b | compat) & aes),
        inst("aesimc", fmt("A", [w(xmm1), r(align(xmm_m128))]), rex([0x66, 0x0F, 0x38, 0xDB]), (_64b | compat) & aes),
        inst("pclmulqdq", fmt("A", [rw(xmm1), r(align(xmm_m128)), r(imm8)]), rex([0x66, 0x0F, 0x3A, 0x44]).ib(), (_64b | compat) & pclmulqdq),
        inst("sha256rnds2", fmt("A", [rw(xmm1), r(align(xmm_m128)), r(xmm0)]), rex([0x0F, 0x38, 0xCB]), (_64b | compat) & sha),
        inst("sha256msg1", fmt("A", [rw(xmm1), r(align(xmm_m128))]), rex([0x0F, 0x38, 0xCC]), (_64b | compat) & sha),
        inst("sha256msg2", fmt("A", [rw(xmm1), r(align(xmm_m128))]), rex([0x0F, 0x38, 0xCD]), (_64b | compat) & sha),

        inst("crc32b", fmt("RM", [rw(r32), r(rm8)]), rex([0xF2, 0x0F, 0x38, 0xF0]).r(), (_64b | compat) & sse42),
        inst("crc32l", fmt("RM", [rw(r32), r(rm32)]), rex([0xF2, 0x0F, 0x38, 0xF1]).r(), (_64b | compat) & sse42),
        inst("crc32q", fmt("RM", [rw(r64), r(rm64)]), rex([0xF2, 0x0F, 0x38, 0xF1]).r().w(), _64b & sse42),

        inst("pextl", fmt("RVM", [w(r32a), r(r32b), r(rm32)]), vex(LZ)._f3()._0f38().w0().op(0xF5), (_64b | compat) & bmi2),
        inst("pextq", fmt("RVM", [w(r64a), r(r64b), r(rm64)]), vex(LZ)._f3()._0f38().w1().op(0xF5), _64b & bmi2),
        inst("pdepl", fmt("RVM", [w(r32a), r(r32b), r(rm32)]), vex(LZ)._f2()._0f38().w0().op(0xF5), (_64b | compat) & bmi2),
        inst("pdepq", fmt("RVM", [w(r64a), r(r64b), r(rm64)]), vex(LZ)._f2()._0f38().w1().op(0xF5), _64b & bmi2),

        inst("prefetchnta", fmt("M", [r(m8)]), rex([0x0F, 0x18]).digit(0), _64b | compat),
        inst("prefetcht0", fmt("M", [r(m8)]), rex([0x0F, 0x18]).digit(1), _64b | compat),
        inst("prefetcht1", fmt("M", [r(m8)]), rex([0x0F, 0x18]).digit(2), _64b | compat),
        inst("prefetcht2", fmt("M", [r(m8)]), rex([0x0F, 0x18]).digit(3), _64b | compat),
        inst("prefetchw", fmt("M", [r(m8)]), rex([0x0F, 0x0D]).digit(1), _64b | compat),
    ]
}
