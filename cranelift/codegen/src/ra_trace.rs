//! Bench-only register-allocator move tracing.
//!
//! When the environment variable `CRANELIFT_RA_TRACE_DIR` is set (read once
//! per process), regalloc2 runs with `trace_reasons` and every regalloc edit
//! emitted by `VCode::emit` is recorded with its machine-code byte range and
//! its reason (`regalloc2::edit_reason`). One file per function is written to
//! `<dir>/<namespace>_<index>.tsv` (or `<dir>/testcase_<name>.tsv`), with the
//! lines `start\tend\treason\tkind\tfrom\tto`; offsets are relative to the
//! start of the function's code buffer, which includes the prologue. A
//! `legend.tsv` describing the reason encoding is written once into the
//! directory. When the variable is unset nothing changes.

/// One emitted regalloc edit.
#[derive(Clone, Debug)]
pub struct Record {
    /// Offset of the first byte of the emitted move.
    pub start: u32,
    /// Offset one past its last byte.
    pub end: u32,
    /// `regalloc2::edit_reason` encoding.
    pub reason: u16,
    /// "move", "spill" or "reload".
    pub kind: &'static str,
    /// Source allocation (display form).
    pub from: alloc::string::String,
    /// Destination allocation (display form).
    pub to: alloc::string::String,
}

#[cfg(feature = "std")]
mod imp {
    use crate::ir::UserFuncName;
    use alloc::string::String;
    use alloc::vec::Vec;
    use core::cell::RefCell;
    use core::fmt::Write as _;
    use std::path::{Path, PathBuf};
    use std::sync::OnceLock;

    use super::Record;

    std::thread_local! {
        static PENDING: RefCell<Option<Vec<Record>>> = const { RefCell::new(None) };
    }

