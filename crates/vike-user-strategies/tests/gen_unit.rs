//! Unit tests for the PURE generator (`vike_user_strategies::codegen`) on synthetic trees — the exact
//! code `build.rs` runs (it `include!`s the same file) — and ONE test that this host's read set,
//! fed to the rerun WATCH (`vike_model::host_build::watch`), re-runs the build script exactly when
//! it should. The watch mechanism's own properties are tested once, in
//! `crates/vike-model/tests/host_build.rs`. Temp trees are built with std only (no tempfile dep): a
//! unique dir under the system temp dir, owned by a `vike_model::scratch::ScratchDir` that removes
//! it at the end of each test.

use std::fs;
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::time::{Duration, SystemTime};

#[cfg(unix)]
use vike_model::host_build::watch;
use vike_model::scratch::ScratchDir;
#[cfg(unix)]
use vike_model::test_support::mtime::{age, reruns, write_marker};
use vike_user_strategies::codegen::{ScannedStrategy, render, scan, scanned_dirs};

/// A scratch user_data root, unique per test (the [`ScratchDir`] it wraps appends `-<pid>-<seq>`
/// to the tag), cleaned on drop.
struct Scratch(ScratchDir);

impl Scratch {
    fn new(tag: &str) -> Self {
        let tag = format!("vike-user-strategies-gen-{tag}");
        let dir = ScratchDir::create_in(&std::env::temp_dir(), &tag).unwrap();
        Scratch(dir)
    }
    fn strategy(&self, name: &str, with_entry: bool, manifest: Option<&str>) {
        let d = self.0.join("strategies").join("rust").join(name);
        fs::create_dir_all(&d).unwrap();
        if with_entry {
            fs::write(d.join(format!("{name}.rs")), "// entry\n").unwrap();
        }
        if let Some(m) = manifest {
            fs::write(d.join("strategy.toml"), m).unwrap();
        }
    }
}

#[test]
fn absent_root_scans_empty_without_errors() {
    let out = scan(Path::new("Z:/definitely/absent/user_data"));
    assert!(out.strategies.is_empty());
    assert!(out.errors.is_empty());
}

#[test]
fn scan_finds_entries_skips_presets_only_and_reads_live() {
    let s = Scratch::new("happy");
    s.strategy("alpha", true, None);
    s.strategy("beta", true, Some("live = true\n"));
    s.strategy("built_in_presets", false, None); // presets-only: skipped silently
    let out = scan(&s.0);
    assert!(out.errors.is_empty(), "{:?}", out.errors);
    let names: Vec<&str> = out.strategies.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["alpha", "beta"], "sorted, presets-only folder absent");
    assert!(!out.strategies[0].live && out.strategies[1].live);
}

#[test]
fn bad_name_and_bad_manifest_are_errors_naming_the_path() {
    let s = Scratch::new("errors");
    s.strategy("Bad-Name", true, None);
    s.strategy("okay", true, Some("live = \"yes\"\n")); // non-boolean live
    let out = scan(&s.0);
    assert!(out.strategies.is_empty());
    assert_eq!(out.errors.len(), 2, "{:?}", out.errors);
    assert!(out.errors.iter().any(|e| e.contains("Bad-Name")));
    assert!(out.errors.iter().any(|e| e.contains("okay") && e.contains("live")));
}

