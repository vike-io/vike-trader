use std::path::Path;

use super::*;

const SOURCES: &str = "0123456789abcdef";

/// One folder per build contract: the same inputs name the same eight hex digits, and a different
/// profile, SDK source hash or recipe names another.
#[test]
fn the_build_folder_is_stable_and_moves_with_the_profile_the_sources_and_the_recipe() {
    let folder = build_folder("release", SOURCES, "recipe");
    assert_eq!(folder.len(), 8, "{folder}");
    assert!(folder.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()), "{folder}");
    assert_eq!(folder, build_folder("release", SOURCES, "recipe"));
    assert_ne!(folder, build_folder("debug", SOURCES, "recipe"));
    assert_ne!(folder, build_folder("release", "fedcba9876543210", "recipe"), "other sources");
    assert_ne!(folder, build_folder("release", SOURCES, "another recipe"), "another recipe");
    let (abi, fp, boundary) =
        (crate::abi::ABI_VERSION, crate::fingerprint::FINGERPRINT, crate::sdk::BOUNDARY_HASH);
    assert_eq!(folder, folder_name(abi, fp, boundary, "release", SOURCES, "recipe"));
    // The terminator keeps two inputs from running together.
    assert_ne!(
        folder_name(1, fp, boundary, "release", "x", ""),
        folder_name(1, fp, boundary, "releasex", "", "")
    );
}

/// The last line of defence stays in the folder's name: an artifact built under another interface
/// version, another toolchain or another boundary is never looked for in this binary's folder,
/// whatever the sources.
#[test]
fn an_abi_toolchain_or_boundary_change_moves_the_build_folder() {
    let (abi, fp, boundary) =
        (crate::abi::ABI_VERSION, crate::fingerprint::FINGERPRINT, crate::sdk::BOUNDARY_HASH);
    let here = folder_name(abi, fp, boundary, "release", SOURCES, "");
    assert_ne!(here, folder_name(abi + 1, fp, boundary, "release", SOURCES, ""), "the ABI version");
    let other_rustc = fp.replacen("rustc=", "rustc=0.0.0+other;", 1);
    assert_ne!(
        here,
        folder_name(abi, &other_rustc, boundary, "release", SOURCES, ""),
        "the fingerprint"
    );
    assert_ne!(
        here,
        folder_name(abi, fp, "0000000000000000", "release", SOURCES, ""),
        "the boundary"
    );
}

/// The baked hashes are sixteen lowercase hex digits (`build/source_hash.rs`'s shape), never empty.
#[test]
fn the_source_and_boundary_hashes_are_baked() {
    for hash in [SOURCE_HASH, crate::sdk::BOUNDARY_HASH, recipe_hash(&["a", "b"]).as_str()] {
        assert_eq!(hash.len(), 16, "{hash}");
        assert!(hash.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()), "{hash}");
    }
    assert_ne!(recipe_hash(&["ab", ""]), recipe_hash(&["a", "b"]));
}

/// `<builds>/sdk-N/<folder>/<sha>.so`, and the work directories beside the folders.
#[test]
fn the_artifact_path_is_the_sdk_then_the_folder_then_the_sha() {
    let sha = "ab".repeat(32);
    let builds = Path::new("/b");
    assert_eq!(
        artifact_path(builds, Sdk::Number(3), "0123abcd", &sha),
        builds.join("sdk-3").join("0123abcd").join(format!("{sha}.so"))
    );
    assert_eq!(
        artifact_path(builds, Sdk::Dev, "0123abcd", &sha),
        builds.join("sdk-dev").join("0123abcd").join(format!("{sha}.so"))
    );
    let work = |kind| work_directory(builds, Sdk::Number(12), "0123abcd", kind);
    assert_eq!(work(TemplateKind::Strategy), builds.join("sdk-12").join("work-0123abcd"));
    assert_eq!(work(TemplateKind::StrategyOnnx), builds.join("sdk-12").join("work-0123abcd-onnx"));
    assert_eq!(work(TemplateKind::Study), builds.join("sdk-12").join("work-0123abcd-study"));
}
