//! Unit tests for the PURE generator (`vike_user_research::codegen`) on synthetic trees — the exact
//! code `build.rs` runs (it `include!`s the same file). Temp trees are built with std only (no
//! tempfile dep): a unique dir under the target-local temp root, removed at the end of each test.

use std::fs;
use std::path::PathBuf;

use vike_model::state_path::{RESEARCH_SUBDIR, RHAI_SUBDIR, RUST_SUBDIR, STUDIES_SUBDIR};
use vike_user_research::codegen::{ScannedStudy, render, rhai_tier, rust_tier, scan, valid_name};

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
/// fails, the scan and `vike_model::state_path::user_studies_dir` have come to disagree about where
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
