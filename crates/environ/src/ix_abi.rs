//! ix fork: the artifact ABI fingerprint.
//!
//! A `.cwasm` bakes in the vmctx layout, the shape of every serialized
//! struct, and the code each ix feature emits. Upstream Wasmtime ties
//! artifact compatibility to its release version; the fork used to append a
//! hand-bumped tag (`+ix1`, `+ix3`, ...), which fork branches bumped
//! independently, so two branches could ship the same tag with different
//! layouts, and a merge could keep a tag that matches neither parent.
//!
//! Instead the tag is a hash, computed at compile time, of:
//!
//! * the vmctx layouts (`for_each_vmctx_type!`, the single source of
//!   `VMOffsets`) and the fixed VM struct layouts (`for_each_vm_type!`),
//!   token for token;
//! * the field list of `Tunables` (serialized in every artifact's metadata);
//! * the field list of every serialized struct the fork defines or extends
//!   (`Module` and every ix struct in it), via [`ix_abi_struct!`];
//! * [`IX_FEATURES`]: one `(name, version)` line per ix feature that changes
//!   generated code or serialized data without changing any of the above;
//! * the contents of every source that can change generated code: all
//!   `.rs` and `.isle` files under Cranelift's codegen (and its meta
//!   crate), the x64 assembler, Winch's codegen, `wasmtime-cranelift` and
//!   this crate, plus
//!   the runtime halves of fork features (`IX_SOURCE_TREES` and
//!   `IX_SOURCE_FILES` in `build.rs`), and the full package version, so a
//!   fix to what the generated code or a proof means (which tables are
//!   static, what a poll does) is a new ABI even when no layout or feature
//!   number changes.
//!
//! So a layout change can't be forgotten (the hash follows the source), and
//! merging two branches that each add a feature line or a field gives a
//! hash that differs from both parents. Whitespace is ignored, so a
//! formatting change in the stringified tokens alone does not matter;
//! anything else (including doc comments inside the VM type macro) changes
//! the hash, which only ever costs a recompile.
//!
//! The semantic ABI number stays: `IX_INTRINSICS_ABI` is bumped by hand
//! when the meaning of something changes with no layout change (as `ix2` to
//! `ix3` did: same vmctx, a poll word instead of a poll page). The hash
//! catches every layout change the bump forgets, and the feature lines keep
//! two branches that pick the same number apart.
//!
//! `wasmtime::ModuleVersionStrategy::WasmtimeVersion` is
//! `<major>+ix<IX_INTRINSICS_ABI>.<IX_ABI_TAG>`, and the artifact metadata
//! carries both `IX_INTRINSICS_ABI` and [`IX_ABI_HASH`], checked in
//! `Metadata::check_ix_abi` under every version strategy (`Custom` and
//! `None` skip the version string). `N` is written once, in
//! `ix_intrinsics_abi!`.

use crate::{IxDispatchPlan, IxDispatchRow, Module, Tunables};

