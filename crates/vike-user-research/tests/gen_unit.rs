//! Unit tests for the PURE generator (`vike_user_research::codegen`) on synthetic trees — the exact
//! code `build.rs` runs (it `include!`s the same file). Temp trees are built with std only (no
//! tempfile dep): a unique dir under the target-local temp root, removed at the end of each test.
//!
//! The rerun WATCH (`src/watch.rs`, also `include!`d by `build.rs`) is the strategy host's file,
//! byte for byte, and its mechanism is tested there (`crates/vike-user-strategies/tests/gen_unit.rs`).
//! Held here: the identity of the two copies, and THIS host's read set — which directories its scan
//! reads, and that only a write inside them re-runs its build script.

use std::fs;
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::time::{Duration, SystemTime};

use vike_model::paths::state_path::{RESEARCH_SUBDIR, RHAI_SUBDIR, RUST_SUBDIR, STUDIES_SUBDIR};
use vike_user_research::codegen::{
    ScannedStudy, render, rhai_tier, rust_tier, scan, scanned_dirs, valid_name,
};

/// A scratch user_data root, unique per test, cleaned on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir()
            .join(format!("vike-user-research-gen-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }
    /// A compiled-tier study folder. `entry` writes `<name>.rs`; `extra` writes one more file.
    fn rust_study(&self, name: &str, entry: bool, extra: Option<&str>) {
        let d = rust_tier(&self.0).join(name);
        fs::create_dir_all(&d).unwrap();
        if entry {
            fs::write(d.join(format!("{name}.rs")), "// entry\n").unwrap();
        }
        if let Some(f) = extra {
            fs::write(d.join(f), "// extra\n").unwrap();
        }
    }
    /// An interpreted-tier study folder — never compiled, only noticed.
    fn rhai_study(&self, name: &str) {
        let d = rhai_tier(&self.0).join(name);
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join(format!("{name}.rhai")), "// entry\n").unwrap();
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// The scanned path is `state_path`'s, component for component — never a literal. If this ever
/// fails, the scan and `vike_model::paths::state_path::user_studies_dir` have come to disagree about where
/// a study lives, which is the whole failure this test exists to make loud.
#[test]
fn the_scanned_tier_is_spelled_through_state_path() {
    let root = PathBuf::from("Z:/proj/user_data");
    let expected = root.join(RESEARCH_SUBDIR).join(STUDIES_SUBDIR).join(RUST_SUBDIR);
    assert_eq!(rust_tier(&root), expected);
    assert_eq!(rhai_tier(&root), root.join(RESEARCH_SUBDIR).join(STUDIES_SUBDIR).join(RHAI_SUBDIR));
}

#[test]
fn absent_root_scans_empty_without_errors() {
    let out = scan(std::path::Path::new("Z:/definitely/absent/user_data"));
    assert!(out.studies.is_empty());
    assert!(out.errors.is_empty());
    assert!(out.warnings.is_empty());
}

#[test]
fn scan_finds_entries_sorted_and_skips_a_recipes_only_folder() {
    let s = Scratch::new("happy");
    s.rust_study("beta", true, Some("baseline.toml"));
    s.rust_study("alpha", true, None);
    s.rust_study("recipes_only", false, Some("baseline.toml")); // no .rs at all: silent
    let out = scan(&s.0);
    assert!(out.errors.is_empty(), "{:?}", out.errors);
    assert!(out.warnings.is_empty(), "{:?}", out.warnings);
    let names: Vec<&str> = out.studies.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["alpha", "beta"], "sorted, recipes-only folder absent");
}

/// The one place this scan is STRICTER than the strategy tier's: Rust is present and none of it
/// would ever be compiled, so it is an error naming the path — not a silent skip.
#[test]
fn rust_without_the_matching_entry_name_is_an_error() {
    let s = Scratch::new("missing-entry");
    s.rust_study("mystudy", false, Some("study.rs"));
    let out = scan(&s.0);
    assert!(out.studies.is_empty());
    assert_eq!(out.errors.len(), 1, "{:?}", out.errors);
    assert!(out.errors[0].contains("mystudy"), "{:?}", out.errors);
    assert!(out.errors[0].contains("mystudy.rs"), "names the file it wanted: {:?}", out.errors);
}

#[test]
fn a_bad_folder_name_is_an_error_naming_the_path() {
    let s = Scratch::new("badname");
    s.rust_study("Bad-Name", true, None);
    let out = scan(&s.0);
    assert!(out.studies.is_empty());
    assert_eq!(out.errors.len(), 1, "{:?}", out.errors);
    assert!(out.errors[0].contains("Bad-Name"));
}

/// A name in BOTH tiers is ambiguous to whatever resolves a study by name — a WARNING, because the
/// interpreted tier still runs on a binary install and refusing to build would remove the
/// resolution the operator has left.
#[test]
fn a_name_in_both_tiers_warns_and_still_builds() {
    let s = Scratch::new("bothtiers");
    s.rust_study("twin", true, None);
    s.rhai_study("twin");
    s.rust_study("only_rust", true, None);
    let out = scan(&s.0);
    assert!(out.errors.is_empty(), "{:?}", out.errors);
    let names: Vec<&str> = out.studies.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["only_rust", "twin"], "the compiled twin is still registered");
    assert_eq!(out.warnings.len(), 1, "{:?}", out.warnings);
    assert!(out.warnings[0].contains("twin"));
    assert!(out.warnings[0].contains(RHAI_SUBDIR), "{:?}", out.warnings);
}