    /// The trace directory, if tracing is enabled for this process.
    pub fn trace_dir() -> Option<&'static Path> {
        static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
        DIR.get_or_init(|| {
            let dir = PathBuf::from(std::env::var_os("CRANELIFT_RA_TRACE_DIR")?);
            if let Err(e) = std::fs::create_dir_all(&dir) {
                panic!("CRANELIFT_RA_TRACE_DIR: cannot create {}: {e}", dir.display());
            }
            write_legend(&dir);
            Some(dir)
        })
        .as_deref()
    }

    /// Is tracing enabled?
    #[inline]
    pub fn enabled() -> bool {
        trace_dir().is_some()
    }

    pub(crate) fn set_pending(records: Vec<Record>) {
        PENDING.with(|p| *p.borrow_mut() = Some(records));
    }

    /// Discard any records left by a compilation that did not finish.
    pub(crate) fn clear_pending() {
        PENDING.with(|p| *p.borrow_mut() = None);
    }

    /// File stem used for a function name.
    pub fn file_stem(name: &UserFuncName) -> String {
        match name {
            UserFuncName::User(u) => alloc::format!("{}_{}", u.namespace, u.index),
            UserFuncName::Testcase(t) => {
                let s: String = alloc::format!("{t}")
                    .chars()
                    .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
                    .collect();
                alloc::format!("testcase_{s}")
            }
        }
    }

    /// Write the records of the function just emitted on this thread.
    pub(crate) fn flush(name: &UserFuncName) {
        let Some(dir) = trace_dir() else { return };
        let Some(records) = PENDING.with(|p| p.borrow_mut().take()) else {
            return;
        };
        let mut out = String::with_capacity(records.len() * 32);
        for r in &records {
            writeln!(
                out,
                "{}\t{}\t{}\t{}\t{}\t{}",
                r.start, r.end, r.reason, r.kind, r.from, r.to
            )
            .unwrap();
        }
        let path = dir.join(alloc::format!("{}.tsv", file_stem(name)));
        // Write to a temp file and rename so readers never see a torn file.
        let tmp = dir.join(alloc::format!(
            ".{}.tsv.{:?}",
            file_stem(name),
            std::thread::current().id()
        ));
        if let Err(e) = std::fs::write(&tmp, out).and_then(|()| std::fs::rename(&tmp, &path)) {
            panic!("CRANELIFT_RA_TRACE_DIR: cannot write {}: {e}", path.display());
        }
    }

    fn write_legend(dir: &Path) {
        use regalloc2::edit_reason as er;
        let mut s = String::new();
        s.push_str("# reason = DIR | KIND<<2 | CAUSE<<5 | SPILL<<9 | TRIGGER<<10 | SRC<<14\n");
        s.push_str("field\tvalue\tname\n");
        for (v, n) in [
            (er::DIR_REG_REG, "reg_reg"),
            (er::DIR_REG_STACK, "spill_store"),
            (er::DIR_STACK_REG, "reload"),
            (er::DIR_STACK_STACK, "stack_stack"),
        ] {
            writeln!(s, "dir\t{v}\t{n}").unwrap();
        }
        for (v, n) in [
            (er::KIND_INTRA_BLOCK, "intra_block"),
            (er::KIND_EDGE, "block_edge"),
            (er::KIND_BLOCKPARAM, "blockparam"),
            (er::KIND_REUSE_FIXUP, "reuse_fixup"),
            (er::KIND_MULTI_FIXED_FIXUP, "multi_fixed_fixup"),
            (er::KIND_SCRATCH_SAVE, "scratch_save"),
            (er::KIND_UNKNOWN, "unknown"),
        ] {
            writeln!(s, "kind\t{v}\t{n}").unwrap();
        }
        for (v, n) in [
            (er::CAUSE_ORIGINAL, "original"),
            (er::CAUSE_CONFLICT_BUNDLE, "conflict_bundle"),
            (er::CAUSE_CONFLICT_FIXED_CALL, "conflict_fixed_bundle_call"),
            (er::CAUSE_CONFLICT_FIXED_OTHER, "conflict_fixed_bundle_other"),
            (er::CAUSE_CALL_CLOBBER, "call_clobber"),
            (er::CAUSE_FIXED_RESV_AT_CALL, "fixed_resv_at_call"),
            (er::CAUSE_FIXED_RESV_OTHER, "fixed_resv_other"),
            (er::CAUSE_REQUIREMENT, "requirement_conflict"),
            (er::CAUSE_MIN_MAX_SPLITS, "minimal_max_splits"),
            (er::CAUSE_MIN_ONE_INST, "minimal_one_inst"),
        ] {
            writeln!(s, "cause\t{v}\t{n}").unwrap();
        }
        for (v, n) in [
            (er::SRC_DST_START, "dst_lr_start"),
            (er::SRC_SRC_END, "src_lr_end"),
            (er::SRC_DST_BUNDLE, "dst_bundle"),
            (er::SRC_SRC_BUNDLE, "src_bundle"),
        ] {
            writeln!(s, "src\t{v}\t{n}").unwrap();
        }
        // Ignore races between processes writing the same legend.
        let _ = std::fs::write(dir.join("legend.tsv"), s);
    }
}

#[cfg(feature = "std")]
pub use imp::{enabled, file_stem, trace_dir};
#[cfg(feature = "std")]
pub(crate) use imp::{clear_pending, flush, set_pending};

#[cfg(not(feature = "std"))]
mod imp_nostd {
    /// Tracing needs `std`.
    #[inline]
    pub fn enabled() -> bool {
        false
    }
    pub(crate) fn set_pending(_: alloc::vec::Vec<super::Record>) {}
    pub(crate) fn clear_pending() {}
    pub(crate) fn flush(_: &crate::ir::UserFuncName) {}
}
#[cfg(not(feature = "std"))]
pub use imp_nostd::enabled;
#[cfg(not(feature = "std"))]
pub(crate) use imp_nostd::{clear_pending, flush, set_pending};
