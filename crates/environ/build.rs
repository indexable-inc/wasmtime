//! ix fork: hash the fork's own compiler and runtime sources into
//! `IX_SOURCES_HASH`, part of `IX_ABI_HASH` (see `src/ix_abi.rs`).
//!
//! Layout and field lists catch most ABI changes, but not a fix to what the
//! fork's code generation or its proofs (which tables are static, what a
//! poll does) mean. Hashing the files those live in makes any edit to them
//! a new artifact ABI, which only ever costs a recompile, so a soundness
//! fix can never leave stale artifacts loadable.

use std::path::Path;

/// Files, relative to this crate, whose contents an artifact depends on
/// beyond the layouts. A fork branch adds the files its feature lives in
/// (keep sorted; a merge keeps every line).
const IX_SOURCES: &[&str] = &[
    "../cranelift/src/alias_region.rs",
    "../cranelift/src/func_environ.rs",
    "../wasmtime/src/runtime/poll_interrupt.rs",
    "src/ix_intrinsics.rs",
];

fn main() {
    let dir = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for rel in IX_SOURCES {
        let path = Path::new(&dir).join(rel);
        println!("cargo:rerun-if-changed={}", path.display());
        let bytes = std::fs::read(&path).unwrap_or_else(|e| {
            panic!(
                "ix fork: cannot read {} for IX_SOURCES_HASH ({e}); \
                 update IX_SOURCES in crates/environ/build.rs",
                path.display()
            )
        });
        for b in rel.bytes().chain([0xff]).chain(bytes).chain([0xff]) {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rustc-env=IX_SOURCES_HASH={h:016x}");
}
