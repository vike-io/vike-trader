use std::path::Path;

use super::*;

/// One folder per build contract: the same inputs name the same eight hex digits, and a different
/// profile or source stamp names another.
#[test]
fn the_build_folder_is_stable_and_moves_with_the_profile_and_the_stamp() {
    let folder = build_folder("release", "abc1234");
    assert_eq!(folder.len(), 8, "{folder}");
    assert!(folder.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()), "{folder}");
    assert_eq!(folder, build_folder("release", "abc1234"));
    assert_ne!(folder, build_folder("debug", "abc1234"));
    assert_ne!(folder, build_folder("release", "abc1235"));
    // The terminator keeps two inputs from running together.
    assert_ne!(build_folder("release", "x"), build_folder("releasex", ""));
}

#[test]
fn the_artifact_path_is_the_folder_then_the_sha() {
    let sha = "ab".repeat(32);
    assert_eq!(
        artifact_path(Path::new("/b"), "0123abcd", &sha),
        Path::new("/b").join("0123abcd").join(format!("{sha}.so"))
    );
}
