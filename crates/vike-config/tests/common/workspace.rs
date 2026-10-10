//! The workspace root every source-reading gate in this crate's `tests/` resolves paths against.
//!
//! ONE spelling, included by `#[path = "common/workspace.rs"] mod workspace;` in each test binary
//! that walks the tree (a `tests/*.rs` file is the crate root of its own binary, so a bare `mod`
//! would resolve beside it; cargo builds no binary from `tests/common/`, which has no `main.rs`).
//! `crates/vike-config/tests/layers_are_reachable.rs` keeps its own, differently shaped helper.

use std::path::{Path, PathBuf};

// Same spelling as `crates/vike-ops/tests/common/repo.rs`'s `workspace_root` (keeps the `..`); the
// `parent()` twins, e.g. `crates/vike-catalog/tests/baseline_artifact.rs`'s `repo_root`, do not.
/// Workspace root, resolved from `CARGO_MANIFEST_DIR` (never CWD) — the idiom every other
/// source-walking gate in this workspace uses.
pub fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}
