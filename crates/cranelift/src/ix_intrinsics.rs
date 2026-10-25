//! Inline lowering of `ix` intrinsic imports (indexable fork).
//!
//! `wasmtime_environ::IxIntrinsic` lists the imports and their Wasm
//! signatures. A direct `call` (or `return_call`) to one lands here instead of
//! becoming a call: the arguments are popped from the operand stack and the
//! result is computed with the host's own instruction (see
//! `cranelift/codegen/meta/src/shared/ix_instructions.rs`). There is no
//! portable fallback: if the target lacks a feature the module fails to
//! compile with an error naming it.

use crate::func_environ::FuncEnvironment;
use crate::translate::TargetEnvironment;
use cranelift_codegen::ir::condcodes::IntCC;
use cranelift_codegen::ir::types::{I8, I8X16, I32X4, I64, I64X2};
use cranelift_codegen::ir::{self, ConstantData, InstBuilder, MemFlagsData, Value};
use cranelift_frontend::FunctionBuilder;
use target_lexicon::Architecture;
use wasmtime_environ::{IxIntrinsic, MemoryIndex, WasmResult, wasm_unsupported};

#[derive(Copy, Clone, PartialEq)]
enum Arch {
    X64,
    Aarch64,
}

/// The ISA flag an intrinsic needs on `arch`, and how to name it in errors.
fn required_flag(arch: Arch, i: IxIntrinsic) -> Option<(&'static str, &'static str)> {
    use IxIntrinsic::*;
    match (arch, i) {
        (_, Prefetch) => None,
        (Arch::X64, AesEnc | AesEncLast | AesDec | AesDecLast | AesImc) => {
            Some(("has_aes", "AES-NI"))
        }
        (Arch::X64, Clmul | ClmulLo | ClmulHi) => Some(("has_pclmulqdq", "PCLMULQDQ")),
        (Arch::X64, Crc32cU8 | Crc32cU32 | Crc32cU64) => Some(("has_sse42", "SSE4.2 CRC32")),
        (Arch::X64, Sha256H | Sha256H2 | Sha256Su0 | Sha256Su1) => {
            Some(("has_sha", "the SHA extensions"))
        }
        (Arch::X64, PextU32 | PextU64 | PdepU32 | PdepU64) => Some(("has_bmi2", "BMI2")),
        (
            Arch::Aarch64,
            AesEnc | AesEncLast | AesDec | AesDecLast | AesImc | Clmul | ClmulLo | ClmulHi,
        ) => Some(("has_aes", "FEAT_AES and FEAT_PMULL")),
        (Arch::Aarch64, Crc32cU8 | Crc32cU32 | Crc32cU64) => Some(("has_crc", "FEAT_CRC32")),
        (Arch::Aarch64, Sha256H | Sha256H2 | Sha256Su0 | Sha256Su1) => {
            Some(("has_sha2", "FEAT_SHA256"))
        }
        (Arch::Aarch64, PextU32 | PextU64 | PdepU32 | PdepU64) => {
            Some(("has_sve2_bitperm", "FEAT_SVE2 with FEAT_SVE_BitPerm"))
        }
    }
}

