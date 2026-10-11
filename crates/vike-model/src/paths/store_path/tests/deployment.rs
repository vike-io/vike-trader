use super::*;

// ---- the OPERATIONAL cases: the real walk feeding the real precedence -----------------------

/// **THE deployment case, end to end.** A project root holds a binary and a `settings/`
/// directory and no source tree — so the hinge cannot fire — and the tape must land in that
/// same folder,
/// not under `$HOME`. The walk and the precedence are exercised together here precisely because
/// each is separately correct in the shipped tree and the pairing is what was wrong.
///
/// ⚠ The `settings/` directory is what makes this resolve at all: it is the deployment MARKER
/// (`crates/vike-model/src/paths/state_path.rs`'s `nearest_project_marker`), which is why the install
/// recipe creates it even when empty.
#[test]
fn a_deployment_with_a_settings_marker_stores_inside_the_project() {
    let scratch = Scratch::new("deployment");
    let opt_vike = scratch.path().join("opt").join("vike");
    std::fs::create_dir_all(opt_vike.join("settings")).unwrap();

    let project = crate::paths::state_path::project_hist_store_dir(&opt_vike);
    assert_eq!(
        project.as_deref(),
        Some(opt_vike.join("market_data").join("hist").as_path()),
        "the walk must find the deployment's own data dir"
    );

    let got = resolve_store_root(
        None,
        None,
        Some(Path::new(NO_CHECKOUT)),
        // No override — this deployment is DISCOVERED by its `settings/` marker, which is the
        // rung this test has always been about.
        project_default_from(None, project),
        Some(PathBuf::from("/home/u/.local/share/vike-data")),
    );
    assert_eq!(
        got.root,
        opt_vike.join("market_data").join("hist"),
        "a deployment's tape belongs in the deployment's own folder, not under $HOME"
    );
    assert_eq!(got.rung, StoreRootRung::Project);
}

/// …and WITHOUT that marker nothing changes from before this rung existed: no project, no
/// project rung, and the per-user directory still answers. A tree with no marker anywhere is
/// the one shape where inventing a project path would be a guess.
#[test]
fn a_deployment_without_a_marker_falls_through_exactly_as_before() {
    let scratch = Scratch::new("unmarked");
    let bare = scratch.path().join("opt").join("vike");
    std::fs::create_dir_all(&bare).unwrap();

    // ⚠ The system temp dir is an ancestor, and a stray `Cargo.toml` or `settings/` up there
    // would make this test assert nothing. Skip rather than pass vacuously.
    if crate::paths::state_path::project_hist_store_dir(&bare).is_some() {
        eprintln!("skipped: a project marker exists above {}", bare.display());
        return;
    }

    let got = resolve_store_root(
        None,
        None,
        Some(Path::new(NO_CHECKOUT)),
        project_default_from(None, crate::paths::state_path::project_hist_store_dir(&bare)),
        Some(PathBuf::from("/home/u/.local/share/vike-data")),
    );
    assert_eq!(got.root, PathBuf::from("/home/u/.local/share/vike-data"));
    assert_eq!(got.rung, StoreRootRung::UserDir);
}
