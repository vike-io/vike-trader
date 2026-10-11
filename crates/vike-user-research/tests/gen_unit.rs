//! Unit tests for the PURE generator (`vike_user_research::codegen`) on synthetic trees — the exact
//! code `build.rs` runs (it `include!`s the same file). Temp trees are built with no tempfile dep:
//! a unique dir under the temp root, a `vike_model::scratch::ScratchDir` removed at the end of
//! each test.
//!
//! The rerun WATCH is the shared build kit's (`vike_model::host_build::watch`), and its mechanism
//! is tested there (`crates/vike-model/tests/host_build.rs`). Held here: THIS host's read set —
//! which directories its scan reads, and that only a write inside them re-runs its build script.

use std::fs;
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::time::{Duration, SystemTime};

use vike_model::paths::state_path::{RESEARCH_SUBDIR, RHAI_SUBDIR, RUST_SUBDIR, STUDIES_SUBDIR};
use vike_model::scratch::ScratchDir;
use vike_model::test_support::mtime::write_marker;
#[cfg(unix)]
use vike_model::test_support::mtime::{age, reruns};
use vike_user_research::codegen::{ScannedStudy, render, rhai_tier, rust_tier, scan, scanned_dirs};

/// A scratch user_data root, unique per test, cleaned on drop (by the `ScratchDir` it wraps).
struct Scratch(ScratchDir);

impl Scratch {
    fn new(tag: &str) -> Self {
        let tag = format!("vike-user-research-gen-{tag}");
        let dir = ScratchDir::create_in(&std::env::temp_dir(), &tag).unwrap();
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
    let out = scan(Path::new("Z:/definitely/absent/user_data"));
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

// ── The rerun WATCH over what the scan reads ──────────────────────────────────────────────────────
//
// `build.rs` watches `scanned_dirs` — never the whole user_data, whose recursive watch re-ran this
// script after every write to `runs/` or `logs/`. The mechanism is `vike_model::host_build::watch`
// and its behaviour is tested there; what is THIS host's own is the read set, held below, and a
// model check that this host's two tiers re-run it and nothing else does.

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
        write_marker(&crowded.0, rel);
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

/// One "previous build": the REAL directives over the REAL read set, then the tree aged to two
/// hours ago, before that build started (`started` is one hour ago at every call site). cargo's
/// verdict over them is `vike_model::test_support::mtime::reruns`, the shared model.
#[cfg(unix)]
fn previous_build(root: &Path, out: &Path, started: SystemTime) -> Vec<String> {
    let lines =
        vike_model::host_build::watch::rerun_directives(root, &scanned_dirs(root), out, started);
    age(root, SystemTime::now() - 2 * HOUR);
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
    write_marker(&root, "runs/1789975661-fb10947ede15b4d5-0/search.json");
    let started = SystemTime::now() - HOUR;

    let lines = previous_build(&root, &out, started);
    assert!(!reruns(&lines, started), "nothing changed: the next build must be fresh");
    for rel in [
        "runs/r/manifest.json",
        "logs/compile.log",
        "research/notebooks/n.ipynb",
        "strategies/rust/s/s.rs",
    ] {
        write_marker(&root, rel);
        assert!(!reruns(&lines, started), "a write to {rel} must not re-run the build script");
    }
    write_marker(&root, "research/studies/rhai/r/r.rhai");
    assert!(reruns(&lines, started), "the interpreted tier appearing must re-run it");

    let lines = previous_build(&root, &out, started);
    assert!(!reruns(&lines, started));
    write_marker(&root, "research/studies/rust/q/q.rs");
    assert!(reruns(&lines, started), "the compiled tier appearing must re-run it");

    let lines = previous_build(&root, &out, started);
    assert!(!reruns(&lines, started));
    write_marker(&root, "research/studies/rhai/r/r.rhai");
    assert!(reruns(&lines, started), "a write inside a present tier must re-run it");
}