/// Lower a direct call to `intrinsic`, whose arguments are on top of the
/// operand stack.
pub(crate) fn translate(
    intrinsic: IxIntrinsic,
    builder: &mut FunctionBuilder,
    environ: &mut FuncEnvironment<'_>,
) -> WasmResult<()> {
    let isa = environ.isa();
    let arch = match isa.triple().architecture {
        Architecture::X86_64 => Arch::X64,
        Architecture::Aarch64(_) => Arch::Aarch64,
        other => {
            return Err(wasm_unsupported!(
                "`ix.{}` is not available on {other}",
                intrinsic.name()
            ));
        }
    };
    if let Some((flag, what)) = required_flag(arch, intrinsic) {
        let on = isa
            .isa_flags()
            .iter()
            .any(|v| v.name == flag && v.as_bool() == Some(true));
        if !on {
            return Err(wasm_unsupported!(
                "`ix.{}` needs {what} (Cranelift flag `{flag}`), which this target does not have",
                intrinsic.name()
            ));
        }
    }

    let n = intrinsic.params().len();
    let mut args = environ.stacks.peekn(n).to_vec();
    environ.stacks.popn(n);
    // A v128 may arrive as any vector type; the instructions take i8x16.
    for a in &mut args {
        if builder.func.dfg.value_type(*a).is_vector() {
            *a = bitcast(builder, I8X16, *a);
        }
    }

    use IxIntrinsic::*;
    let result = match (arch, intrinsic) {
        // AES, x86 semantics.
        (Arch::X64, AesEnc) => Some(builder.ins().x86_aesenc(args[0], args[1])),
        (Arch::X64, AesEncLast) => Some(builder.ins().x86_aesenclast(args[0], args[1])),
        (Arch::X64, AesDec) => Some(builder.ins().x86_aesdec(args[0], args[1])),
        (Arch::X64, AesDecLast) => Some(builder.ins().x86_aesdeclast(args[0], args[1])),
        (Arch::X64, AesImc) => Some(builder.ins().x86_aesimc(args[0])),
        // Arm: AESE/AESD xor the key first, x86 last. Use a zero key and xor
        // after; the backend folds that xor into the next round's AESE.
        (Arch::Aarch64, AesEnc | AesEncLast | AesDec | AesDecLast) => {
            let s = args[0];
            let k = args[1];
            let zero = zero_vector(builder);
            let enc = matches!(intrinsic, AesEnc | AesEncLast);
            let t = if enc {
                builder.ins().aarch64_aese(s, zero)
            } else {
                builder.ins().aarch64_aesd(s, zero)
            };
            let t = match intrinsic {
                AesEnc => builder.ins().aarch64_aesmc(t),
                AesDec => builder.ins().aarch64_aesimc(t),
                _ => t,
            };
            Some(builder.ins().bxor(t, k))
        }
        (Arch::Aarch64, AesImc) => Some(builder.ins().aarch64_aesimc(args[0])),

        // Carry-less multiply.
        (_, Clmul) => {
            let a = builder.ins().scalar_to_vector(I64X2, args[0]);
            let b = builder.ins().scalar_to_vector(I64X2, args[1]);
            let (a, b) = (a, b);
            Some(clmul(builder, arch, a, b, false))
        }
        (_, ClmulLo | ClmulHi) => {
            let (a, b) = (args[0], args[1]);
            Some(clmul(builder, arch, a, b, intrinsic == ClmulHi))
        }

        // CRC-32C: the same instruction semantics on both.
        (_, Crc32cU8) => {
            let byte = builder.ins().ireduce(I8, args[1]);
            Some(builder.ins().ix_crc32c(byte, args[0]))
        }
        (_, Crc32cU32 | Crc32cU64) => Some(builder.ins().ix_crc32c(args[1], args[0])),

        // SHA-256, Arm semantics.
        (Arch::Aarch64, Sha256H) => {
            let (a, b, c) = (args[0], args[1], args[2]);
            Some(builder.ins().aarch64_sha256h(a, b, c))
        }
        (Arch::Aarch64, Sha256H2) => {
            let (a, b, c) = (args[0], args[1], args[2]);
            Some(builder.ins().aarch64_sha256h2(a, b, c))
        }
        (Arch::Aarch64, Sha256Su0) => {
            let (a, b) = (args[0], args[1]);
            Some(builder.ins().aarch64_sha256su0(a, b))
        }
        (Arch::Aarch64, Sha256Su1) => {
            let (a, b, c) = (args[0], args[1], args[2]);
            Some(builder.ins().aarch64_sha256su1(a, b, c))
        }
        (Arch::X64, Sha256H | Sha256H2) => {
            // Four rounds as two `sha256rnds2`. x86 keeps the state as ABEF
            // and CDGH (lane 0 = F, H); Arm as ABCD and EFGH (lane 0 = A, E).
            let (abcd, efgh) = if intrinsic == Sha256H {
                (args[0], args[1])
            } else {
                (args[1], args[0])
            };
            let wk = args[2];
            let abef = shuffle32(builder, abcd, efgh, [5, 4, 1, 0]);
            let cdgh = shuffle32(builder, abcd, efgh, [7, 6, 3, 2]);
            let abef1 = builder.ins().x86_sha256rnds2(cdgh, abef, wk);
            let wk_hi = shuffle32(builder, wk, wk, [2, 3, 2, 3]);
            let abef2 = builder.ins().x86_sha256rnds2(abef, abef1, wk_hi);
            // After four rounds ABEF = abef2 and CDGH = abef1.
            Some(if intrinsic == Sha256H {
                shuffle32(builder, abef2, abef1, [3, 2, 7, 6])
            } else {
                shuffle32(builder, abef2, abef1, [1, 0, 5, 4])
            })
        }
        (Arch::X64, Sha256Su0) => {
            let (a, b) = (args[0], args[1]);
            Some(builder.ins().x86_sha256msg1(a, b))
        }
        (Arch::X64, Sha256Su1) => {
            // msg2(su0 + W[9..13], W[12..16]).
            let (su0, w8, w12) = (args[0], args[1], args[2]);
            let w9 = shuffle32(builder, w8, w12, [1, 2, 3, 4]);
            let su0 = bitcast(builder, I32X4, su0);
            let w9 = bitcast(builder, I32X4, w9);
            let t = builder.ins().iadd(su0, w9);
            let t = bitcast(builder, I8X16, t);
            Some(builder.ins().x86_sha256msg2(t, w12))
        }

        // Bit permutes.
        (_, PextU32 | PextU64) => Some(builder.ins().ix_pext(args[0], args[1])),
        (_, PdepU32 | PdepU64) => Some(builder.ins().ix_pdep(args[0], args[1])),

        (_, Prefetch) => {
            prefetch(builder, environ, args[0], args[1]);
            None
        }
    };
    if let Some(r) = result {
        environ.stacks.push1(r);
    }
    Ok(())
}

