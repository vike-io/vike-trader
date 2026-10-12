//! Tests for `vike_model::host_build` — the build-script mechanism the two compiled user-code
//! hosts share — on synthetic trees, host-independently: the tier scan and its error texts, the
//! shared render pieces here; the driver's root rule and its OUT_DIR effects in the
//! `host_build/driver_run.rs` child; the rerun watch, held against a MODEL of cargo's rule, in the
//! `host_build/watch_model.rs` child.
//!
//! What stays with each host: its read set (`scanned_dirs`), its per-folder extras and its
//! generated resolver, tested in that host's `tests/gen_unit.rs`.
//!
//! Every tree lives in a `vike_model::scratch::ScratchDir` under the system temp directory,
//! removed at the end of each test.

use std::fs;
use std::path::{Path, PathBuf};

use vike_model::host_build::render::{header, module_lines, quoted_list};
use vike_model::host_build::scan::{
    EntryPolicy, Found, folder_names, holds_any_rust, scan_tier, valid_name,
};
use vike_model::scratch::ScratchDir;
use vike_model::test_support::mtime::write_marker;

#[path = "host_build/driver_run.rs"]
mod driver_run;
#[path = "host_build/watch_model.rs"]
mod watch_model;

/// A self-deleting directory under the system temp directory, unique per call (the
/// [`ScratchDir`] appends `-<pid>-<seq>` to the tag).
fn scratch(tag: &str) -> ScratchDir {
    let dir = ScratchDir::create_in(&std::env::temp_dir(), &format!("vike-model-host-build-{tag}"));
    dir.unwrap()
}

/// A folder `<tier>/<name>/` holding `files` (each the marker bytes); no files = an empty folder.
fn folder(tier: &Path, name: &str, files: &[&str]) {
    fs::create_dir_all(tier.join(name)).unwrap();
    for f in files {
        write_marker(tier, &format!("{name}/{f}"));
    }
}

fn names(found: &[Found]) -> Vec<&str> {
    found.iter().map(|f| f.name.as_str()).collect()
}

const BOTH: [EntryPolicy; 2] = [EntryPolicy::SkipEntryless, EntryPolicy::ErrorIfRustWithoutEntry];

// ── scan ──────────────────────────────────────────────────────────────────────────────────────────

#[test]
fn valid_name_charset() {
    for ok in ["abs_v2", "cohort_v2", "a", "z9_"] {
        assert!(valid_name(ok), "{ok}");
    }
    for bad in ["Abs", "Cohort", "2fast", "dash-ed", "_lead", "", "spa ce"] {
        assert!(!valid_name(bad), "{bad}");
    }
}

#[test]
fn an_absent_tier_scans_empty_without_errors() {
    for policy in BOTH {
        let out = scan_tier(Path::new("Z:/definitely/absent/user_data/tier"), "thing", policy);
        assert!(out.found.is_empty() && out.errors.is_empty(), "{policy:?}: {out:?}");
    }
}

/// A MISSING tier and an EMPTY one scan the same and render the same module lines. This is what
/// makes it safe for a build script to stop re-running the scan while the tier is absent: an
/// absent tier can produce nothing an empty one does not. (Adapted from the strategy host's
/// `a_missing_root_and_an_empty_root_generate_the_same_registry`; each host keeps its own copy
/// over its full render.)
#[test]
fn a_missing_tier_and_an_empty_tier_scan_the_same() {
    let dir = scratch("missing-vs-empty");
    let (missing, empty) = (dir.join("absent"), dir.join("empty"));
    fs::create_dir_all(&empty).unwrap();
    for policy in BOTH {
        let (a, b) = (scan_tier(&missing, "thing", policy), scan_tier(&empty, "thing", policy));
        assert!(a.errors.is_empty() && b.errors.is_empty(), "{:?} / {:?}", a.errors, b.errors);
        assert_eq!(a.found, b.found);
        assert_eq!(module_lines(&a.found), module_lines(&b.found));
    }
}

