//! The rerun WATCH (`vike_model::host_build::watch`) over a scan's read set, held against a MODEL of
//! cargo's rerun-if-changed rule. Moved from `vike-user-strategies`' `tests/gen_unit/watch_model.rs`
//! with the mechanism; the read set here is a stand-in for a host's (the strategy tier's path), and
//! each host keeps the tests that pin its OWN read set.
//!
//! `crates/vike-model/src/host_build/watch.rs`'s module doc carries the cargo source behind the
//! defects and the cure, and the lane check that holds them against the real cargo; these hold the
//! cure's properties against the model (`vike_model::test_support::mtime`).

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use vike_model::host_build::watch;
use vike_model::test_support::mtime::watched;
#[cfg(unix)]
use vike_model::test_support::mtime::{age, reruns, write_marker};

use super::{folder, scratch};

/// The stand-in host's read set: one compiled tier, `<user_data>/strategies/rust`.
fn scanned_dirs(root: &Path) -> Vec<PathBuf> {
    vec![root.join("strategies").join("rust")]
}

/// A strategy-shaped folder in that tier: `<name>.rs`, plus `strategy.toml` when `manifest`.
fn strategy(root: &Path, name: &str, manifest: bool) {
    let rs = format!("{name}.rs");
    let tier = scanned_dirs(root).remove(0);
    if manifest {
        folder(&tier, name, &[rs.as_str(), "strategy.toml"]);
    } else {
        folder(&tier, name, &[rs.as_str()]);
    }
}

/// Every directive names a scanned directory or its stand-in — NEVER the user_data root, whose
/// recursive watch is what re-ran the scripts after every write to `runs/`.
#[test]
fn the_watch_names_the_scanned_dirs_never_the_root() {
    let dir = scratch("watch-names");
    strategy(&dir, "alpha", false);
    let out = dir.join("out");
    fs::create_dir_all(&out).unwrap();
    let lines = watch::rerun_directives(&dir, &scanned_dirs(&dir), &out, previous_run_started());
    assert_eq!(watched(&lines), scanned_dirs(&dir), "a present tier is watched directly");
    assert!(
        !watched(&lines).contains(&dir.to_path_buf()),
        "the user_data root itself is never watched"
    );
}

/// A directory that is not STRICTLY below the root gets no stand-in — the answer could otherwise
/// escape OUT_DIR — and falls back to being watched directly.
#[test]
fn a_dir_not_strictly_below_the_root_is_watched_directly() {
    let dir = scratch("watch-escape");
    let (root, out) = (dir.join("user_data"), dir.join("out"));
    fs::create_dir_all(&out).unwrap();
    let elsewhere = dir.join("elsewhere").join("rust");
    let t = previous_run_started();
    assert_eq!(watch::rerun_path(&root, &root, &out, t), root);
    assert_eq!(watch::rerun_path(&root, &elsewhere, &out, t), elsewhere);
    assert!(!out.join(watch::WATCH_DIR).exists(), "no stand-in was made for either");
}

/// The instant cargo STARTED the previous run — the reference a watched path's mtime is compared
/// against. Fixture trees are `age`d to an hour BEFORE it (`now - 2 * HOUR`, passed to
/// `vike_model::test_support::mtime::age`, which may not read the clock itself) and every write a
/// test then makes lands an hour AFTER it, so no filesystem clock granularity can blur which side a
/// file is on.
fn previous_run_started() -> SystemTime {
    SystemTime::now() - HOUR
}

const HOUR: Duration = Duration::from_secs(3600);

/// One "previous build": the directives a build script prints for user_data `root` (the REAL
/// call, over the read set), then the tree aged to before that build started (two hours ago, an
/// hour before [`previous_run_started`]).
#[cfg(unix)]
fn previous_build(root: &Path, out: &Path, started: SystemTime) -> Vec<String> {
    previous_build_over(root, &scanned_dirs(root), out, started)
}

#[cfg(unix)]
fn previous_build_over(
    root: &Path,
    scanned: &[PathBuf],
    out: &Path,
    started: SystemTime,
) -> Vec<String> {
    let lines = watch::rerun_directives(root, scanned, out, started);
    age(root, SystemTime::now() - 2 * HOUR);
    lines
}

