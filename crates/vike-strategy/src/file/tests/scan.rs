//! The folder scan: a broken file beside good ones, the build states, the path refusals.

use std::fs;
use std::path::Path;

use vike_paths::scratch::ScratchDir;

use crate::file::scan::{
    StrategyFileBuildState, StrategyFileLanguage, build_state, resolve, scan, sha256_hex,
};

const GOOD: &str =
    "//! ```toml\n//! [strategy]\n//! runs_on = \"live\"\n//! ```\npub fn build() {}\n";

fn write(root: &Path, relative: &str, bytes: &[u8]) {
    let path = root.join(relative);
    fs::create_dir_all(path.parent().expect("a parent")).expect("mkdir");
    fs::write(path, bytes).expect("write");
}

fn sha256_of(bytes: &[u8]) -> String {
    sha256_hex(bytes)
}

/// The spelling is a contract (a run records it, the builder keys its cache on it): lowercase hex,
/// 64 characters, pinned against the published FIPS 180-2 vectors.
#[test]
fn the_sha256_is_lowercase_hex_of_the_published_vectors() {
    assert_eq!(sha256_hex(b""), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
    assert_eq!(
        sha256_hex(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

#[test]
fn a_broken_file_is_listed_beside_the_good_ones_and_hides_none() {
    let scratch =
        ScratchDir::create_in(&std::env::temp_dir(), "strategy-file-scan").expect("scratch");
    let root = scratch.path().join("strategies");
    let builds = scratch.path().join("builds");
    fs::create_dir_all(&builds).expect("mkdir builds");
    write(&root, "trend/momentum.rs", GOOD.as_bytes());
    write(&root, "broken.rhai", b"fn on_bar(bar) {}\n");
    write(&root, "other/deeper/LOUD.RS", GOOD.as_bytes());
    write(&root, "other/not_text.rs", &[0xff, 0xfe, 0x00]);
    write(&root, "trend/NOTICE", b"licence text, not a strategy\n");
    // Same bytes, one artifact: `LOUD.RS` and `momentum.rs` share it.
    fs::write(builds.join(format!("{}.so", sha256_of(GOOD.as_bytes()))), b"").expect("artifact");
    fs::write(builds.join(format!("{}.err", sha256_of(&[0xff, 0xfe, 0x00]))), "error[E0425]: x\n")
        .expect("failure");

    let listed = scan(&root, Some(&builds), None);
    assert!(listed.unreadable.is_empty(), "{:?}", listed.unreadable);
    let paths: Vec<&str> = listed.files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(
        paths,
        ["broken.rhai", "other/deeper/LOUD.RS", "other/not_text.rs", "trend/momentum.rs"]
    );

    let [broken, loud, not_text, momentum] = &listed.files[..] else { unreachable!() };
    assert_eq!(broken.language, StrategyFileLanguage::Rhai);
    assert!(broken.header.as_ref().expect_err("no header").starts_with("no header"));
    assert_eq!(broken.build, None, "a Rhai file has no build state");
    assert_eq!(broken.sha256.as_deref(), Some(sha256_of(b"fn on_bar(bar) {}\n").as_str()));

    for good in [loud, momentum] {
        assert_eq!(good.language, StrategyFileLanguage::Rust);
        assert!(good.header.is_ok(), "{:?}", good.header);
        assert_eq!(
            good.build,
            Some(StrategyFileBuildState::Built { sha256: sha256_of(GOOD.as_bytes()) })
        );
    }
    assert_eq!(not_text.header.as_ref().expect_err("not text"), "the file is not UTF-8 text");
    assert_eq!(
        not_text.build,
        Some(StrategyFileBuildState::BuildFailed { message: "error[E0425]: x".to_string() })
    );

    // No sandbox here: every Rust file says so, whatever the build folder holds.
    let here = scan(&root, Some(&builds), Some("Linux 6.7 or later is needed"));
    for f in here.files.iter().filter(|f| f.language == StrategyFileLanguage::Rust) {
        assert_eq!(
            f.build.as_ref().map(ToString::to_string).as_deref(),
            Some("cannot run here: Linux 6.7 or later is needed")
        );
    }
    // No build folder known: not built.
    let unknown = scan(&root, None, None);
    assert_eq!(unknown.files[3].build, Some(StrategyFileBuildState::NotBuilt));
}

#[test]
fn a_missing_folder_is_an_empty_list() {
    let scratch =
        ScratchDir::create_in(&std::env::temp_dir(), "strategy-file-none").expect("scratch");
    let listed = scan(&scratch.path().join("strategies"), None, None);
    assert!(listed.files.is_empty() && listed.unreadable.is_empty(), "{listed:?}");
}

#[test]
fn a_build_state_says_what_the_list_shows() {
    let shown = |s: StrategyFileBuildState| s.to_string();
    assert_eq!(shown(StrategyFileBuildState::NotBuilt), "not built");
    assert_eq!(
        shown(StrategyFileBuildState::Built { sha256: "0123456789abcdef".to_string() }),
        "built 01234567"
    );
    assert_eq!(
        shown(StrategyFileBuildState::BuildFailed { message: "error[E0425]".to_string() }),
        "build failed: error[E0425]"
    );
    assert_eq!(
        shown(StrategyFileBuildState::CannotRunHere { why: "no sandbox".to_string() }),
        "cannot run here: no sandbox"
    );
    assert_eq!(build_state("ab", None, None), StrategyFileBuildState::NotBuilt);
}

#[test]
fn a_path_outside_the_folder_is_refused() {
    let root = Path::new("strategies");
    for bad in ["", "/etc/vike/x.rs", "../x.rs", "trend/../../x.rs", "trend/momentum.toml", "trend"]
    {
        assert!(resolve(root, bad).is_err(), "{bad:?} must be refused");
    }
    assert_eq!(resolve(root, "trend/momentum.rs"), Ok(root.join("trend/momentum.rs")));
    assert_eq!(resolve(root, "./grid.RHAI"), Ok(root.join("./grid.RHAI")));
}
