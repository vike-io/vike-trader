//! **The strategy FILES this crate's tests mount** — no strategy is compiled into the program, so a
//! test that needs "a strategy" writes a file into a temp `strategies/` folder and names it, exactly as
//! an operator does. The lib's unit tests reach this as `crate::test_strategy_files`; an integration
//! binary `#[path]`-includes this file as its own module.
//!
//! Every file here is a `.rhai` file, which runs in the daemon's own process: a `.rs` file runs in a
//! sandboxed process (`crate::config::ProfileDaemon::resolve_mount`'s `.rs` arm), and a test of the
//! resolution around that arm hands `ProfileDaemon::resolve_mount_with` a stand-in opener instead.

use std::path::Path;

/// `buy_hold.rhai`: one market buy of `size` (default 1) from flat, then hold — the shipped
/// `strategies/trend/buy_hold.rs`'s behaviour, spelled as a Rhai strategy file, its header saying it may run live.
pub(crate) const BUY_HOLD: &str = "\
// A test strategy file: buy `size` once from flat, then hold.
//
// ```toml
// [strategy]
// runs_on = \"live\"
// why = \"a test file: one market buy of `size` from flat, then nothing\"
//
// [params.size]
// type = \"number\"
// default = 1.0
// ```
const SIZE = param(\"size\", 1.0);
fn on_bar(strategy) { if strategy.position() == 0.0 { strategy.order(\"buy\", OrderSide::Long).size(SIZE); } }
";

/// `nothing.rhai`: the minimal mountable strategy — a hook that does nothing, no params, live.
pub(crate) const NOTHING: &str = "\
// A test strategy file that does nothing.
//
// ```toml
// [strategy]
// runs_on = \"live\"
// why = \"a test file: it places no order\"
// ```
fn on_bar() {}
";

/// `paper_only.rhai`: [`NOTHING`] whose header says nothing about where it runs (paper).
pub(crate) const PAPER_ONLY: &str = "\
// ```toml
// [strategy]
// ```
fn on_bar() {}
";

/// A temp strategies folder holding `files` (`(relative path, text)`), removed when the guard drops.
/// ⚠ Bind the guard for as long as anything reads the folder.
///
/// It replaces `own_script` (plan 2b), which carried this measurement and was the same remedy: the
/// script directory used to be `temp_dir().join(format!("…-{}", process::id()))`, which satisfies
/// `crates/vike-ops/tests/hygiene/temp_path_gate.rs` (the name is not fixed) and was still wrong twice
/// over. MEASURED on the CI box, 2026-08-25:
///
/// * **1,725 of these directories were sitting in `/tmp`**, dating back to 2026-08-18 — 1,082
///   owned by `the CI user` and 643 by `the operator`. Nothing ever deleted one, so every CI run and
///   every lane run leaked a directory permanently (44,840 across the workspace by that idiom).
/// * **A PID is REUSED.** When one collides with a directory the OTHER user created, the
///   `create_dir_all` succeeds (it already exists) and the `fs::write` fails with
///   PermissionDenied: a live, intermittent CI flake. PID-uniquification prevents collision WITHIN a
///   run; it does not prevent collision ACROSS users over time, and it leaks either way.
///
/// `tempfile::TempDir` fixes both halves at once: unique by construction, and self-deleting.
#[must_use]
pub(crate) fn folder(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("a temp strategies folder");
    write_into(dir.path(), files);
    dir
}

/// Write `files` (`(relative path, text)`) into the strategies folder `root`, creating subfolders.
pub(crate) fn write_into(root: &Path, files: &[(&str, &str)]) {
    for (relative, text) in files {
        let path = root.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create the file's folder");
        }
        std::fs::write(&path, text).expect("write the strategy file");
    }
}

/// [`folder`] with the three files above, under their own names.
#[must_use]
pub(crate) fn standard() -> tempfile::TempDir {
    folder(&[
        ("buy_hold.rhai", BUY_HOLD),
        ("nothing.rhai", NOTHING),
        ("paper_only.rhai", PAPER_ONLY),
    ])
}
