//! The throwaway PROJECT ROOT the daemon spawn tests `sigterm_stop.rs` and
//! `venue_feed_splice_smoke.rs` start the shipped binary in, each of which `#[path]`-includes it (a
//! bare `mod` would resolve beside the binary root, not here).

/// The project root one daemon is started in: an OWNED temp directory with the EMPTY `settings/`
/// store already inside it, removed when the returned guard drops.
///
/// ⚠ Returns the `TempDir` GUARD, and the caller must HOLD it for as long as the child runs, in a
/// field declared AFTER the child's reaper: fields drop in declaration order, so the daemon is
/// killed and reaped before the directory it was started in is removed. A hand-rolled
/// `impl Drop for Daemon` got that BACKWARDS once — a type's own `drop` runs BEFORE its fields', so
/// the `remove_dir_all` fired while the child was still alive.
///
/// This used to be `env::temp_dir().join(format!("vike_tradehub_{tag}_{pid}_{nanos}"))`, which
/// satisfies `crates/vike-ops/tests/hygiene/temp_path_gate.rs` (the name is not fixed) and was still
/// wrong twice over — the two defects `crates/vike-tradehub/src/config/tests/rhai.rs`'s `own_script`
/// records. MEASURED on the CI box, 2026-08-25:
///
/// * **Nothing ever deleted one.** `/tmp` held 44,840 leaked test directories of this shape, and
///   the feed-splice copy alone was adding ~45 a day — one per venue case per run, forever.
/// * **A PID is REUSED**, and the CI box runs these tests as TWO users (`the CI user` for CI, `the operator`
///   for the verification lanes). When a pid collides with a directory the OTHER user made, the
///   `create_dir_all` SUCCEEDS (it is already there) and the write into it fails
///   `PermissionDenied`. Uniquifying on a pid prevents collision WITHIN a run; it prevents nothing
///   ACROSS users over time, and it leaks either way.
///
/// `tempfile` fixes both halves at once: unique by construction, and self-deleting even when an
/// assertion unwinds. The tag stays in the NAME via `tempfile::Builder::prefix`, so a directory
/// seen mid-run is still attributable to the case that owns it.
pub fn temp_dir(tag: &str) -> tempfile::TempDir {
    let dir = tempfile::Builder::new()
        .prefix(&format!("vike_tradehub_{tag}_"))
        .tempdir()
        .expect("create the temp project root");
    std::fs::create_dir_all(dir.path().join("settings")).expect("create the temp settings dir");
    dir
}
