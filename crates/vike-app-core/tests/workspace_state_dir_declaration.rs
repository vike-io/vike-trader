//! **The workspace family honours the settings directory the composition ROOT resolved** — one
//! walk decides, and `workspace.json`, `layouts/`, `last_layout.txt` and `backends.json` move
//! together or not at all.
//!
//! # The defect
//!
//! MEASURED 2026-09-15 against the live the CI box daemon. `crates/vike-app-core/src/workspace/persist.rs`'s
//! `state_dir` resolved a FRESH walk up from `std::env::current_dir()`, while credentials resolve
//! through `$VIKE_SETTINGS_DIR`. So launching `vike-desktop` with
//! `VIKE_SETTINGS_DIR=<project>/settings` from a DIFFERENT directory read the credentials out of
//! that project and the backend registry from somewhere else entirely: the client reported *"no
//! --observe and no active backend"* while a perfectly valid `backends.json` sat in the named
//! settings directory. Passing `$VIKE_STATE_ROOT` as well made it work, which is the shape of a
//! defect rather than of a configuration.
//!
//! The root `CLAUDE.md` states the rule it broke — **`vike-boot` owns the startup sequence and ONE
//! walk DECIDES**, because "the `_from`-less resolvers are `$VIKE_SETTINGS_DIR`-BLIND and a second
//! call answers with whatever the working directory sits above". That walk was exactly the second
//! call.
//!
//! # ⚠ Why this is ONE test function rather than five
//!
//! `declare_project_state_dir` writes a process-wide `OnceLock` (the shape
//! `vike_bridge_core::halt::declare_project_state_dir` uses for the kill switch, and for the same
//! reason: the alternative is threading a directory through the whole `path`/`base_dir`/`layouts`/
//! `backends` surface and its GUI call sites). An integration-test FILE is one binary and therefore
//! one process, but its `#[test]`s run as parallel THREADS — so split across functions these
//! assertions would race for the one cell and their order would decide the outcome. Written as one
//! ordered function, the sequence is the test.
//!
//! No environment variable is read or written here: `$VIKE_STATE_ROOT` outranks the declaration by
//! design and the assertions say so where it would show.

use std::path::Path;
use vike_app_core::backend_registry;
use vike_app_core::workspace::persist;

/// A temp directory that is removed when the test ends, without a fixture crate: the test's own
/// name plus the process id, under the system temp root.
struct TempDir(std::path::PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let p = std::env::temp_dir().join(format!("vike-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&p).expect("create the temp state root");
        Self(p)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// ⚠⚠ **THE WHOLE FAMILY FOLLOWS THE ROOT'S DECLARATION, AND IT FOLLOWS IT TOGETHER.**
///
/// The assertions, in the order they have to run:
///
/// 1. A declaration made before anything reads the family is accepted and MOVES it — every member
///    (`workspace.json`, `layouts/`, `last_layout.txt`, `backends.json`) now resolves under the
///    declared directory. Reddens on a `state_dir` that goes back to walking from the working
///    directory, which is the defect itself.
/// 2. `backends.json` is IN that family. It is the member the measured failure was about, it lives
///    one module away (`crate::backend_registry`) and it reaches the base directory through the
///    same `base_dir`, so a fix that moved only `workspace.json` would look identical from the
///    workspace side and leave the registry exactly where it was.
/// 3. A SECOND, DIFFERENT declaration is REFUSED and moves nothing — one process has one workspace
///    directory, and a silent second answer is the failure class this whole change is about.
/// 4. Re-declaring the SAME directory is not a fault and reports none.
#[test]
fn the_declared_state_dir_moves_the_whole_workspace_family_and_only_once() {
    let tmp = TempDir::new("workspace-decl");
    let declared = tmp.path().to_path_buf();

    // 1 — the declaration is accepted (nothing in this binary has read the family yet).
    //
    // ⚠ The relocation NOTICE is deliberately not pinned to a value here: it depends on whether the
    // checkout this test runs in happens to hold a `settings/state/workspace.json` of its own, and
    // a test whose expected value is a property of the developer's box is a flake. The notice's own
    // rules are unit-tested in `crates/vike-app-core/src/workspace/persist.rs` against planted
    // directories. What IS asserted here is that when it speaks, it names where the family went.
    let notice = persist::declare_project_state_dir(Some(declared.clone()))
        .expect("the first declaration, made before any read, is accepted");
    if let Some(ref moved) = notice {
        assert!(
            moved.contains(&declared.display().to_string()),
            "a relocation notice names the directory the family moved TO: {moved}"
        );
    }

    // …and every member now resolves under it. `$VIKE_STATE_ROOT` would outrank the declaration by
    // design, so a failure here on a box that has it set is the rung working, not a defect.
    let base = persist::layouts_dir().expect("a declared state dir yields a layouts dir");
    assert_eq!(base, declared.join("layouts"), "layouts/ follows the declaration");
    assert_eq!(
        persist::path().expect("a declared state dir yields a workspace path"),
        declared.join("workspace.json"),
        "workspace.json follows the declaration (or $VIKE_STATE_ROOT is set on this box)"
    );

    // 2 — and so does the REGISTRY, which is the file the measured failure was about.
    assert_eq!(
        backend_registry::path().expect("a declared state dir yields a registry path"),
        declared.join("backends.json"),
        "backends.json is a member of the same family and moves with it"
    );

    // 3 — a second, DIFFERENT declaration is refused, by name, and changes nothing.
    let other = declared.join("elsewhere");
    let err = persist::declare_project_state_dir(Some(other.clone()))
        .expect_err("a second, different declaration is refused");
    assert!(
        err.contains("already declared"),
        "the refusal says what happened rather than failing silently: {err}"
    );
    assert_eq!(
        persist::path().expect("still resolvable"),
        declared.join("workspace.json"),
        "…and the refused declaration moved nothing"
    );

    // 4 — repeating the declaration in force is not a fault, and reports no relocation because
    // nothing moved.
    assert_eq!(
        persist::declare_project_state_dir(Some(declared.clone()))
            .expect("re-declaring the directory already in force is not an error"),
        None,
        "…and nothing moved, so there is nothing to say"
    );

    // A declaration of `None` (a root that booted and found no project) is a DIFFERENT answer from
    // the one in force, so it is refused like any other second answer rather than quietly blanking
    // the family.
    assert!(
        persist::declare_project_state_dir(None).is_err(),
        "`None` is a real answer and a different one — it cannot overwrite a declared directory"
    );
}
