//! `Scratch` — the OWNED scratch-directory guard every vike-core journal test allocates its
//! working directory through, so a finished test leaves nothing on disk.
//!
//! # Why this exists
//!
//! Every journal/replay/restore suite in this crate used to mint its own directory with the same
//! few lines — `env::temp_dir().join(format!("vjl-{tag}-{pid}"))`, a `remove_dir_all` to survive
//! PID reuse, then a bare `PathBuf` handed back to the test. **Nothing removed it afterwards.**
//! That leaks without bound, because the PID changes every run: one directory per tag per
//! test-binary invocation, forever.
//!
//! Measured on the the CI box CI box on 2026-08-23, before this module existed: **27,471 leaked
//! `/tmp/vjl-*` directories totalling 211 GB** — 96% of everything under `/tmp` and ~70% of the
//! used space on that filesystem, growing by ~2,450 directories a day. The SIZE comes from the
//! journal's own design rather than from many files: `crates/vike-core/src/journal/segment.rs`'s
//! `reserve_blocks` calls `posix_fallocate`, and `crates/vike-core/src/runtime/mod.rs`'s
//! `JournalConfig::at` defaults to 64 MiB segments — so a segment is FULLY allocated the moment it
//! is created rather than sparse. A leaked test directory is a real 64 MiB, not a stub.
//!
//! # Why it wraps `tempfile` rather than hand-rolling the cleanup
//!
//! `tempfile = "3"` is this workspace's standard test scratch guard — a dev-dependency of 21
//! crates (deliberately per-crate rather than a workspace dep; see
//! `crates/bridges/polymarket/Cargo.toml`'s note), including `crates/vike-tradehub/Cargo.toml`
//! for exactly this purpose, "a throwaway journal directory". `crates/vike-ops/tests/
//! temp_path_gate.rs` — the gate that already polices temp paths in test code — names it "the
//! better of the two" spellings in its own module doc, because it also cleans up on the panic
//! path. Hand-rolling a thirteenth bespoke `Scratch` next to it would be the second name that
//! rots. What this newtype adds over a bare `TempDir` is the two shapes the journal suites
//! actually need, below.
//!
//! # The contract
//!
//! - **`Drop` removes the directory and everything under it.** Unwinding runs destructors, so a
//!   test that FAILS an assertion cleans up exactly like one that passes. Only an abort
//!   (`panic = "abort"`, SIGKILL, OOM) still leaks — the residual no RAII shape can close.
//! - **Hold the guard for the whole test.** Binding it to `_` drops it immediately and deletes the
//!   directory out from under the code under test; `let _guard = ...` or a plain `let dir = ...`
//!   is what you want.
//! - `Deref<Target = Path>` so the guard is used wherever the old `PathBuf` was: `dir.join(..)`,
//!   `dir.exists()`, `&dir` as `&Path`, and `dir.to_path_buf()` for the owned copy a
//!   `JournalConfig` wants. It is deliberately NOT `Clone` — two owners would each try to delete.
//! - The name carries the caller's tag, so a directory seen mid-run is attributable to a test.
//!   Uniqueness is `tempfile`'s random suffix, which is why nothing here has to clear a stale
//!   directory the way the hand-rolled helpers did: a path from an earlier run cannot be picked
//!   again, and concurrent CI lanes sharing one `/tmp` cannot collide.
//!
//! # Two constructors, because the journal creates its own directory
//!
//! `crates/vike-core/src/journal/writer.rs`'s `open` and the reader in
//! `crates/vike-core/src/journal/read.rs` both call `create_dir_all` themselves, so most suites
//! want a path that does NOT yet exist — [`Scratch::reserved`], which owns a temp root and points
//! one level INTO it. One suite (`replay_cli.rs`'s `missing` case) depends on the directory never
//! existing at all, and `MaterializeCheckpoint::load` has an absent-directory arm. The lock and
//! frame suites want it already there before the code under test runs — [`Scratch::created`].
//! Keeping both explicit means no suite has to reason about which behaviour it inherited.
//!
//! # Where this file is compiled
//!
//! Twice, deliberately, and never into a production build. `crates/vike-core/src/lib.rs` declares
//! it `#[cfg(test)]` for this crate's own `mod tests` blocks; the grouped `tests/` binaries pull
//! the SAME file in with `#[path = "../src/scratch.rs"]`, the idiom already used for
//! `crates/vike-core/tests/common/latency_line.rs`. One source file, no duplicated guard, and no
//! `test-support` feature to thread through CI — vike-core declares no features at all, and a
//! feature the derived roster lane does not pass would leave the `tests/` binaries uncovered.

#![allow(dead_code)] // each including binary uses a subset of the constructors

use std::path::{Path, PathBuf};

/// A scratch directory owned by the test that allocated it, removed when the guard drops.
#[derive(Debug)]
pub(crate) struct Scratch {
    /// The owning temp root. Never read — held so its `Drop` runs at the end of the test.
    _root: tempfile::TempDir,
    /// The path handed to the code under test: the root itself, or a child of it.
    path: PathBuf,
}

impl Scratch {
    /// A path INSIDE a fresh temp root that does **not** exist yet — for the suites whose code
    /// under test calls `create_dir_all` itself, and the ones that assert on a directory which
    /// never appears. The root is still owned, so whatever the test creates there is cleaned up.
    pub(crate) fn reserved(tag: &str) -> Self {
        let root = Self::root(tag);
        let path = root.path().join("j");
        Self { _root: root, path }
    }