/// The scan reads its tier and NOTHING else: a tree crowded with everything else a user_data holds
/// scans the same as the tier alone. (Adapted from the strategy host's
/// `files_outside_the_scanned_dirs_do_not_change_the_registry`; the host keeps the half that pins
/// its own `scanned_dirs`.)
#[test]
fn scan_tier_reads_nothing_outside_its_tier() {
    let lean = scratch("read-set-lean");
    folder(&lean.join("strategies/rust"), "alpha", &["alpha.rs", "strategy.toml"]);
    let crowded = scratch("read-set-crowded");
    folder(&crowded.join("strategies/rust"), "alpha", &["alpha.rs", "strategy.toml"]);
    for rel in [
        "runs/1790000000-abc-0/manifest.json",
        "logs/compile.log",
        "profiles/backtest.toml",
        "strategies/rhai/sma/sma.rhai",
        "strategies/stray/stray.rs",
    ] {
        write_marker(&crowded, rel);
    }
    for policy in BOTH {
        let a = scan_tier(&lean.join("strategies/rust"), "strategy", policy);
        let b = scan_tier(&crowded.join("strategies/rust"), "strategy", policy);
        assert_eq!(a.errors, b.errors);
        assert_eq!(names(&a.found), names(&b.found));
    }
}

#[test]
fn found_folders_come_back_sorted_with_their_dir_and_entry() {
    let dir = scratch("sorted");
    let tier = dir.join("tier");
    folder(&tier, "beta", &["beta.rs", "baseline.toml"]);
    folder(&tier, "alpha", &["alpha.rs"]);
    write_marker(&tier, "loose_file.rs"); // a FILE in the tier is not a folder: ignored
    for policy in BOTH {
        let out = scan_tier(&tier, "thing", policy);
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert_eq!(names(&out.found), ["alpha", "beta"], "sorted regardless of creation order");
        assert_eq!(
            out.found[0],
            Found {
                name: "alpha".into(),
                dir: tier.join("alpha"),
                entry: tier.join("alpha").join("alpha.rs"),
            }
        );
    }
}

/// The strategy tier's rule: a folder without `<name>.rs` is a presets-only folder for a built-in
/// — skipped silently, even when it holds Rust under another name, and even when its name is one
/// `valid_name` refuses (the entry check comes first).
#[test]
fn skip_entryless_skips_every_folder_without_its_entry_file() {
    let dir = scratch("skip-entryless");
    let tier = dir.join("tier");
    folder(&tier, "alpha", &["alpha.rs"]);
    folder(&tier, "built_in_presets", &["fast.toml"]);
    folder(&tier, "misnamed", &["strategy.rs"]);
    folder(&tier, "Bad-Presets", &["fast.toml"]);
    folder(&tier, "empty", &[]);
    let out = scan_tier(&tier, "strategy", EntryPolicy::SkipEntryless);
    assert!(out.errors.is_empty(), "{:?}", out.errors);
    assert_eq!(names(&out.found), ["alpha"]);
}

/// The research tier's rule: Rust present under the wrong file name is an ERROR naming the folder
/// and the file it wanted; a folder with no `.rs` at all is a recipes-only folder, skipped
/// silently.
#[test]
fn error_if_rust_without_entry_refuses_misnamed_rust_and_skips_recipes_only() {
    let dir = scratch("rust-without-entry");
    let tier = dir.join("tier");
    folder(&tier, "alpha", &["alpha.rs"]);
    folder(&tier, "mystudy", &["study.rs", "baseline.toml"]);
    folder(&tier, "recipes_only", &["baseline.toml"]);
    folder(&tier, "empty", &[]);
    let out = scan_tier(&tier, "study", EntryPolicy::ErrorIfRustWithoutEntry);
    assert_eq!(names(&out.found), ["alpha"]);
    assert_eq!(
        out.errors,
        [format!(
            "{}: holds Rust source but no `mystudy.rs` — the entry file's stem must equal the \
             folder name (the rhai tier's rule), or nothing in this folder is compiled",
            tier.join("mystudy").display()
        )]
    );
}

