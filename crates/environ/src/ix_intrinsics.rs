//! `ix` intrinsic imports (indexable fork).
//!
//! A core module may import functions from the reserved module `"ix"` whose
//! names appear in [`IxIntrinsic`]. Those imports are not linked: they hold a
//! function index (so the module's index space is unchanged) but get no
//! `Initializer::Import`, so `Module::imports()` and the `Linker` never see
//! them. Cranelift lowers each direct `call` to one inline (see
//! `wasmtime-cranelift`'s `ix_intrinsics.rs`). Taking one's reference
//! (export, element segment, `ref.func`) is a compile error.
//!
//! Other names in `"ix"` (for example the host's `ix.log`) stay ordinary
//! imports.

use crate::{WasmFuncType, WasmValType};
use serde_derive::{Deserialize, Serialize};

/// The reserved import module.
pub const IX_INTRINSIC_MODULE: &str = "ix";

/// Version of the fork's ABI: the intrinsic set and its semantics, and the
/// fork's changes to the vmctx layout. Part of the engine's compatibility
/// hash, and the `+ixN` suffix of the `.cwasm` version string
/// (`ModuleVersionStrategy::as_str`, keep the two in step), so artifacts
/// compiled against another version are rejected.
///
/// - 1: `ix` intrinsics.
/// - 2: vmctx `poll_ptr` after `epoch_ptr` (poll-page interruption).
pub const IX_INTRINSICS_ABI: u32 = 2;

macro_rules! intrinsics {
    ($($(#[$doc:meta])* $variant:ident $name:literal ($($p:ident),*) -> ($($r:ident),*);)*) => {
        /// One `ix` intrinsic import.
        #[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[expect(missing_docs, reason = "documented in the table below")]
        pub enum IxIntrinsic {
            $($variant,)*
        }

        impl IxIntrinsic {
            /// Every intrinsic, in table order.
            pub const ALL: &[IxIntrinsic] = &[$(IxIntrinsic::$variant,)*];

            /// Looks an intrinsic up by its import field name.
            pub fn from_name(name: &str) -> Option<IxIntrinsic> {
                match name {
                    $($name => Some(IxIntrinsic::$variant),)*
                    _ => None,
                }
            }

            /// The import field name.
            pub fn name(self) -> &'static str {
                match self {
                    $(IxIntrinsic::$variant => $name,)*
                }
            }

            /// The required Wasm parameter types.
            pub fn params(self) -> &'static [WasmValType] {
                match self {
                    $(IxIntrinsic::$variant => &[$(WasmValType::$p),*],)*
                }
            }

            /// The required Wasm result types.
            pub fn results(self) -> &'static [WasmValType] {
                match self {
                    $(IxIntrinsic::$variant => &[$(WasmValType::$r),*],)*
                }
            }
        }
    };
}

intrinsics! {
    // AES rounds on a 128-bit state, with x86 AES-NI semantics:
    // aes_enc(s, k) = MixColumns(ShiftRows(SubBytes(s))) ^ k;
    // aes_enc_last drops MixColumns; aes_dec / aes_dec_last are the
    // equivalent inverse cipher (InvShiftRows, InvSubBytes, InvMixColumns).
    AesEnc "aes_enc" (V128, V128) -> (V128);
    AesEncLast "aes_enc_last" (V128, V128) -> (V128);
    AesDec "aes_dec" (V128, V128) -> (V128);
    AesDecLast "aes_dec_last" (V128, V128) -> (V128);
    // InvMixColumns, to turn encryption round keys into decryption ones.
    AesImc "aes_imc" (V128) -> (V128);
    // Carry-less 64x64 -> 128 multiply; the low half is lane 0.
    Clmul "clmul" (I64, I64) -> (V128);
    // The same on lane 0 (lo) or lane 1 (hi) of two vectors.
    ClmulLo "clmul_lo" (V128, V128) -> (V128);
    ClmulHi "clmul_hi" (V128, V128) -> (V128);
    // One CRC-32C step (no pre- or post-inversion): crc32c_*(crc, data).
    Crc32cU8 "crc32c_u8" (I32, I32) -> (I32);
    Crc32cU32 "crc32c_u32" (I32, I32) -> (I32);
    Crc32cU64 "crc32c_u64" (I32, I64) -> (I32);
    // SHA-256 with Arm semantics: sha256_h(abcd, efgh, wk) -> abcd',
    // sha256_h2(efgh, abcd, wk) -> efgh' (four rounds), sha256_su0(w0, w4),
    // sha256_su1(su0, w8, w12) (message schedule).
    Sha256H "sha256_h" (V128, V128, V128) -> (V128);
    Sha256H2 "sha256_h2" (V128, V128, V128) -> (V128);
    Sha256Su0 "sha256_su0" (V128, V128) -> (V128);
    Sha256Su1 "sha256_su1" (V128, V128, V128) -> (V128);
    // Parallel bit extract / deposit: pext(x, mask), pdep(x, mask).
    PextU32 "pext_u32" (I32, I32) -> (I32);
    PextU64 "pext_u64" (I64, I64) -> (I64);
    PdepU32 "pdep_u32" (I32, I32) -> (I32);
    PdepU64 "pdep_u64" (I64, I64) -> (I64);
    // prefetch(addr, hint): hint bit 0 = write, bits 1-2 = locality 0..3.
    // A hint for an address outside memory 0 does nothing; never traps.
    Prefetch "prefetch" (I32, I32) -> ();
}

impl IxIntrinsic {
    /// Whether `ty` is exactly this intrinsic's signature.
    pub fn matches(self, ty: &WasmFuncType) -> bool {
        ty.params() == self.params() && ty.results() == self.results()
    }

    /// The signature as WAT, for error messages.
    pub fn signature(self) -> alloc::string::String {
        use core::fmt::Write;
        let mut s = alloc::string::String::from("(func");
        if !self.params().is_empty() {
            s.push_str(" (param");
            for p in self.params() {
                let _ = write!(s, " {p}");
            }
            s.push(')');
        }
        if !self.results().is_empty() {
            s.push_str(" (result");
            for r in self.results() {
                let _ = write!(s, " {r}");
            }
            s.push(')');
        }
        s.push(')');
        s
    }
}
