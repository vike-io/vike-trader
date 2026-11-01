//! The source-gate machinery this crate's integration binaries share, compiled ONCE here behind the
//! `test-support` feature (only this crate's self-referential dev edge turns it on) instead of once
//! per binary that `#[path]`-included `tests/common/{scanner,workspace}.rs`: the [`scanner`] both
//! consumption gates read the tree through, and [`workspace_root`], which every source-reading gate
//! here resolves paths against. The store seal every loader test hands `StoreLayer::Rows` is the
//! production one, [`crate::write::synthetic_seal`].

use std::path::{Path, PathBuf};

pub mod scanner;

/// The workspace root, from the CALLER's compile-time manifest directory (never CWD) — the idiom every
/// other source-walking gate in this workspace uses. A parameter rather than this crate's own
/// manifest directory because a compile-time path in `src/` is what
/// `crates/vike-ops/tests/hygiene/compile_time_path_gate.rs` refuses, and this file is `src/` under
/// `test-support`.
///
/// Same spelling as `crates/vike-ops/tests/common/repo.rs`'s `workspace_root` (keeps the `..`); the
/// `parent()` twins, e.g. `crates/vike-catalog/tests/baseline_artifact.rs`'s `repo_root`, do not.
/// `crates/vike-config/tests/layers_are_reachable.rs` keeps its own, differently shaped helper.
pub fn workspace_root(manifest_dir: &str) -> PathBuf {
    Path::new(manifest_dir).join("..").join("..")
}
