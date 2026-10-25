use super::*;

// ---- the settings-directory walk: the two markers ----------------------------------------

/// **The DEV shape, unchanged.** A source checkout resolves to the OUTERMOST `Cargo.toml`'s
/// `settings/` from any depth — the #1089 regression guard, restated here because
/// `state_path.rs` never had one of its own (only `vike-secrets`' copy did) and the two-marker
/// change is exactly the kind that could quietly reintroduce the nearest-crate answer.
#[test]
fn a_source_checkout_resolves_to_the_outermost_cargo_toml() {
    let scratch = Scratch::new("dev-shape");
    let root = scratch.path();
    let crate_dir = root.join("crates").join("bridges").join("aster");
    std::fs::create_dir_all(crate_dir.join("src")).unwrap();
    std::fs::write(root.join("Cargo.toml"), "[workspace]\n").unwrap();
    std::fs::write(crate_dir.join("Cargo.toml"), "[package]\nname=\"a\"\n").unwrap();

    let want = root.join(PROJECT_SETTINGS_DIR);
    for from in [root, &crate_dir, &crate_dir.join("src")] {
        assert_eq!(project_settings_dir(from).as_deref(), Some(want.as_path()));
    }
    assert_eq!(project_state_dir(&crate_dir).as_deref(), Some(want.join(STATE_SUBDIR).as_path()));
}

/// …and a `settings/` directory sitting at a CRATE level does not capture it either. This is
/// the shape the two-marker walk could have broken and the reason `Cargo.toml` outranks the
/// deployment marker instead of competing with it level by level.
#[test]
fn a_settings_dir_inside_a_checkout_never_beats_the_workspace_root() {
    let scratch = Scratch::new("dev-inner-settings");
    let root = scratch.path();
    let crate_dir = root.join("crates").join("bridges").join("aster");
    std::fs::create_dir_all(crate_dir.join("src")).unwrap();
    std::fs::write(root.join("Cargo.toml"), "[workspace]\n").unwrap();
    std::fs::write(crate_dir.join("Cargo.toml"), "[package]\nname=\"a\"\n").unwrap();
    // A real `settings/` directory at the crate level — the nearest deployment marker there is.
    std::fs::create_dir_all(crate_dir.join(PROJECT_SETTINGS_DIR)).unwrap();

    assert_eq!(
        project_settings_dir(&crate_dir.join("src")).as_deref(),
        Some(root.join(PROJECT_SETTINGS_DIR).as_path()),
        "the workspace root must still win: a stray settings/ must not capture the project"
    );

    // …including a `settings/` at a level with NO manifest of its own — deployment-SHAPED, but
    // inside a checkout. **This is the one accepted residual, pinned rather than left to
    // drift**: it is byte-identical to the #1089 tree (`crates/bridges/aster/settings/` is also
    // a settings/ below the workspace root with no [workspace] table at that level), so no
    // marker rule can serve both, and a declared workspace root must keep winning. An operator
    // who genuinely deploys inside a checkout names the directory with `VIKE_SETTINGS_DIR`.
    let inside = root.join("deploy").join("vike");
    std::fs::create_dir_all(inside.join(PROJECT_SETTINGS_DIR)).unwrap();
    assert_eq!(
        project_settings_dir(&inside).as_deref(),
        Some(root.join(PROJECT_SETTINGS_DIR).as_path()),
        "a declared [workspace] root decides alone — relaxing that is #1089"
    );
}

/// ⚠ **THE HIJACK.** One unrelated `Cargo.toml` a single level above a project used to capture
/// it, because the walk kept the OUTERMOST manifest unconditionally — a `cargo new` at the
/// wrong level, a parent monorepo or a vendored crate directory was enough. With no outer
/// `settings/` the project's own populated store simply vanished; with one, the project
/// silently read the OTHER tree's credentials.
///
/// Reproduced end-to-end on the CI box before the fix, in exactly this shape: `vike-cli secrets
/// list` printed `no store found — every venue stays paper` without the outer `settings/`, and
/// listed the OUTER key and never the inner one with it.
#[test]
fn an_unrelated_outer_manifest_cannot_capture_the_project() {
    let scratch = Scratch::new("hijack");
    let outer = scratch.path();
    // The stray: a plain PACKAGE manifest, plus a settings/ to be stolen from.
    std::fs::write(outer.join("Cargo.toml"), "[package]\nname=\"unrelated\"\n").unwrap();
    std::fs::create_dir_all(outer.join(PROJECT_SETTINGS_DIR)).unwrap();
    // The project: a real cargo WORKSPACE with its own settings/.
    let proj = outer.join("proj");
    std::fs::create_dir_all(proj.join("src")).unwrap();
    std::fs::write(proj.join("Cargo.toml"), "[workspace]\nmembers = []\n").unwrap();
    std::fs::create_dir_all(proj.join(PROJECT_SETTINGS_DIR)).unwrap();

    let want = proj.join(PROJECT_SETTINGS_DIR);
    for from in [&proj, &proj.join("src")] {
        assert_eq!(
            project_settings_dir(from).as_deref(),
            Some(want.as_path()),
            "a stranger's manifest above the project must not capture its settings"
        );
    }
    assert_eq!(project_state_dir(&proj).as_deref(), Some(want.join(STATE_SUBDIR).as_path()));
}