#[test]
fn valid_name_charset() {
    assert!(valid_name("cohort_v2"));
    assert!(!valid_name("Cohort"));
    assert!(!valid_name("2fast"));
    assert!(!valid_name("dash-ed"));
    assert!(!valid_name(""));
}

#[test]
fn render_empty_and_nonempty_shapes() {
    let empty = render(&[]);
    assert!(empty.contains("USER_STUDIES: &[&str] = &[]"));
    assert!(empty.contains("let _ = name;"));
    // Even the empty registry carries the runner, so a consumer's call site is identical in a
    // checkout with no user_data and in one with a dozen studies.
    assert!(empty.contains("pub fn run_user_study("));

    let one = render(&[ScannedStudy {
        name: "cohort".into(),
        entry: PathBuf::from(r"C:\p\user_data\research\studies\rust\cohort\cohort.rs"),
    }]);
    assert!(
        one.contains(r#"#[path = "C:/p/user_data/research/studies/rust/cohort/cohort.rs"]"#),
        "backslashes rendered forward: {one}"
    );
    assert!(one.contains("pub mod user_cohort;"));
    assert!(one.contains(r#"USER_STUDIES: &[&str] = &["cohort"]"#));
    // The fn-pointer COERCION is the compile-time signature check; a plain call would not be one.
    assert!(
        one.contains(r#""cohort" => Some(user_cohort::run as vike_user_research::StudyFn),"#),
        "{one}"
    );
    // Absolute crate paths, never `crate::` — the same text has to compile in the lib and in an
    // integration test.
    assert!(!one.contains("crate::Study"), "{one}");
}

/// A MISSING user_data and an EMPTY one generate the same registry, byte for byte, with the same
/// (empty) errors and warnings. This is what makes it safe for `build.rs` to stop re-running the
/// scan while the root is absent: an absent root can produce nothing an empty one does not.
#[test]
fn a_missing_root_and_an_empty_root_generate_the_same_registry() {
    let s = Scratch::new("missing-vs-empty");
    let (missing, empty) = (s.0.join("absent"), s.0.join("empty"));
    fs::create_dir_all(&empty).unwrap();
    let (a, b) = (scan(&missing), scan(&empty));
    assert!(a.errors.is_empty() && b.errors.is_empty(), "{:?} / {:?}", a.errors, b.errors);
    assert!(a.warnings.is_empty() && b.warnings.is_empty(), "{:?} / {:?}", a.warnings, b.warnings);
    assert_eq!(render(&a.studies), render(&b.studies));
}

/// `src/watch.rs` is ONE mechanism kept as TWO files — this host's and
/// `crates/vike-user-strategies/src/watch.rs` — because neither crate depends on the other, and a
/// `build.rs` reaching into a sibling package's `src/` would be an edge no manifest and no layer gate
/// can see. Nothing else holds the two equal, so this does.
#[test]
fn the_watch_module_is_byte_identical_in_both_hosts() {
    let here = Path::new(env!("CARGO_MANIFEST_DIR"));
    let read =
        |p: &Path| fs::read_to_string(p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()));
    let ours = read(&here.join("src").join("watch.rs"));
    let theirs = read(&here.join("..").join("vike-user-strategies").join("src").join("watch.rs"));
    assert!(
        ours == theirs,
        "crates/vike-user-research/src/watch.rs and crates/vike-user-strategies/src/watch.rs have \
         drifted apart: edit both, or neither"
    );
}

// ── The rerun WATCH over what the scan reads ──────────────────────────────────────────────────────
//
// `build.rs` watches `scanned_dirs` — never the whole user_data, whose recursive watch re-ran this
// script after every write to `runs/` or `logs/`. The mechanism is the strategy host's file and its
// behaviour is tested there; what is THIS host's own is the read set, held below, and a model check
// that this host's two tiers re-run it and nothing else does.

/// The scan reads the two tiers and NOTHING else: a tree crowded with everything else a user_data
/// holds — the STRATEGY tier included — scans the same as the tiers alone.
#[test]
fn the_scan_reads_the_two_tiers_and_nothing_else() {
    let lean = Scratch::new("read-set-lean");
    lean.rust_study("alpha", true, Some("baseline.toml"));
    lean.rhai_study("alpha");
    let crowded = Scratch::new("read-set-crowded");
    crowded.rust_study("alpha", true, Some("baseline.toml"));
    crowded.rhai_study("alpha");
    for rel in [
        "runs/1790000000-abc-0/manifest.json",
        "logs/compile.log",
        "research/notebooks/n.ipynb",
        "research/studies/python/x/x.py",
        "strategies/rust/s/s.rs",
    ] {
        write(&crowded.0, rel);
    }
    let (a, b) = (scan(&lean.0), scan(&crowded.0));
    assert_eq!(a.errors, b.errors);
    let names = |o: &vike_user_research::codegen::ScanOutcome| -> Vec<String> {
        o.studies.iter().map(|s| s.name.clone()).collect()
    };
    assert_eq!(names(&a), names(&b));
    assert_eq!(a.warnings.len(), b.warnings.len(), "{:?} / {:?}", a.warnings, b.warnings);
    assert_eq!(scanned_dirs(&crowded.0), [rust_tier(&crowded.0), rhai_tier(&crowded.0)]);
}

#[cfg(unix)]
const HOUR: Duration = Duration::from_secs(3600);

fn write(root: &Path, rel: &str) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, "x\n").unwrap();
}

/// Set every mtime at or under `path` to two hours ago, links excluded.
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

/// The same MODEL of `cargo_util::paths::mtime_recursive` as
/// `crates/vike-user-strategies/tests/gen_unit.rs`'s `newest_mtime`.
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

/// cargo's verdict over a directive set, under the model.
#[cfg(unix)]
fn reruns(lines: &[String], started: SystemTime) -> bool {
    lines.iter().any(|l| {
        let w = Path::new(l.strip_prefix("cargo:rerun-if-changed=").expect("a directive"));
        newest_mtime(w).is_none_or(|t| t > started)
    })
}

/// One "previous build": the REAL directives over the REAL read set, then the tree aged to before
/// that build started.
#[cfg(unix)]
fn previous_build(root: &Path, out: &Path, started: SystemTime) -> Vec<String> {
    let lines =
        vike_user_research::watch::rerun_directives(root, &scanned_dirs(root), out, started);
    age(root);
    lines
}

/// THE CI-RUNNER CASE, for this host: a user_data holding neither tier, written into by tests. Only a
/// write into one of THIS host's two tiers re-runs its build script — not `runs/`, not `logs/`, and
/// not the strategy tier, which is the other host's.
#[cfg(unix)]
#[test]
fn only_a_write_inside_a_scanned_tier_reruns_the_build_script() {
    let s = Scratch::new("watch-tiers");
    let (root, out) = (s.0.join("user_data"), s.0.join("out"));
    fs::create_dir_all(&out).unwrap();
    write(&root, "runs/1789975661-fb10947ede15b4d5-0/search.json");
    let started = SystemTime::now() - HOUR;

    let lines = previous_build(&root, &out, started);
    assert!(!reruns(&lines, started), "nothing changed: the next build must be fresh");
    for rel in [
        "runs/r/manifest.json",
        "logs/compile.log",
        "research/notebooks/n.ipynb",
        "strategies/rust/s/s.rs",
    ] {
        write(&root, rel);
        assert!(!reruns(&lines, started), "a write to {rel} must not re-run the build script");
    }
    write(&root, "research/studies/rhai/r/r.rhai");
    assert!(reruns(&lines, started), "the interpreted tier appearing must re-run it");

    let lines = previous_build(&root, &out, started);
    assert!(!reruns(&lines, started));
    write(&root, "research/studies/rust/q/q.rs");
    assert!(reruns(&lines, started), "the compiled tier appearing must re-run it");

    let lines = previous_build(&root, &out, started);
    assert!(!reruns(&lines, started));
    write(&root, "research/studies/rhai/r/r.rhai");
    assert!(reruns(&lines, started), "a write inside a present tier must re-run it");
}