/// The bad-name error, word for word, with each host's noun — the text both hosts printed before
/// the move.
#[test]
fn a_bad_folder_name_is_an_error_naming_the_path_and_the_noun() {
    let dir = scratch("bad-name");
    let tier = dir.join("tier");
    folder(&tier, "Bad-Name", &["Bad-Name.rs"]);
    folder(&tier, "okay", &["okay.rs"]);
    for (noun, policy) in
        [("strategy", EntryPolicy::SkipEntryless), ("study", EntryPolicy::ErrorIfRustWithoutEntry)]
    {
        let out = scan_tier(&tier, noun, policy);
        assert_eq!(names(&out.found), ["okay"], "the bad folder is dropped, the rest kept");
        assert_eq!(
            out.errors,
            [format!(
                "{}: {noun} folder name must match [a-z][a-z0-9_]* (it becomes the registry name \
                 and the generated module name)",
                tier.join("Bad-Name").display()
            )]
        );
    }
}

/// A folder name that is not UTF-8 is an error naming the path, never a skip. Linux only: it is
/// the platform whose filesystems accept such a name, and every lane is Linux.
#[cfg(target_os = "linux")]
#[test]
fn a_non_utf8_folder_name_is_an_error() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    let dir = scratch("non-utf8");
    let tier = dir.join("tier");
    let bad = tier.join(OsStr::from_bytes(b"bad\xffname"));
    fs::create_dir_all(&bad).unwrap();
    for policy in BOTH {
        let out = scan_tier(&tier, "thing", policy);
        assert!(out.found.is_empty());
        assert_eq!(out.errors, [format!("{}: folder name is not valid UTF-8", bad.display())]);
    }
}

#[test]
fn holds_any_rust_looks_for_a_rust_file_directly_inside() {
    let dir = scratch("holds-rust");
    folder(&dir, "with_rust", &["x.rs", "a.toml"]);
    folder(&dir, "without", &["a.toml", "notes.rs.txt"]);
    folder(&dir, "nested", &["deeper/x.rs"]);
    assert!(holds_any_rust(&dir.join("with_rust")));
    assert!(!holds_any_rust(&dir.join("without")));
    assert!(!holds_any_rust(&dir.join("nested")), "only the folder's own files count");
    assert!(!holds_any_rust(&dir.join("absent")));
}

#[test]
fn folder_names_lists_sub_directories_only_and_is_empty_when_absent() {
    let dir = scratch("folder-names");
    folder(&dir, "twin", &["twin.rhai"]);
    folder(&dir, "other", &[]);
    write_marker(&dir, "loose.rhai");
    let got: Vec<String> = folder_names(&dir).into_iter().collect();
    assert_eq!(got, ["other", "twin"]);
    assert!(folder_names(&dir.join("absent")).is_empty());
}

// ── render ────────────────────────────────────────────────────────────────────────────────────────

#[test]
fn header_names_the_host_build_script() {
    assert_eq!(
        header("vike-user-strategies"),
        "// @generated by vike-user-strategies/build.rs — NEVER committed, lives in OUT_DIR.\n"
    );
}

#[test]
fn module_lines_render_forward_slash_paths_in_scan_order() {
    assert_eq!(module_lines(&[]), "");
    let found = [
        Found {
            name: "abs".into(),
            dir: PathBuf::from(r"C:\some\user_data\strategies\rust\abs"),
            entry: PathBuf::from(r"C:\some\user_data\strategies\rust\abs\abs.rs"),
        },
        Found {
            name: "cohort".into(),
            dir: PathBuf::from("/p/user_data/research/studies/rust/cohort"),
            entry: PathBuf::from("/p/user_data/research/studies/rust/cohort/cohort.rs"),
        },
    ];
    assert_eq!(
        module_lines(&found),
        "#[path = \"C:/some/user_data/strategies/rust/abs/abs.rs\"]\npub mod user_abs;\n\
         #[path = \"/p/user_data/research/studies/rust/cohort/cohort.rs\"]\npub mod user_cohort;\n"
    );
}

#[test]
fn quoted_list_quotes_and_comma_separates() {
    assert_eq!(quoted_list(&[]), "");
    assert_eq!(quoted_list(&["abs"]), r#""abs""#);
    assert_eq!(quoted_list(&["abs", "cohort"]), r#""abs", "cohort""#);
}