/// ix fork: define a serialized struct and record its field list (names and
/// types, not attributes or docs) as `Self::IX_SCHEMA` for [`IX_ABI_HASH`].
#[macro_export]
#[doc(hidden)]
macro_rules! ix_abi_struct {
    (
        $(#[$m:meta])*
        $vis:vis struct $name:ident {
            $(
                $(#[$fm:meta])*
                $fvis:vis $field:ident : $ty:ty
            ),* $(,)?
        }
    ) => {
        $(#[$m])*
        $vis struct $name {
            $(
                $(#[$fm])*
                $fvis $field: $ty,
            )*
        }

        impl $name {
            /// ix fork: this struct's field list, for the artifact ABI hash.
            #[doc(hidden)]
            pub const IX_SCHEMA: &'static str =
                concat!(stringify!($name), "{", $(stringify!($field: $ty), ",",)* "}");
        }
    };
}

/// Every ix fork feature whose generated code or serialized data an artifact
/// depends on, with a version number.
///
/// Rules for fork branches:
///
/// * a new feature adds one line here (keep the list sorted by name; a merge
///   keeps every line from both sides);
/// * a feature's codegen and proofs must live under one of the hashed
///   trees, or its files are added to `IX_SOURCE_FILES`
///   (`crates/environ/build.rs`), so editing them changes the hash with no
///   manual step;
/// * a change to a feature's generated code, or to the meaning of its
///   serialized data, bumps that feature's number;
/// * a new or changed serialized field is picked up automatically when the
///   struct is defined with [`ix_abi_struct!`] and its schema is listed in
///   `SCHEMAS` below; a new vmctx field is picked up from
///   `for_each_vmctx_type!` with no edit here.
pub const IX_FEATURES: &[(&str, u32)] = &[
    // `ix_leaf_stack_slack`, `ix_table_devirt`; their codegen and the
    // static-table proof are in the hashed sources.
    ("check-trim", 1),
    // The `ix` intrinsics module; `IX_INTRINSICS_ABI`.
    ("intrinsics", crate::IX_INTRINSICS_ABI),
    // `poll_interruption`: the vmctx `poll_ptr` word, tested at every
    // function entry and loop header.
    ("poll-interruption", 1),
    // `ix_static_call_indirect`: `IxDispatchPlan` in `Module`, the
    // `ix_dispatch` vmctx field. 2: rows truncated after their last match,
    // a zero entry falls back to the checked call, rows ranked by sites
    // per entry under an 8-per-element cap.
    ("static-call-indirect", 2),
    // `ix_wasm_cc`: Cranelift `tail_arg_regs`, the integer argument
    // registers of every wasm-to-wasm call and trampoline.
    ("wasm-cc", 1),
];

/// Serialized struct schemas covered by the hash.
const SCHEMAS: &[&str] = &[
    Module::IX_SCHEMA,
    IxDispatchPlan::IX_SCHEMA,
    IxDispatchRow::IX_SCHEMA,
];

macro_rules! ix_stringify_all {
    ($($t:tt)*) => {
        stringify!($($t)*)
    };
}

/// The vmctx layouts, as written in `for_each_vmctx_type!`.
const VMCTX_LAYOUT: &str = crate::for_each_vmctx_type!(ix_stringify_all);
/// The fixed VM struct layouts, as written in `for_each_vm_type!`.
const VM_TYPES_LAYOUT: &str = crate::for_each_vm_type!(ix_stringify_all);

const fn fnv1a(mut h: u64, s: &str) -> u64 {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        i += 1;
        if c.is_ascii_whitespace() {
            continue;
        }
        h ^= c as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    // Separator so that ("ab", "c") and ("a", "bc") differ.
    h ^= 0xff;
    h.wrapping_mul(0x0000_0100_0000_01b3)
}

const fn compute_hash() -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325;
    h = fnv1a(h, VMCTX_LAYOUT);
    h = fnv1a(h, VM_TYPES_LAYOUT);
    h = fnv1a(h, Tunables::IX_FIELDS);
    h = fnv1a(h, env!("IX_SOURCES_HASH"));
    let mut i = 0;
    while i < SCHEMAS.len() {
        h = fnv1a(h, SCHEMAS[i]);
        i += 1;
    }
    let mut i = 0;
    while i < IX_FEATURES.len() {
        let (name, version) = IX_FEATURES[i];
        h = fnv1a(h, name);
        let mut v = [0u8; 10];
        let mut n = version;
        let mut j = 0;
        while j < 10 {
            v[j] = b'0' + (n % 10) as u8;
            n /= 10;
            j += 1;
        }
        h = fnv1a(h, match core::str::from_utf8(&v) {
            Ok(s) => s,
            Err(_) => panic!(),
        });
        i += 1;
    }
    h
}

/// ix fork: the artifact ABI fingerprint (see the module docs).
pub const IX_ABI_HASH: u64 = compute_hash();

const fn hex16(h: u64) -> [u8; 16] {
    let digits = b"0123456789abcdef";
    let mut out = [0u8; 16];
    let mut i = 0;
    while i < 16 {
        out[i] = digits[((h >> (60 - 4 * i)) & 0xf) as usize];
        i += 1;
    }
    out
}

const TAG_BYTES: [u8; 16] = hex16(IX_ABI_HASH);

/// ix fork: [`IX_ABI_HASH`] as 16 lowercase hex digits.
pub const IX_ABI_TAG: &str = match core::str::from_utf8(&TAG_BYTES) {
    Ok(s) => s,
    Err(_) => panic!(),
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn features_sorted_and_unique() {
        for w in IX_FEATURES.windows(2) {
            assert!(w[0].0 < w[1].0, "IX_FEATURES must be sorted by name: {w:?}");
        }
    }

    #[test]
    fn layout_text_is_captured() {
        assert!(VMCTX_LAYOUT.contains("type_ids"));
        assert!(VMCTX_LAYOUT.contains("poll_ptr"));
        assert!(VMCTX_LAYOUT.contains("ix_dispatch"));
        assert!(Module::IX_SCHEMA.contains("ix_dispatch"));
        assert!(IxDispatchRow::IX_SCHEMA.contains("len"));
        assert!(Tunables::IX_FIELDS.contains("ix_static_call_indirect"));
        assert!(Module::IX_SCHEMA.contains("ix_intrinsics"));
        assert!(Tunables::IX_FIELDS.contains("table_lazy_init"));
        assert!(Tunables::IX_FIELDS.contains("ix_wasm_cc"));
        assert_eq!(IX_ABI_TAG.len(), 16);
    }

    #[test]
    fn source_hash_covers_the_trees() {
        // The build script walks whole trees; a broken walk (a renamed root,
        // an extension filter that matches nothing) would hash a handful of
        // files and still build. There are several hundred.
        let n: usize = env!("IX_SOURCES_COUNT").parse().unwrap();
        assert!(n > 300, "IX_SOURCES_HASH covers only {n} files");
    }
}