fn bitcast(builder: &mut FunctionBuilder, ty: ir::Type, x: Value) -> Value {
    if builder.func.dfg.value_type(x) == ty {
        return x;
    }
    let mut flags = MemFlagsData::new();
    flags.set_endianness(ir::Endianness::Little);
    builder.ins().bitcast(ty, flags, x)
}

fn zero_vector(builder: &mut FunctionBuilder) -> Value {
    let c = builder
        .func
        .dfg
        .constants
        .insert(ConstantData::from(&[0u8; 16][..]));
    builder.ins().vconst(I8X16, c)
}

/// A shuffle of the 32-bit lanes of `a` (0-3) and `b` (4-7).
fn shuffle32(builder: &mut FunctionBuilder, a: Value, b: Value, lanes: [u8; 4]) -> Value {
    let mut bytes = [0u8; 16];
    for (i, l) in lanes.iter().enumerate() {
        for j in 0..4 {
            bytes[i * 4 + j] = l * 4 + j as u8;
        }
    }
    let mask = builder
        .func
        .dfg
        .immediates
        .push(ConstantData::from(&bytes[..]));
    builder.ins().shuffle(a, b, mask)
}

fn clmul(builder: &mut FunctionBuilder, arch: Arch, a: Value, b: Value, hi: bool) -> Value {
    match (arch, hi) {
        (Arch::X64, false) => builder.ins().x86_pclmulqdq(a, b, 0x00),
        (Arch::X64, true) => builder.ins().x86_pclmulqdq(a, b, 0x11),
        (Arch::Aarch64, false) => builder.ins().aarch64_pmull(a, b),
        (Arch::Aarch64, true) => builder.ins().aarch64_pmull2(a, b),
    }
}

/// `prefetch(addr, hint)` on memory 0. A non-constant hint, or a module
/// without a memory, emits nothing. An address at or past the memory's
/// current length prefetches the memory's base instead, so the hint never
/// touches anything outside this instance's memory and never traps.
fn prefetch(
    builder: &mut FunctionBuilder,
    environ: &mut FuncEnvironment<'_>,
    addr: Value,
    hint: Value,
) {
    let Some(hint) = iconst_value(builder, hint) else {
        return;
    };
    if environ.module.memories.is_empty() {
        return;
    }
    let heap = environ.get_or_create_heap(builder.func, MemoryIndex::from_u32(0));
    let pcc = environ.proof_carrying_code();
    let heap = &environ.heaps()[heap];
    // Under PCC the prefetch's address is a checked access (see
    // `mem_flags_pcc_checked_data`), so it needs facts. They are given for
    // a 32-bit index into a static memory whose reservation plus guard
    // covers every 32-bit offset; anywhere else the hint is dropped.
    let pcc_mt = match heap.pcc_memory_type {
        Some(mt) if pcc => match builder.func.memory_types[mt] {
            ir::MemoryTypeData::Memory { size } if size > u64::from(u32::MAX) => Some(mt),
            _ => None,
        },
        _ => None,
    };
    if pcc && (pcc_mt.is_none() || builder.func.dfg.value_type(addr) != ir::types::I32) {
        return;
    }
    let vmctx = builder
        .func
        .special_param(ir::ArgumentPurpose::VMContext)
        .expect("missing vmctx parameter");
    let base = heap.base.emit(&mut builder.cursor(), vmctx);
    let bound = match heap.memory.static_heap_size() {
        Some(size) => builder.ins().iconst(I64, size as i64),
        None => heap.bound.emit(&mut builder.cursor(), vmctx),
    };
    let addr = if builder.func.dfg.value_type(addr) == I64 {
        addr
    } else {
        builder.ins().uextend(I64, addr)
    };
    let inside = builder.ins().icmp(IntCC::UnsignedLessThan, addr, bound);
    let in_mem = builder.ins().iadd(base, addr);
    let p = builder.ins().select(inside, in_mem, base);
    if let Some(mt) = pcc_mt {
        use cranelift_codegen::ir::pcc::Fact;
        let max = u64::from(u32::MAX);
        builder.func.dfg.facts[addr] = Some(Fact::max_range_for_width_extended(32, 64));
        for v in [in_mem, p] {
            builder.func.dfg.facts[v] = Some(Fact::Mem {
                ty: mt,
                min_offset: 0,
                max_offset: max,
                nullable: false,
            });
        }
    }
    builder.ins().ix_prefetch(p, (hint & 7) as u8);
}

fn iconst_value(builder: &FunctionBuilder, v: Value) -> Option<i64> {
    let inst = builder.func.dfg.value_def(v).inst()?;
    match builder.func.dfg.insts[inst] {
        ir::InstructionData::UnaryImm {
            opcode: ir::Opcode::Iconst,
            imm,
        } => Some(imm.bits()),
        _ => None,
    }
}
