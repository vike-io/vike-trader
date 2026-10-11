use super::*;

/// A write always lands on the new path, and creates the directory on the way — lazily, only
/// when something is actually written.
#[test]
fn write_lands_on_the_new_path_and_creates_the_directory() {
    let scratch = Scratch::new("write");
    let state = scratch.path().join("deep").join("state");
    assert!(!state.exists(), "precondition: the directory does not exist yet");

    let p = write_path(Some(state.as_path()), "thing.json").expect("creatable");
    assert_eq!(p, state.join("thing.json"));
    assert!(state.is_dir(), "the state directory is created lazily on first write");
}

/// **A state FILE that is a symlink is refused.**
///
/// Everything under `settings/state` is program-written: the caller takes this path and
/// `fs::write`s it, which TRUNCATES whatever the link points at and replaces it with content
/// the program chose. Nothing here has a reason to be redirected file-by-file — these files
/// are re-derivable, their names are constants, and the operator-facing way to relocate them
/// is `VIKE_STATE_ROOT` / `VIKE_SETTINGS_DIR`, which relocates the whole directory.
///
/// The assertion that matters is the last one: the decoy's CONTENT is intact. A refusal that
/// still truncated the target would pass an `is_err()` check.
#[cfg(unix)]
#[test]
fn a_symlinked_state_file_is_refused_and_its_target_is_untouched() {
    let scratch = Scratch::new("write-symlink-file");
    let state = scratch.path().join("state");
    std::fs::create_dir_all(&state).unwrap();
    let decoy = scratch.path().join("important.conf");
    std::fs::write(&decoy, "DO NOT TRUNCATE ME").unwrap();
    std::os::unix::fs::symlink(&decoy, state.join("thing.json")).unwrap();

    let err = write_path(Some(state.as_path()), "thing.json")
        .expect_err("a symlinked state file must be refused, not written through");
    assert!(err.to_string().to_lowercase().contains("symlink"), "the refusal must say why: {err}");
    assert_eq!(
        std::fs::read_to_string(&decoy).unwrap(),
        "DO NOT TRUNCATE ME",
        "the link's target must be untouched"
    );
}

/// **…but a symlinked state DIRECTORY is fine.**
///
/// Pointing `settings/` or `settings/state` at shared or larger storage is a legitimate
/// operator setup — it is the filesystem's spelling of the same intent `VIKE_STATE_ROOT`
/// serves — and the write still lands inside a directory the operator chose, under that
/// directory's own permissions. Refusing it would break real deployments to buy nothing:
/// the danger is the WRITE TARGET, and the target here is a plain file.
#[cfg(unix)]
#[test]
fn a_symlinked_state_directory_is_accepted() {
    let scratch = Scratch::new("write-symlink-dir");
    let real = scratch.path().join("shared-storage");
    std::fs::create_dir_all(&real).unwrap();
    let state = scratch.path().join("state");
    std::os::unix::fs::symlink(&real, &state).unwrap();

    let p = write_path(Some(state.as_path()), "thing.json")
        .expect("a symlinked state DIRECTORY is a legitimate operator setup");
    assert_eq!(p, state.join("thing.json"));
    std::fs::write(&p, "{}").unwrap();
    assert!(real.join("thing.json").is_file(), "the write lands in the linked directory");
}