/// THE CI-RUNNER CASE: a user_data the scan has nothing to read in, but that tests and the program
/// write into. Writes OUTSIDE the scanned tier must leave the next build fresh; the tier appearing
/// must not.
#[cfg(unix)]
#[test]
fn a_present_root_without_the_tier_stays_fresh_until_the_tier_appears() {
    let dir = scratch("watch-runner");
    let (root, out) = (dir.join("user_data"), dir.join("out"));
    fs::create_dir_all(&out).unwrap();
    write_marker(&root, "runs/1789975661-fb10947ede15b4d5-0/search.json");
    let started = previous_run_started();
    let lines = previous_build(&root, &out, started);
    assert!(!reruns(&lines, started), "nothing changed: the next build must be fresh");

    for rel in
        ["runs/1790000000-abc-0/manifest.json", "logs/compile.log", "strategies/rhai/a/a.rhai"]
    {
        write_marker(&root, rel);
        assert!(!reruns(&lines, started), "a write to {rel} must not re-run the build script");
    }
    write_marker(&root, "strategies/rust/probe/probe.rs");
    assert!(reruns(&lines, started), "the scanned tier appearing must re-run it");
}

/// A present tier: writes OUTSIDE it leave the next build fresh, a write INSIDE it — a new strategy,
/// an edited `strategy.toml` — re-runs it.
#[cfg(unix)]
#[test]
fn only_a_write_inside_the_scanned_tier_reruns_the_build_script() {
    let dir = scratch("watch-present-tier");
    strategy(&dir, "alpha", true);
    let out = dir.join("out");
    fs::create_dir_all(&out).unwrap();
    let root = dir.path();
    let started = previous_run_started();
    let lines = previous_build(root, &out, started);
    assert!(!reruns(&lines, started));

    for rel in [
        "runs/r/trials.jsonl",
        "logs/compile.log",
        "strategies/README.md",
        "backtest_results/b.json",
    ] {
        write_marker(root, rel);
        assert!(!reruns(&lines, started), "a write to {rel} must not re-run the build script");
    }
    write_marker(root, "strategies/rust/beta/beta.rs");
    assert!(reruns(&lines, started), "a new strategy folder must re-run it");

    age(root, SystemTime::now() - 2 * HOUR);
    assert!(!reruns(&lines, started));
    write_marker(root, "strategies/rust/alpha/strategy.toml");
    assert!(reruns(&lines, started), "an edited strategy.toml must re-run it");
}

/// No user_data at all — every fresh clone. Fresh until the tier appears (`vike-cli init`), and a
/// user_data that appears WITHOUT the tier changes nothing.
#[cfg(unix)]
#[test]
fn an_absent_root_stays_fresh_until_its_tier_appears() {
    let dir = scratch("watch-absent-root");
    let (root, out) = (dir.join("user_data"), dir.join("out"));
    fs::create_dir_all(&out).unwrap();
    let started = previous_run_started();
    let lines = previous_build(&root, &out, started);
    assert!(watched(&lines).iter().all(|w| w.exists()), "every watched path exists: {lines:?}");
    assert!(!reruns(&lines, started), "absent root: the next build must be fresh");

    write_marker(&root, "runs/r/search.json");
    assert!(!reruns(&lines, started), "a user_data with no tier in it must not re-run it");
    write_marker(&root, "strategies/rust/probe/probe.rs");
    assert!(reruns(&lines, started), "the tier appearing must re-run it");
}

/// A DELETED tier re-runs the build script once (it was watched directly), so the registry stops
/// `#[path]`-including files that no longer exist.
#[cfg(unix)]
#[test]
fn a_deleted_tier_reruns_the_build_script() {
    let dir = scratch("watch-deleted");
    strategy(&dir, "alpha", false);
    let out = dir.join("out");
    fs::create_dir_all(&out).unwrap();
    let started = previous_run_started();
    let lines = previous_build(&dir, &out, started);
    assert!(!reruns(&lines, started));
    fs::remove_dir_all(dir.join("strategies").join("rust")).unwrap();
    assert!(reruns(&lines, started), "a deleted tier must re-run the build script");
}