/// The same escape with **no `[workspace]` table anywhere** — a single-crate project under a
/// stray manifest. Nothing on the chain claims to be a workspace root, so the NEAREST manifest
/// is the project and the outermost is just the nearest stranger.
///
/// ⚠ This arm cannot reopen #1089: that bug needs `cargo test -p <crate>`, which needs a
/// workspace, which needs a `[workspace]` table — and a chain that has one never reaches here.
#[test]
fn a_plain_package_project_under_a_stray_manifest_keeps_its_own_settings() {
    let scratch = Scratch::new("hijack-plain");
    let outer = scratch.path();
    std::fs::write(outer.join("Cargo.toml"), "[package]\nname=\"unrelated\"\n").unwrap();
    let proj = outer.join("proj");
    std::fs::create_dir_all(proj.join("src")).unwrap();
    std::fs::write(proj.join("Cargo.toml"), "[package]\nname=\"proj\"\n").unwrap();

    assert_eq!(
        project_settings_dir(&proj.join("src")).as_deref(),
        Some(proj.join(PROJECT_SETTINGS_DIR).as_path()),
    );
}

/// A COMMENTED-OUT `[workspace]` is not a workspace table — the marker is a table HEADER, and
/// `# [workspace]` is prose. Without this the stray above would capture the project again.
#[test]
fn a_commented_out_workspace_table_is_not_a_workspace_root() {
    let scratch = Scratch::new("hijack-comment");
    let outer = scratch.path();
    std::fs::write(outer.join("Cargo.toml"), "# [workspace]\n[package]\nname=\"x\"\n").unwrap();
    let proj = outer.join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    std::fs::write(proj.join("Cargo.toml"), "[workspace]\n").unwrap();

    assert_eq!(
        project_settings_dir(&proj).as_deref(),
        Some(proj.join(PROJECT_SETTINGS_DIR).as_path()),
    );
}

/// A NESTED workspace resolves to the OUTERMOST one, not the nearest — this repo HAS one
/// (`crates/bridges/ctrader/protogen` carries its own `[workspace]` table so the drift-gate
/// codegen stays out of the build), and a tool run from inside it must still find the project's
/// settings. This is the guard on choosing "outermost `[workspace]`" over "nearest".
#[test]
fn a_nested_workspace_still_resolves_to_the_outermost_one() {
    let scratch = Scratch::new("nested-ws");
    let root = scratch.path();
    std::fs::write(root.join("Cargo.toml"), "[workspace]\nmembers = []\n").unwrap();
    let inner = root.join("crates").join("ctrader").join("protogen");
    std::fs::create_dir_all(&inner).unwrap();
    std::fs::write(inner.join("Cargo.toml"), "[package]\nname=\"p\"\n[workspace]\n").unwrap();

    assert_eq!(
        project_settings_dir(&inner).as_deref(),
        Some(root.join(PROJECT_SETTINGS_DIR).as_path()),
        "a nested workspace must not become the project"
    );
}

/// An UNREADABLE manifest is evidence of NOTHING, so the walk degrades to the rule that
/// shipped — the outermost manifest — rather than to a new answer. Guessing the other way
/// would mean an unreadable ROOT manifest resolves to the nearest crate, which is #1089
/// (credentials silently vanish, every venue on paper) — the worse of the two failures.
///
/// Invalid UTF-8 is the portable stand-in for "cannot be read"; a mode-000 file is not.
#[test]
fn an_unreadable_manifest_degrades_to_the_shipped_outermost_rule() {
    let scratch = Scratch::new("unreadable");
    let outer = scratch.path();
    std::fs::write(outer.join("Cargo.toml"), [0xff_u8, 0xfe, 0x00, 0x80]).unwrap();
    let proj = outer.join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    std::fs::write(proj.join("Cargo.toml"), "[package]\nname=\"proj\"\n").unwrap();

    assert_eq!(
        project_settings_dir(&proj).as_deref(),
        Some(outer.join(PROJECT_SETTINGS_DIR).as_path()),
    );
}