#[test]
fn render_empty_and_nonempty_shapes() {
    let empty = render(&[]);
    assert!(empty.contains("USER_STRATEGIES: &[&str] = &[]"));
    assert!(empty.contains("let _ = (name, params);"));

    let one = render(&[ScannedStrategy {
        name: "abs".into(),
        entry: PathBuf::from(r"C:\some\user_data\strategies\rust\abs\abs.rs"),
        live: true,
    }]);
    assert!(
        one.contains(r#"#[path = "C:/some/user_data/strategies/rust/abs/abs.rs"]"#),
        "backslashes rendered forward: {one}"
    );
    assert!(one.contains("pub mod user_abs;"));
    assert!(one.contains(r#"USER_LIVE_CAPABLE: &[&str] = &["abs"]"#));
    assert!(one.contains(r#""abs" => Some(user_abs::build::<B>(params)),"#));
}

// ── The rerun WATCH over this host's read set ───────────────────────────────────────────────────
//
// `build.rs` used to name the whole user_data root to cargo — stale on EVERY build while absent,
// and stale after ANY write under it while present (`runs/`, `logs/`, …). Either way both build
// scripts re-ran and everything above them recompiled.
// `crates/vike-model/src/host_build/watch.rs`'s module doc carries the cargo source behind that
// and the cure, and `crates/vike-model/tests/host_build.rs` holds the cure's properties. Here: the
// two facts that make watching only `scanned_dirs` SAFE for this host's scan, and one run of this
// host's real directives against a MODEL of cargo's rule
// (`vike_model::test_support::mtime::reruns`); the lane check in
// `crates/vike-user-strategies/build.rs`'s module doc holds it against the real cargo.

/// A MISSING user_data and an EMPTY one generate the same registry, byte for byte. This is what makes
/// it safe to stop re-running the scan while the root is absent: an absent root can produce nothing
/// an empty one does not.
#[test]
fn a_missing_root_and_an_empty_root_generate_the_same_registry() {
    let s = Scratch::new("missing-vs-empty");
    let (missing, empty) = (s.0.join("absent"), s.0.join("empty"));
    fs::create_dir_all(&empty).unwrap();
    let (a, b) = (scan(&missing), scan(&empty));
    assert!(a.errors.is_empty() && b.errors.is_empty(), "{:?} / {:?}", a.errors, b.errors);
    assert_eq!(render(&a.strategies), render(&b.strategies));
}

/// The scan reads `scanned_dirs` and NOTHING else: a tree crowded with everything else a user_data
/// holds generates the same registry as the tier alone. This is what makes it safe to watch only
/// those directories.
#[test]
fn files_outside_the_scanned_dirs_do_not_change_the_registry() {
    let lean = Scratch::new("read-set-lean");
    lean.strategy("alpha", true, Some("live = true\n"));
    let crowded = Scratch::new("read-set-crowded");
    crowded.strategy("alpha", true, Some("live = true\n"));
    for (dir, file) in [
        ("runs/1790000000-abc-0", "manifest.json"),
        ("logs", "compile.log"),
        ("profiles", "backtest.toml"),
        ("strategies/rhai/sma", "sma.rhai"),
        ("strategies/stray", "stray.rs"),
    ] {
        let d = crowded.0.join(dir);
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join(file), "x\n").unwrap();
    }
    let (a, b) = (scan(&lean.0), scan(&crowded.0));
    assert_eq!(a.errors, b.errors);
    // Entry paths differ by scratch root; the SHAPE (names, live flags) is what the tree decides.
    let shape = |o: &vike_user_strategies::codegen::ScanOutcome| -> Vec<(String, bool)> {
        o.strategies.iter().map(|s| (s.name.clone(), s.live)).collect()
    };
    assert_eq!(shape(&a), shape(&b));
    assert_eq!(scanned_dirs(&crowded.0), [crowded.0.join("strategies").join("rust")]);
}

/// The instant cargo STARTED the previous run — the reference a watched path's mtime is compared
/// against. Fixture trees are `age`d to an hour BEFORE it (`now - 2 * HOUR`, passed to
/// `vike_model::test_support::mtime::age`, which may not read the clock itself) and every write a
/// test then makes lands an hour AFTER it, so no filesystem clock granularity can blur which side a
/// file is on.
#[cfg(unix)]
fn previous_run_started() -> SystemTime {
    SystemTime::now() - HOUR
}

#[cfg(unix)]
const HOUR: Duration = Duration::from_secs(3600);

/// One "previous build": the directives `build.rs` prints for user_data `root` (the REAL call, over
/// the REAL scan's read set), then the tree aged to before that build started (two hours ago, an
/// hour before [`previous_run_started`]).
#[cfg(unix)]
fn previous_build(root: &Path, out: &Path, started: SystemTime) -> Vec<String> {
    let lines = watch::rerun_directives(root, &scanned_dirs(root), out, started);
    age(root, SystemTime::now() - 2 * HOUR);
    lines
}

/// A present tier: writes OUTSIDE it leave the next build fresh, a write INSIDE it — a new strategy,
/// an edited `strategy.toml` — re-runs it.
#[cfg(unix)]
#[test]
fn only_a_write_inside_the_scanned_tier_reruns_the_build_script() {
    let s = Scratch::new("watch-present-tier");
    s.strategy("alpha", true, Some("live = false\n"));
    let out = s.0.join("out");
    fs::create_dir_all(&out).unwrap();
    let root = s.0.path();
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
