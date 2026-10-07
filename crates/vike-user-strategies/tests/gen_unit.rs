//! Unit tests for the PURE generator (`vike_user_strategies::codegen`) on synthetic trees — the exact
//! code `build.rs` runs (it `include!`s the same file) — and for the rerun WATCH over what that
//! scan reads (`vike_user_strategies::watch`, `include!`d the same way). Temp trees are built with
//! std only (no tempfile dep): a unique dir under the target-local temp root, removed at the end of
//! each test.

use std::fs;
#[cfg(unix)]
use std::path::Path;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use vike_user_strategies::codegen::{ScannedStrategy, render, scan, scanned_dirs, valid_name};
use vike_user_strategies::watch;

/// A scratch user_data root, unique per test, cleaned on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir()
            .join(format!("vike-user-strategies-gen-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
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

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn absent_root_scans_empty_without_errors() {
    let out = scan(std::path::Path::new("Z:/definitely/absent/user_data"));
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
fn valid_name_charset() {
    assert!(valid_name("abs_v2"));
    assert!(!valid_name("Abs"));
    assert!(!valid_name("2fast"));
    assert!(!valid_name("dash-ed"));
    assert!(!valid_name(""));
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

// ── The rerun WATCH ───────────────────────────────────────────────────────────────────────────────
//
// `build.rs` used to name the whole user_data root to cargo — stale on EVERY build while absent,
// and stale after ANY write under it while present (`runs/`, `logs/`, …). Either way both build
// scripts re-ran and everything above them recompiled.
// `crates/vike-user-strategies/src/watch.rs`'s module comment carries the cargo source behind that
// and the cure; these hold the cure's properties against a MODEL of cargo's rule, and the lane
// check in `crates/vike-user-strategies/build.rs`'s module doc holds them against the real cargo.

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

/// Every directive names a scanned directory or its stand-in — NEVER the user_data root, whose
/// recursive watch is what re-ran the scripts after every write to `runs/`.
#[test]
fn the_watch_names_the_scanned_dirs_never_the_root() {
    let s = Scratch::new("watch-names");
    s.strategy("alpha", true, None);
    let out = s.0.join("out");
    fs::create_dir_all(&out).unwrap();
    let lines = watch::rerun_directives(&s.0, &scanned_dirs(&s.0), &out, previous_run_started());
    assert_eq!(watched(&lines), scanned_dirs(&s.0), "a present tier is watched directly");
    assert!(!watched(&lines).contains(&s.0), "the user_data root itself is never watched");
}

/// A directory that is not STRICTLY below the root gets no stand-in — the answer could otherwise
/// escape OUT_DIR — and falls back to being watched directly.
#[test]
fn a_dir_not_strictly_below_the_root_is_watched_directly() {
    let s = Scratch::new("watch-escape");
    let (root, out) = (s.0.join("user_data"), s.0.join("out"));
    fs::create_dir_all(&out).unwrap();
    let elsewhere = s.0.join("elsewhere").join("rust");
    let t = previous_run_started();
    assert_eq!(watch::rerun_path(&root, &root, &out, t), root);
    assert_eq!(watch::rerun_path(&root, &elsewhere, &out, t), elsewhere);
    assert!(!out.join(watch::WATCH_DIR).exists(), "no stand-in was made for either");
}

/// The instant cargo STARTED the previous run — the reference a watched path's mtime is compared
/// against. Fixture trees are [`age`]d to an hour BEFORE it and every write a test then makes lands
/// an hour AFTER it, so no filesystem clock granularity can blur which side a file is on.
fn previous_run_started() -> SystemTime {
    SystemTime::now() - HOUR
}

const HOUR: Duration = Duration::from_secs(3600);

/// The paths a directive set names.
fn watched(lines: &[String]) -> Vec<PathBuf> {
    lines
        .iter()
        .map(|l| PathBuf::from(l.strip_prefix("cargo:rerun-if-changed=").expect("a directive")))
        .collect()
}

/// Set every mtime at or under `path` to two hours ago, links excluded: the tree as it stood before
/// the previous run started.
#[cfg(unix)]
fn age(path: &Path) {
    let Ok(meta) = fs::symlink_metadata(path) else { return };
    if meta.file_type().is_symlink() {
        return;
    }
    if meta.is_dir() {
        for e in fs::read_dir(path).unwrap().flatten() {
            age(&e.path());
        }
    }
    let then = SystemTime::now() - 2 * HOUR;
    fs::File::open(path).and_then(|f| f.set_modified(then)).unwrap();
}

/// `cargo_util::paths::mtime_recursive`, MODELLED: the newest mtime at or under `path`, following
/// symlinks and skipping whatever cannot be stat'ed — `None` only when `path` itself cannot be, which
/// cargo reports as `MissingFile`. A model, not cargo: the lane check in
/// `crates/vike-user-strategies/build.rs`'s module doc is what proves the real tool behaves this way.
#[cfg(unix)]
fn newest_mtime(path: &Path) -> Option<SystemTime> {
    let meta = fs::metadata(path).ok()?;
    let mut newest = meta.modified().ok()?;
    if let Ok(link) = fs::symlink_metadata(path)
        && link.file_type().is_symlink()
        && let Ok(t) = link.modified()
    {
        newest = newest.max(t);
    }
    if meta.is_dir()
        && let Ok(entries) = fs::read_dir(path)
    {
        for e in entries.flatten() {
            if let Some(t) = newest_mtime(&e.path()) {
                newest = newest.max(t);
            }
        }
    }
    Some(newest)
}

/// cargo's verdict over a directive set, under the model: the build script re-runs when ANY watched
/// path is missing, or holds anything newer than the previous run's start.
#[cfg(unix)]
fn reruns(lines: &[String], started: SystemTime) -> bool {
    watched(lines).iter().any(|w| newest_mtime(w).is_none_or(|t| t > started))
}

/// One "previous build": the directives `build.rs` prints for user_data `root` (the REAL call, over
/// the REAL scan's read set), then the tree aged to before that build started.
#[cfg(unix)]
fn previous_build(root: &Path, out: &Path, started: SystemTime) -> Vec<String> {
    let lines = watch::rerun_directives(root, &scanned_dirs(root), out, started);
    age(root);
    lines
}

#[cfg(unix)]
fn write(root: &Path, rel: &str) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, "x\n").unwrap();
}

/// THE CI-RUNNER CASE: a user_data the scan has nothing to read in, but that tests and the program
/// write into. Writes OUTSIDE the scanned tier must leave the next build fresh; the tier appearing
/// must not.
#[cfg(unix)]
#[test]
fn a_present_root_without_the_tier_stays_fresh_until_the_tier_appears() {
    let s = Scratch::new("watch-runner");
    let (root, out) = (s.0.join("user_data"), s.0.join("out"));
    fs::create_dir_all(&out).unwrap();
    write(&root, "runs/1789975661-fb10947ede15b4d5-0/search.json");
    let started = previous_run_started();
    let lines = previous_build(&root, &out, started);
    assert!(!reruns(&lines, started), "nothing changed: the next build must be fresh");

    for rel in
        ["runs/1790000000-abc-0/manifest.json", "logs/compile.log", "strategies/rhai/a/a.rhai"]
    {
        write(&root, rel);
        assert!(!reruns(&lines, started), "a write to {rel} must not re-run the build script");
    }
    write(&root, "strategies/rust/probe/probe.rs");
    assert!(reruns(&lines, started), "the scanned tier appearing must re-run it");
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
    let root = &s.0;
    let started = previous_run_started();
    let lines = previous_build(root, &out, started);
    assert!(!reruns(&lines, started));

    for rel in [
        "runs/r/trials.jsonl",
        "logs/compile.log",
        "strategies/README.md",
        "backtest_results/b.json",
    ] {
        write(root, rel);
        assert!(!reruns(&lines, started), "a write to {rel} must not re-run the build script");
    }
    write(root, "strategies/rust/beta/beta.rs");
    assert!(reruns(&lines, started), "a new strategy folder must re-run it");

    age(root);
    assert!(!reruns(&lines, started));
    write(root, "strategies/rust/alpha/strategy.toml");
    assert!(reruns(&lines, started), "an edited strategy.toml must re-run it");
}

/// No user_data at all — every fresh clone. Fresh until the tier appears (`vike-cli init`), and a
/// user_data that appears WITHOUT the tier changes nothing.
#[cfg(unix)]
#[test]
fn an_absent_root_stays_fresh_until_its_tier_appears() {
    let s = Scratch::new("watch-absent-root");
    let (root, out) = (s.0.join("user_data"), s.0.join("out"));
    fs::create_dir_all(&out).unwrap();
    let started = previous_run_started();
    let lines = previous_build(&root, &out, started);
    assert!(watched(&lines).iter().all(|w| w.exists()), "every watched path exists: {lines:?}");
    assert!(!reruns(&lines, started), "absent root: the next build must be fresh");

    write(&root, "runs/r/search.json");
    assert!(!reruns(&lines, started), "a user_data with no tier in it must not re-run it");
    write(&root, "strategies/rust/probe/probe.rs");
    assert!(reruns(&lines, started), "the tier appearing must re-run it");
}

/// A DELETED tier re-runs the build script once (it was watched directly), so the registry stops
/// `#[path]`-including files that no longer exist.
#[cfg(unix)]
#[test]
fn a_deleted_tier_reruns_the_build_script() {
    let s = Scratch::new("watch-deleted");
    s.strategy("alpha", true, None);
    let out = s.0.join("out");
    fs::create_dir_all(&out).unwrap();
    let started = previous_run_started();
    let lines = previous_build(&s.0, &out, started);
    assert!(!reruns(&lines, started));
    fs::remove_dir_all(s.0.join("strategies").join("rust")).unwrap();
    assert!(reruns(&lines, started), "a deleted tier must re-run the build script");
}

/// OUT_DIR outlives a run, so a second run over the same absent tier must leave the watch exactly as
/// fresh as the first did; and a moved root (a changed build-time override) re-points the link
/// rather than leaving it on the old one.
#[cfg(unix)]
#[test]
fn the_stand_in_survives_a_rerun_and_follows_a_moved_root() {
    let s = Scratch::new("watch-repoint");
    let out = s.0.join("out");
    fs::create_dir_all(&out).unwrap();
    let (a, b) = (s.0.join("a"), s.0.join("b"));
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

/// Off Unix there is no link (`crates/vike-user-strategies/src/watch.rs`'s module comment argues
/// why): each scanned directory is watched directly, absent or not — slow while absent, never stale
/// — but the NARROWING still holds, so the root is never watched.
#[cfg(not(unix))]
#[test]
fn a_non_unix_host_watches_the_scanned_dirs_directly() {
    let s = Scratch::new("watch-nonunix");
    let (root, out) = (s.0.join("user_data"), s.0.join("out"));
    fs::create_dir_all(&out).unwrap();
    let lines = watch::rerun_directives(&root, &scanned_dirs(&root), &out, previous_run_started());
    assert_eq!(watched(&lines), scanned_dirs(&root));
}