/// **The DEPLOYMENT shape — the bug this walk was extended for.** A project root holds a binary,
/// a profile and `settings/`, and NO `Cargo.toml` anywhere above it. Before the second marker
/// this returned `None` and a production daemon ran with no settings and no credentials.
#[test]
fn a_deployment_with_no_cargo_toml_resolves_through_its_settings_dir() {
    let scratch = Scratch::new("deploy-shape");
    let opt_vike = scratch.path().join("opt").join("vike");
    std::fs::create_dir_all(opt_vike.join("bin")).unwrap();
    std::fs::create_dir_all(opt_vike.join(PROJECT_SETTINGS_DIR)).unwrap();

    let want = opt_vike.join(PROJECT_SETTINGS_DIR);
    // From the unit's WorkingDirectory…
    assert_eq!(project_settings_dir(&opt_vike).as_deref(), Some(want.as_path()));
    // …and from anywhere below it.
    assert_eq!(project_settings_dir(&opt_vike.join("bin")).as_deref(), Some(want.as_path()));
    assert_eq!(project_state_dir(&opt_vike).as_deref(), Some(want.join(STATE_SUBDIR).as_path()));
}

/// ⚠ **THE DEPLOYMENT HIJACK — the half #1101 left behind.** A deployment is a `settings/`
/// directory beside a binary with **no `Cargo.toml` at that level**, and ONE unrelated
/// `[package]` manifest anywhere above it used to take it: with no `[workspace]` table on the
/// chain the manifest arm falls back to the NEAREST manifest, which is still above the
/// deployment, and the `settings/` arm was never reached at all because a manifest existed.
///
/// Reproduced end-to-end on the CI box with a real `vike-cli` before the fix, in exactly this shape:
/// from a deployment holding `settings/policy.toml` (ceiling 22.0) and `settings/secrets.env`,
/// `secrets list` printed `no store found — every venue stays paper` and `config show` reported
/// `policy.max_notional_per_order  -  default` — a daemon with **no ceiling and no
/// credentials**, silently, because one stranger's manifest sat above the install directory.
#[test]
fn a_stray_manifest_above_a_deployment_cannot_capture_it() {
    let scratch = Scratch::new("deploy-hijack");
    let parent = scratch.path();
    std::fs::write(parent.join("Cargo.toml"), "[package]\nname=\"unrelated\"\n").unwrap();
    let opt_vike = parent.join("opt-vike");
    std::fs::create_dir_all(opt_vike.join("bin")).unwrap();
    std::fs::create_dir_all(opt_vike.join(PROJECT_SETTINGS_DIR)).unwrap();

    let want = opt_vike.join(PROJECT_SETTINGS_DIR);
    for from in [&opt_vike, &opt_vike.join("bin")] {
        assert_eq!(
            project_settings_dir(from).as_deref(),
            Some(want.as_path()),
            "a stranger's manifest above a deployment must not capture its settings"
        );
    }
    assert_eq!(project_state_dir(&opt_vike).as_deref(), Some(want.join(STATE_SUBDIR).as_path()));
}

/// …and when the stranger has a `settings/` of its own, the deployment must still take ITS OWN
/// — the NEAREST one, per the blast-radius reasoning the deployment marker was chosen for.
/// Measured on the CI box before the fix: the stranger's 999.0 ceiling won over the deployment's
/// 22.0, and `secrets list` printed the stranger's key and never the deployment's.
#[test]
fn a_deployment_under_a_stray_manifest_takes_the_nearest_settings_dir() {
    let scratch = Scratch::new("deploy-hijack-both");
    let parent = scratch.path();
    std::fs::write(parent.join("Cargo.toml"), "[package]\nname=\"unrelated\"\n").unwrap();
    std::fs::create_dir_all(parent.join(PROJECT_SETTINGS_DIR)).unwrap();
    let opt_vike = parent.join("opt-vike");
    std::fs::create_dir_all(opt_vike.join(PROJECT_SETTINGS_DIR)).unwrap();

    assert_eq!(
        project_settings_dir(&opt_vike).as_deref(),
        Some(opt_vike.join(PROJECT_SETTINGS_DIR).as_path()),
        "the deployment's own settings/ is nearer than the stranger's — it must win"
    );
}

