//! ix fork: hash the compiler and runtime sources into `IX_SOURCES_HASH`,
//! part of `IX_ABI_HASH` (see `src/ix_abi.rs`).
//!
//! Layout and field lists catch most ABI changes, but not a fix to what the
//! generated code or the fork's proofs (which tables are static, what a poll
//! does) mean. Hashing every source that can change generated code makes any
//! edit to it a new artifact ABI, which only ever costs a recompile, so a
//! soundness fix can never leave stale artifacts loadable.
//!
//! A hand-kept file list missed files that change code (the ISLE lowerings,
//! alias analysis, the translator), so whole trees are hashed instead: every
//! `.rs` and `.isle` file under [`IX_SOURCE_TREES`], plus [`IX_SOURCE_FILES`]
//! outside them. Paths are sorted and hashed together with the contents, so
//! the hash is the same on every machine and a rename changes it.
//!
//! The trees live outside this crate, so a packaged (`cargo package`,
//! vendored) copy of `wasmtime-environ` alone cannot build: a missing or
//! empty tree panics rather than silently hashing less.

use std::path::{Path, PathBuf};

/// Directories, relative to this crate, whose `.rs` and `.isle` files are
/// hashed recursively: Cranelift's code generator (with its ISLE rules and
/// the meta crates that generate instruction definitions), the x64
/// assembler, Winch's code generator, the Wasm-to-CLIF translator, and this
/// crate.
const IX_SOURCE_TREES: &[&str] = &[
    "../../cranelift/assembler-x64",
    "../../cranelift/codegen/meta/src",
    "../../cranelift/codegen/src",
    "../../winch/codegen/src",
    "../cranelift/src",
    "src",
];

/// Single files outside the trees whose contents an artifact depends on
/// (the runtime halves of fork features; the codegen build script, which
/// drives ISLE generation). A fork branch adds the files its feature lives
/// in (keep sorted; a merge keeps every line).
const IX_SOURCE_FILES: &[&str] = &[
    "../../cranelift/codegen/build.rs",
    "../wasmtime/src/runtime/module.rs",
    "../wasmtime/src/runtime/poll_interrupt.rs",
    "build.rs",
];

fn hashed_ext(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("rs" | "isle")
    )
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = std::fs::read_dir(dir).unwrap_or_else(|e| {
        panic!(
            "ix fork: cannot read {} for IX_SOURCES_HASH ({e}); this crate must be \
             built inside the wasmtime fork's workspace (see crates/environ/build.rs)",
            dir.display()
        )
    });
    for entry in entries {
        let entry = entry.unwrap_or_else(|e| panic!("ix fork: reading {}: {e}", dir.display()));
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        // Build output and editor/VCS droppings are not sources.
        if name.starts_with('.') || name == "target" {
            continue;
        }
        let ty = entry
            .file_type()
            .unwrap_or_else(|e| panic!("ix fork: stat {}: {e}", path.display()));
        if ty.is_dir() {
            walk(&path, out);
        } else if hashed_ext(&path) {
            out.push(path);
        }
    }
}

/// `path` relative to `base`, with `/` separators, so the hashed name does
/// not depend on the checkout location or the host OS.
fn rel_name(base: &Path, path: &Path) -> String {
    let rel = path.strip_prefix(base).unwrap_or(path);
    rel.components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

fn main() {
    let dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));

    let mut files: Vec<(String, PathBuf)> = Vec::new();
    for rel in IX_SOURCE_TREES {
        let root = dir.join(rel);
        // Cargo rescans a directory given to rerun-if-changed recursively.
        println!("cargo:rerun-if-changed={}", root.display());
        let mut found = Vec::new();
        walk(&root, &mut found);
        assert!(
            !found.is_empty(),
            "ix fork: no .rs or .isle files under {} for IX_SOURCES_HASH",
            root.display()
        );
        for path in found {
            let name = format!("{rel}/{}", rel_name(&root, &path));
            files.push((name, path));
        }
    }
    for rel in IX_SOURCE_FILES {
        let path = dir.join(rel);
        println!("cargo:rerun-if-changed={}", path.display());
        files.push(((*rel).to_string(), path));
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));
    files.dedup_by(|a, b| a.0 == b.0);

    // FNV-1a 64 over (name, 0xff, contents, 0xff) for each file in order,
    // then the full package version (the workspace version every crate
    // here shares), so a version bump alone is a new ABI too.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut feed = |bytes: &[u8]| {
        for &b in bytes {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    };
    for (name, path) in &files {
        let bytes = std::fs::read(path).unwrap_or_else(|e| {
            panic!(
                "ix fork: cannot read {} for IX_SOURCES_HASH ({e}); \
                 update IX_SOURCE_FILES in crates/environ/build.rs",
                path.display()
            )
        });
        feed(name.as_bytes());
        feed(&[0xff]);
        feed(&bytes);
        feed(&[0xff]);
    }
    let version = std::env::var("CARGO_PKG_VERSION").expect("CARGO_PKG_VERSION");
    feed(version.as_bytes());

    println!("cargo:rustc-env=IX_SOURCES_HASH={h:016x}");
    println!("cargo:rustc-env=IX_SOURCES_COUNT={}", files.len());
}