/// OUT_DIR outlives a run, so a second run over the same absent tier must leave the watch exactly as
/// fresh as the first did; and a moved root (a changed build-time override) re-points the link
/// rather than leaving it on the old one.
#[cfg(unix)]
#[test]
fn the_stand_in_survives_a_rerun_and_follows_a_moved_root() {
    let dir = scratch("watch-repoint");
    let out = dir.join("out");
    fs::create_dir_all(&out).unwrap();
    let (a, b) = (dir.join("a"), dir.join("b"));
    let started = previous_run_started();
    let tier = |root: &Path| scanned_dirs(root).remove(0);

    let stand_in = watch::rerun_path(&a, &tier(&a), &out, started);
    assert_eq!(stand_in, out.join(watch::WATCH_DIR).join("strategies").join("rust"));
    let link = stand_in.join(watch::WATCH_LINK);
    assert_eq!(fs::read_link(&link).unwrap(), tier(&a));

    assert_eq!(watch::rerun_path(&a, &tier(&a), &out, started), stand_in);
    assert!(!reruns(&[format!("cargo:rerun-if-changed={}", stand_in.display())], started));

    assert_eq!(watch::rerun_path(&b, &tier(&b), &out, started), stand_in);
    assert_eq!(fs::read_link(&link).unwrap(), tier(&b), "a moved root re-points the link");
    assert!(!reruns(&[format!("cargo:rerun-if-changed={}", stand_in.display())], started));
}

/// SEVERAL scanned directories (the research host reads two tiers) are each watched on their own:
/// each one appearing re-runs the build script, a write beside them does not, and two absent ones
/// get two stand-ins. Adapted from `vike-user-research`'s
/// `only_a_write_inside_a_scanned_tier_reruns_the_build_script`, over a host-independent read set.
#[cfg(unix)]
#[test]
fn each_of_several_scanned_dirs_is_watched_on_its_own() {
    let dir = scratch("watch-two-dirs");
    let (root, out) = (dir.join("user_data"), dir.join("out"));
    fs::create_dir_all(&out).unwrap();
    write_marker(&root, "runs/r/search.json");
    let scanned = [root.join("studies").join("rust"), root.join("studies").join("rhai")];
    let started = previous_run_started();

    let lines = previous_build_over(&root, &scanned, &out, started);
    assert_eq!(
        watched(&lines),
        [
            out.join(watch::WATCH_DIR).join("studies/rust"),
            out.join(watch::WATCH_DIR).join("studies/rhai")
        ],
        "two absent dirs, two stand-ins"
    );
    assert!(!reruns(&lines, started));
    for rel in ["runs/r/manifest.json", "studies/notebooks/n.ipynb", "strategies/rust/s/s.rs"] {
        write_marker(&root, rel);
        assert!(!reruns(&lines, started), "a write to {rel} must not re-run the build script");
    }
    write_marker(&root, "studies/rhai/r/r.rhai");
    assert!(reruns(&lines, started), "the second dir appearing must re-run it");

    let lines = previous_build_over(&root, &scanned, &out, started);
    assert!(!reruns(&lines, started));
    write_marker(&root, "studies/rust/q/q.rs");
    assert!(reruns(&lines, started), "the first dir appearing must re-run it");

    let lines = previous_build_over(&root, &scanned, &out, started);
    assert!(!reruns(&lines, started));
    write_marker(&root, "studies/rhai/r/r.rhai");
    assert!(reruns(&lines, started), "a write inside a present dir must re-run it");
}

/// Off Unix there is no link (`crates/vike-model/src/host_build/watch.rs`'s module doc argues why):
/// each scanned directory is watched directly, absent or not — slow while absent, never stale — but
/// the NARROWING still holds, so the root is never watched.
#[cfg(not(unix))]
#[test]
fn a_non_unix_host_watches_the_scanned_dirs_directly() {
    let dir = scratch("watch-nonunix");
    let (root, out) = (dir.join("user_data"), dir.join("out"));
    fs::create_dir_all(&out).unwrap();
    let lines = watch::rerun_directives(&root, &scanned_dirs(&root), &out, previous_run_started());
    assert_eq!(watched(&lines), scanned_dirs(&root));
}