    /// A directory that already EXISTS — for the suites that need it in place before the code
    /// under test runs.
    pub(crate) fn created(tag: &str) -> Self {
        let root = Self::root(tag);
        let path = root.path().to_path_buf();
        Self { _root: root, path }
    }

    /// The guarded path. `Deref` covers most uses; this is for call sites that read better named,
    /// and for the generic `impl Into<PathBuf>` parameters that get no deref coercion.
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// The temp root, prefixed with the caller's tag so a directory seen mid-run is attributable.
    fn root(tag: &str) -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix(&format!("vjl-{tag}-"))
            .tempdir()
            .expect("create scratch temp dir")
    }
}

impl std::ops::Deref for Scratch {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.path
    }
}

impl AsRef<Path> for Scratch {
    fn as_ref(&self) -> &Path {
        &self.path
    }
}

/// `replay_cli.rs` hands the directory straight to `std::process::Command::arg`, whose bound is
/// `AsRef<OsStr>` — a generic parameter, so it gets no deref coercion from the `Deref` above.
impl AsRef<std::ffi::OsStr> for Scratch {
    fn as_ref(&self) -> &std::ffi::OsStr {
        self.path.as_os_str()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole point: the directory and its contents are gone once the guard drops. A journal
    /// segment is `posix_fallocate`d to its full size, so "the contents too" is the 211 GB half.
    #[test]
    fn drop_removes_the_directory_and_its_contents() {
        let (root, inner) = {
            let s = Scratch::created("selftest-drop");
            std::fs::write(s.join("journal-00000000.vjl"), b"a fully allocated segment")
                .expect("write into scratch");
            (s.path().to_path_buf(), s.join("journal-00000000.vjl"))
        };
        assert!(!inner.exists(), "the segment is gone");
        assert!(!root.exists(), "and so is the directory holding it");
    }

    /// Unwinding runs destructors, so a test that FAILS still cleans up. This is the property that
    /// makes the leak unable to recur on an assertion failure — a keep-on-failure design would
    /// have gone on leaking exactly where a leak is least likely to be noticed.
    #[test]
    fn a_panicking_test_still_cleans_up() {
        let seen = std::sync::Arc::new(std::sync::Mutex::new(PathBuf::new()));
        let sink = std::sync::Arc::clone(&seen);
        let hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {})); // keep the deliberate panic off the test log
        let r = std::panic::catch_unwind(move || {
            let s = Scratch::created("selftest-panic");
            std::fs::write(s.join("journal-00000000.vjl"), b"segment").expect("write");
            *sink.lock().unwrap() = s.path().to_path_buf();
            panic!("the test under observation fails here");
        });
        std::panic::set_hook(hook);
        assert!(r.is_err(), "the observed closure really did panic");
        let path = seen.lock().unwrap().clone();
        assert!(path.components().count() > 1, "the closure ran far enough to allocate");
        assert!(!path.exists(), "unwinding dropped the guard and removed the directory");
    }

    /// `reserved` hands back a path that does not exist — the journal's own `create_dir_all` is
    /// what materialises it, and one suite asserts on a directory that is never created at all.
    #[test]
    fn reserved_does_not_create_but_created_does() {
        let r = Scratch::reserved("selftest-reserved");
        assert!(!r.exists(), "reserved() leaves the path for the code under test to create");
        assert!(r.parent().expect("a parent").exists(), "…inside a root that IS owned");
        let c = Scratch::created("selftest-created");
        assert!(c.exists(), "created() materialises it");
    }

    /// A path the code under test creates at a `reserved` location is still cleaned up — the guard
    /// owns the ROOT, not just the leaf, which is what makes `reserved` safe.
    #[test]
    fn reserved_still_cleans_up_what_the_code_under_test_creates() {
        let (leaf, root) = {
            let s = Scratch::reserved("selftest-reserved-cleanup");
            std::fs::create_dir_all(&*s).expect("the journal's own create_dir_all");
            std::fs::write(s.join("journal-00000000.vjl"), b"segment").expect("write");
            (s.path().to_path_buf(), s.parent().expect("a parent").to_path_buf())
        };
        assert!(!leaf.exists(), "the journal's directory is gone");
        assert!(!root.exists(), "and the owning root with it");
    }

    /// Two allocations sharing a tag are distinct, so a helper that mints one per test cannot hand
    /// two tests the same directory — including two CI lanes sharing one `/tmp`.
    #[test]
    fn the_same_tag_twice_is_two_directories() {
        let a = Scratch::created("selftest-dup");
        let b = Scratch::created("selftest-dup");
        assert_ne!(a.path(), b.path(), "the random suffix separates them");
        assert!(a.exists() && b.exists(), "both exist independently");
    }

    /// The tag is in the name, so a directory observed mid-run names the test that owns it.
    #[test]
    fn the_path_carries_the_tag() {
        let s = Scratch::created("selftest-tagged");
        let name = s.file_name().expect("a file name").to_string_lossy().into_owned();
        assert!(name.starts_with("vjl-selftest-tagged-"), "got {name}");
    }
}