/// **The cure must not overreach into #1101's bug.** A stray `settings/` ABOVE a plain-package
/// project must not capture it: the project's own manifest is NEARER, and it is the nearest
/// marker OF EITHER KIND that answers — not "any `settings/` outranks any workspace-less
/// manifest", which would hand back exactly the credential theft #1101 fixed.
#[test]
fn a_stray_settings_dir_above_a_plain_package_project_cannot_capture_it() {
    let scratch = Scratch::new("no-overreach");
    let outer = scratch.path();
    std::fs::write(outer.join("Cargo.toml"), "[package]\nname=\"unrelated\"\n").unwrap();
    std::fs::create_dir_all(outer.join(PROJECT_SETTINGS_DIR)).unwrap();
    let proj = outer.join("proj");
    std::fs::create_dir_all(proj.join("src")).unwrap();
    std::fs::write(proj.join("Cargo.toml"), "[package]\nname=\"proj\"\n").unwrap();
    // …and the project has NOT created its settings/ yet: the stray's is the only one that
    // EXISTS, which is precisely when a settings-first rule would reach for it.

    let want = proj.join(PROJECT_SETTINGS_DIR);
    for from in [&proj, &proj.join("src")] {
        assert_eq!(
            project_settings_dir(from).as_deref(),
            Some(want.as_path()),
            "the project's own manifest is nearer than the stray settings/ — it must win"
        );
    }
}

/// **An UNREADABLE manifest still decides ALONE**, so a crate-level `settings/` cannot answer
/// for a checkout whose root manifest could not be read. This is the guard on NOT applying the
/// nearest-marker rule to every case: the aster crate directory holds BOTH markers, so a walk
/// that let `settings/` compete here would resolve `crates/bridges/aster/settings` — #1089
/// exactly, credentials silently gone and every venue on paper.
#[test]
fn an_unreadable_root_manifest_still_outranks_a_crate_level_settings_dir() {
    let scratch = Scratch::new("unreadable-vs-settings");
    let root = scratch.path();
    std::fs::write(root.join("Cargo.toml"), [0xff_u8, 0xfe, 0x00, 0x80]).unwrap();
    let crate_dir = root.join("crates").join("bridges").join("aster");
    std::fs::create_dir_all(crate_dir.join("src")).unwrap();
    std::fs::write(crate_dir.join("Cargo.toml"), "[package]\nname=\"a\"\n").unwrap();
    std::fs::create_dir_all(crate_dir.join(PROJECT_SETTINGS_DIR)).unwrap();

    assert_eq!(
        project_settings_dir(&crate_dir.join("src")).as_deref(),
        Some(root.join(PROJECT_SETTINGS_DIR).as_path()),
        "an unreadable manifest is evidence of nothing — it must not let a crate-level \
             settings/ answer, which is #1089"
    );
}

/// The deployment marker is the NEAREST one, so a stray `settings/` higher up loses to the
/// deployment's own.
#[test]
fn the_deployment_marker_takes_the_nearest_settings_dir() {
    let scratch = Scratch::new("deploy-nearest");
    let stray = scratch.path().join("srv");
    let opt_vike = stray.join("opt").join("vike");
    std::fs::create_dir_all(opt_vike.join(PROJECT_SETTINGS_DIR)).unwrap();
    std::fs::create_dir_all(stray.join(PROJECT_SETTINGS_DIR)).unwrap();

    assert_eq!(
        project_settings_dir(&opt_vike).as_deref(),
        Some(opt_vike.join(PROJECT_SETTINGS_DIR).as_path()),
    );
}

/// A `settings` FILE is not a settings directory — the probe is `is_dir`, not `exists`.
#[test]
fn a_settings_file_is_not_the_marker() {
    let scratch = Scratch::new("settings-file");
    let dir = scratch.path().join("deploy");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(PROJECT_SETTINGS_DIR), "not a directory").unwrap();
    assert_eq!(project_settings_dir(&dir), None);
}

/// Neither marker anywhere ⇒ `None`. Nothing is invented from the CWD, and a deployment that
/// has not created `settings/` yet is told so by the caller rather than guessed at.
///
/// ⚠ Assumes the system temp directory's own ancestry holds neither marker — the same
/// assumption `vike-secrets`' sibling test already makes for `Cargo.toml`. A failure here means
/// a stray `settings/` or `Cargo.toml` was created above `TMPDIR`, not that the walk regressed.
#[test]
fn no_marker_anywhere_still_refuses_to_guess() {
    let scratch = Scratch::new("no-marker");
    let deep = scratch.path().join("a").join("b").join("c");
    std::fs::create_dir_all(&deep).unwrap();
    assert_eq!(project_settings_dir(&deep), None, "no marker above TMPDIR: see the doc note");
    assert_eq!(project_state_dir(&deep), None);
}
