use super::*;

/// The blank-override law, tested once now that it is spelled once. A blank `VIKE_SETTINGS_DIR`
/// must fall through to the walk rather than resolving the settings directory to `""` and
/// reading credentials out of whatever the working directory happens to be.
///
/// Deliberately a pure-function test: the sibling path helpers consult the real CWD, and
/// `std::env::set_current_dir` is PROCESS-GLOBAL — using it here would race every other test in
/// this binary.
#[test]
fn a_blank_override_is_not_an_override() {
    assert_eq!(nonblank(Some("/srv/vike-<unit>")), Some("/srv/vike-<unit>"));
    assert_eq!(nonblank(Some("  /srv/vike-<unit>  ")), Some("/srv/vike-<unit>"));
    assert_eq!(nonblank(Some("")), None, "empty is not a directory");
    assert_eq!(nonblank(Some("   \t ")), None, "and neither is whitespace");
    assert_eq!(nonblank(None), None);
}

#[test]
fn loads_without_panic_and_returns_map() {
    // The store may or may not exist in a given checkout; either way the helper must return a
    // map (empty when absent) and never panic.
    let vars = load_project_secrets(None);
    let _n: usize = vars.len();
}

#[test]
fn the_store_is_the_database_inside_the_projects_settings_dir() {
    let path = workspace_db_path_from(None);
    assert_eq!(path.file_name().and_then(|n| n.to_str()), Some(DB_FILE));
    let db_dir = path.parent().expect("the database sits in a directory");
    assert_eq!(db_dir.file_name().and_then(|n| n.to_str()), Some(DB_DIR));
    assert_eq!(
        db_dir.parent().and_then(|p| p.file_name()).and_then(|n| n.to_str()),
        Some(PROJECT_SETTINGS_DIR),
        "the credential store lives in <project>/settings/db/, not the project root"
    );
}

/// ⚠ The walk must reach the WORKSPACE root, not the first `Cargo.toml`. Every crate has one,
/// and `cargo test -p <crate>` sets the CWD to the crate directory — so a first-match walk
/// resolved `crates/<c>/settings/` and the credentials silently vanished. That
/// shipped, and this is the regression guard.
#[test]
fn the_walk_reaches_the_workspace_root_not_the_nearest_crate() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let crate_dir = root.join("crates").join("bridges").join("aster");
    std::fs::create_dir_all(crate_dir.join("src")).unwrap();
    std::fs::write(
        root.join("Cargo.toml"),
        "[workspace]
",
    )
    .unwrap();
    std::fs::write(
        crate_dir.join("Cargo.toml"),
        "[package]
name=\"a\"
",
    )
    .unwrap();
    let want = root.join(PROJECT_SETTINGS_DIR);
    // From the crate dir — where `cargo test -p aster` runs — and from its src/.
    assert_eq!(project_settings_dir(&crate_dir).as_deref(), Some(want.as_path()));
    assert_eq!(project_settings_dir(&crate_dir.join("src")).as_deref(), Some(want.as_path()));
    assert_eq!(project_settings_dir(root).as_deref(), Some(want.as_path()));
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
    let tmp = tempfile::tempdir().unwrap();
    let outer = tmp.path();
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
            "a stranger's manifest above the project must not capture its credentials"
        );
    }
}

/// The same escape with **no `[workspace]` table anywhere** — a single-crate project under a
/// stray manifest. Nothing on the chain claims to be a workspace root, so the NEAREST manifest
/// is the project and the outermost is just the nearest stranger.
///
/// ⚠ This arm cannot reopen #1089: that bug needs `cargo test -p <crate>`, which needs a
/// workspace, which needs a `[workspace]` table — and a chain that has one never reaches here.
#[test]
fn a_plain_package_project_under_a_stray_manifest_keeps_its_own_store() {
    let tmp = tempfile::tempdir().unwrap();
    let outer = tmp.path();
    std::fs::write(outer.join("Cargo.toml"), "[package]\nname=\"unrelated\"\n").unwrap();
    let proj = outer.join("proj");
    std::fs::create_dir_all(proj.join("src")).unwrap();
    std::fs::write(proj.join("Cargo.toml"), "[package]\nname=\"proj\"\n").unwrap();

    let want = proj.join(PROJECT_SETTINGS_DIR);
    assert_eq!(project_settings_dir(&proj.join("src")).as_deref(), Some(want.as_path()));
}

/// A COMMENTED-OUT `[workspace]` is not a workspace table — the marker is a table HEADER, and
/// `# [workspace]` is prose. Without this the stray above would capture the project again.
#[test]
fn a_commented_out_workspace_table_is_not_a_workspace_root() {
    let tmp = tempfile::tempdir().unwrap();
    let outer = tmp.path();
    std::fs::write(outer.join("Cargo.toml"), "# [workspace]\n[package]\nname=\"x\"\n").unwrap();
    let proj = outer.join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    std::fs::write(proj.join("Cargo.toml"), "[workspace]\n").unwrap();

    let want = proj.join(PROJECT_SETTINGS_DIR);
    assert_eq!(project_settings_dir(&proj).as_deref(), Some(want.as_path()));
}

/// A NESTED workspace resolves to the OUTERMOST one, not the nearest — this repo HAS one
/// (`crates/bridges/ctrader/protogen` carries its own `[workspace]` table so the drift-gate
/// codegen stays out of the build), and a tool run from inside it must still find the project's
/// settings. This is the guard on choosing "outermost `[workspace]`" over "nearest".
#[test]
fn a_nested_workspace_still_resolves_to_the_outermost_one() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
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
    let tmp = tempfile::tempdir().unwrap();
    let outer = tmp.path();
    std::fs::write(outer.join("Cargo.toml"), [0xff_u8, 0xfe, 0x00, 0x80]).unwrap();
    let proj = outer.join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    std::fs::write(proj.join("Cargo.toml"), "[package]\nname=\"proj\"\n").unwrap();

    assert_eq!(
        project_settings_dir(&proj).as_deref(),
        Some(outer.join(PROJECT_SETTINGS_DIR).as_path()),
    );
}

/// ⚠ Assumes the system temp directory's own ancestry holds NEITHER marker — no `Cargo.toml`
/// and no `settings/`. A failure here means a stray was created above `TMPDIR`, not that the
/// walk regressed.
#[test]
fn the_walk_finds_the_project_root_and_refuses_when_there_is_none() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let deep = root.join("crates").join("vike-secrets").join("src");
    std::fs::create_dir_all(&deep).unwrap();
    // Neither marker anywhere above: no guess.
    assert_eq!(project_settings_dir(&deep), None);
    // Now the project exists — every depth resolves to the SAME directory.
    std::fs::write(root.join("Cargo.toml"), "[workspace]\n").unwrap();
    let want = root.join(PROJECT_SETTINGS_DIR);
    assert_eq!(project_settings_dir(&deep).as_deref(), Some(want.as_path()));
    assert_eq!(project_settings_dir(root).as_deref(), Some(want.as_path()));
}

/// **The DEPLOYMENT shape.** A project root holds a binary, a profile and `settings/`, and no
/// `Cargo.toml` anywhere above it — the layout all three shipped systemd units install and run
/// from (`WorkingDirectory=<project>`). Before the second marker this resolved to `None`, so a
/// production daemon loaded NO credentials and every venue silently stayed on paper.
#[test]
fn a_deployment_without_a_cargo_toml_resolves_through_its_settings_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let opt_vike = tmp.path().join("opt").join("vike");
    std::fs::create_dir_all(opt_vike.join("bin")).unwrap();
    std::fs::create_dir_all(opt_vike.join(PROJECT_SETTINGS_DIR)).unwrap();

    let want = opt_vike.join(PROJECT_SETTINGS_DIR);
    // From the unit's WorkingDirectory, and from anywhere below it.
    assert_eq!(project_settings_dir(&opt_vike).as_deref(), Some(want.as_path()));
    assert_eq!(project_settings_dir(&opt_vike.join("bin")).as_deref(), Some(want.as_path()));
    // The marker is a DIRECTORY probe, so nothing needs to exist inside it yet.
    assert!(std::fs::read_dir(&want).unwrap().next().is_none(), "the settings dir is empty");
}

/// ⚠ **THE DEPLOYMENT HIJACK — the half #1101 left behind.** A deployment is a `settings/`
/// directory beside a binary with **no `Cargo.toml` at that level**, and ONE unrelated
/// `[package]` manifest anywhere above it used to take it: with no `[workspace]` table on the
/// chain the manifest arm falls back to the NEAREST manifest, which is still above the
/// deployment, and the `settings/` arm was never reached at all because a manifest existed.
///
/// Reproduced end-to-end on the CI box with a real `vike-cli` before the fix: from a deployment
/// holding its store under `settings/`, `secrets list` printed `no store found — every venue stays
/// paper` — the live gate, silently, because one stranger's manifest sat above the install
/// directory.
#[test]
fn a_stray_manifest_above_a_deployment_cannot_capture_its_store() {
    let tmp = tempfile::tempdir().unwrap();
    let parent = tmp.path();
    std::fs::write(parent.join("Cargo.toml"), "[package]\nname=\"unrelated\"\n").unwrap();
    let opt_vike = parent.join("opt-vike");
    std::fs::create_dir_all(opt_vike.join("bin")).unwrap();
    std::fs::create_dir_all(opt_vike.join(PROJECT_SETTINGS_DIR)).unwrap();

    let want = opt_vike.join(PROJECT_SETTINGS_DIR);
    for from in [&opt_vike, &opt_vike.join("bin")] {
        assert_eq!(
            project_settings_dir(from).as_deref(),
            Some(want.as_path()),
            "a stranger's manifest above a deployment must not capture its credentials"
        );
    }
}

/// …and when the stranger has a `settings/` of its own, the deployment must still take ITS OWN
/// — the NEAREST one. Measured on the CI box before the fix: `secrets list` printed the STRANGER'S
/// key and never the deployment's.
#[test]
fn a_deployment_under_a_stray_manifest_takes_the_nearest_settings_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let parent = tmp.path();
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
    let tmp = tempfile::tempdir().unwrap();
    let outer = tmp.path();
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
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
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

/// The deployment marker is the NEAREST `settings/`, so a stray one higher up cannot capture a
/// deployment that has its own.
#[test]
fn the_deployment_marker_is_the_nearest_settings_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let stray = tmp.path().join("srv");
    let opt_vike = stray.join("opt").join("vike");
    std::fs::create_dir_all(opt_vike.join(PROJECT_SETTINGS_DIR)).unwrap();
    std::fs::create_dir_all(stray.join(PROJECT_SETTINGS_DIR)).unwrap();

    assert_eq!(
        project_settings_dir(&opt_vike).as_deref(),
        Some(opt_vike.join(PROJECT_SETTINGS_DIR).as_path()),
    );
}

/// **A checkout is unaffected by either kind of `settings/` stray** — the "dev behaviour does
/// not change" half of the precedence rule, in the two configurations that could break it: a
/// `settings/` at a CRATE level (nearer than the workspace root) and one ABOVE the checkout.
#[test]
fn a_source_checkout_ignores_settings_dirs_above_and_below_its_root() {
    let tmp = tempfile::tempdir().unwrap();
    let outer = tmp.path();
    std::fs::create_dir_all(outer.join(PROJECT_SETTINGS_DIR)).unwrap(); // a stray ABOVE the checkout
    let root = outer.join("checkout");
    let crate_dir = root.join("crates").join("bridges").join("aster");
    std::fs::create_dir_all(crate_dir.join("src")).unwrap();
    std::fs::create_dir_all(crate_dir.join(PROJECT_SETTINGS_DIR)).unwrap(); // …and one BELOW the root
    std::fs::write(root.join("Cargo.toml"), "[workspace]\n").unwrap();
    std::fs::write(crate_dir.join("Cargo.toml"), "[package]\nname=\"a\"\n").unwrap();

    let want = root.join(PROJECT_SETTINGS_DIR);
    for from in [&root, &crate_dir, &crate_dir.join("src")] {
        assert_eq!(
            project_settings_dir(from).as_deref(),
            Some(want.as_path()),
            "the workspace root must win over every settings/ stray"
        );
    }

    // …including a `settings/` at a level with NO manifest of its own — deployment-SHAPED, but
    // inside a checkout. **The one accepted residual, pinned rather than left to drift**: it is
    // byte-identical to the #1089 tree, so no marker rule can serve both and a declared
    // workspace root must keep winning. `VIKE_SETTINGS_DIR` is the way to deploy inside one.
    let inside = root.join("deploy").join("vike");
    std::fs::create_dir_all(inside.join(PROJECT_SETTINGS_DIR)).unwrap();
    assert_eq!(
        project_settings_dir(&inside).as_deref(),
        Some(want.as_path()),
        "a declared [workspace] root decides alone — relaxing that is #1089"
    );
}

/// A `settings` FILE is not the marker — the probe is `is_dir`, not `exists`.
#[test]
fn a_settings_file_is_not_the_deployment_marker() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("deploy");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(PROJECT_SETTINGS_DIR), "not a directory").unwrap();
    assert_eq!(project_settings_dir(&dir), None);
}

/// **The override wins over both markers**; blank falls through to the walk.
#[test]
fn the_settings_dir_override_beats_the_walk() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::write(root.join("Cargo.toml"), "[workspace]\n").unwrap();
    let elsewhere = root.join("elsewhere");

    assert_eq!(project_settings_dir_from(elsewhere.to_str(), root), Some(elsewhere.clone()));

    let want = root.join(PROJECT_SETTINGS_DIR);
    for blank in [None, Some(""), Some("  \t ")] {
        assert_eq!(project_settings_dir_from(blank, root).as_deref(), Some(want.as_path()));
    }
}

/// The override reaches the CWD-based entry point too — the one `resolve_project` is built on
/// — without this module ever reading the environment.
#[test]
fn the_override_reaches_the_cwd_entry_point() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("explicit");
    assert_eq!(
        workspace_db_path_from(dir.to_str()),
        db_path_in(&dir),
        "an explicit settings dir must not be second-guessed by the CWD walk"
    );
    assert_eq!(workspace_settings_dir_from(dir.to_str()), dir);
}

/// **With no working directory an override-blind call and an override-holding one DISAGREE — that
/// is the whole reason a caller holding an override may not drop it.**
///
/// It is tempting to argue that `workspace_db_path_from(None)` "is just the override arm under
/// `None`, so it cannot answer differently". It is the same function under a DIFFERENT argument,
/// and the argument decides: with a readable CWD the override short-circuits the walk and the
/// answers coincide, but `std::env::current_dir()` FAILS when the directory a process started in
/// was removed, unmounted or made unsearchable — and there the walk contributes nothing, so the
/// override is the only thing left that can name a directory. `None` then falls all the way
/// through to the RELATIVE last resort, which is a different database, resolved against a working
/// directory this process does not have.
///
/// The CWD is a parameter here rather than something this test sets, because
/// `std::env::set_current_dir` is process-global and would race every other test in this binary
/// — the reason [`db_path_for`] exists at all.
///
/// ⚠ The `assert_ne!` is the load-bearing line: an implementation that dropped the override on
/// the no-CWD arm would still satisfy both `assert_eq!`s above it if the last resort happened to
/// match, and this is what refuses that.
#[test]
fn with_no_working_directory_the_override_still_answers() {
    let named = "/srv/vike-<unit>/settings";
    let last_resort = db_path_in(Path::new(PROJECT_SETTINGS_DIR));

    assert_eq!(
        db_path_for(Some(named), None),
        db_path_in(Path::new(named)),
        "a NAMED settings directory needs no walk to reach it"
    );
    assert_eq!(
        db_path_for(None, None),
        last_resort,
        "with neither a walk nor a name, the relative last resort is all that is left"
    );
    assert_ne!(
        db_path_for(Some(named), None),
        db_path_for(None, None),
        "the override-blind spelling cannot produce the named store — so a caller that HAS an \
             override and drops it reports a database nothing reads"
    );

    // A blank override is still not an override, on this arm as on every other.
    for blank in [Some(""), Some("  \t ")] {
        assert_eq!(db_path_for(blank, None), last_resort);
    }
}

/// **The DIRECTORY resolver applies the same law as the DATABASE one, on the same arm** — the half
/// that did not exist, and whose absence let `vike_boot::boot` answer `None` for the settings
/// directory on a box whose credential store it was simultaneously opening by name.
///
/// Four inputs, because the law has four cases and only one of them was ever in doubt: a walk
/// with a start, a walk WITHOUT one, a name with a start, and a NAME WITHOUT ONE. The fourth is
/// where the fix lives, and it is the assertion that goes red without it.
///
/// The loop at the end is the other load-bearing half: the two resolvers must not merely both be
/// correct, they must be the SAME law — a database resolved into a directory the settings loader
/// believes does not exist is the split this pairing exists to prevent, and it is exactly what a
/// second inline copy of the law produced.
#[test]
fn the_directory_resolver_honours_an_override_with_no_working_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::write(root.join("Cargo.toml"), "[workspace]\n").unwrap();
    let named = root.join("elsewhere");

    assert_eq!(
        project_settings_dir_for(None, Some(root)).as_deref(),
        Some(root.join(PROJECT_SETTINGS_DIR).as_path()),
        "with a start and no name, the walk answers exactly as it always did"
    );
    assert_eq!(
        project_settings_dir_for(None, None),
        None,
        "with neither a start nor a name there is nothing to answer with"
    );
    assert_eq!(
        project_settings_dir_for(named.to_str(), Some(root)).as_deref(),
        Some(named.as_path()),
        "a name beats the walk, as it does in `project_settings_dir_from`"
    );
    assert_eq!(
        project_settings_dir_for(named.to_str(), None).as_deref(),
        Some(named.as_path()),
        "…and a name needs NO walk, so losing the working directory cannot drop it"
    );

    // A blank override is not an override here either — it must not resolve the settings
    // directory to `""`, which is the working directory this arm does not have.
    for blank in [Some(""), Some("  \t ")] {
        assert_eq!(project_settings_dir_for(blank, None), None, "{blank:?}");
    }

    // …and the DATABASE resolver is this one plus a join, on every arm above. One law, two names.
    for (o, cwd) in
        [(None, Some(root)), (None, None), (named.to_str(), Some(root)), (named.to_str(), None)]
    {
        if let Some(dir) = project_settings_dir_for(o, cwd) {
            assert_eq!(
                db_path_for(o, cwd),
                db_path_in(&dir),
                "the store must sit inside the directory the settings loader is handed"
            );
        }
    }
}

/// Bring a settings database into existence the one way that is allowed — `secrets init`'s
/// parameter — and file one credential row in it through the one writer.
fn plant_store_with_one_key(settings: &Path, name: &str, value: &str) {
    let dir = settings.to_str().expect("utf-8 tempdir");
    crate::create_store(Some(dir)).expect("secrets init creates the empty store");
    crate::save_credentials_to_store(
        settings,
        crate::Table::Credential,
        &[(name.to_string(), value.to_string())],
        Some(&crate::schema::Classification::unrecognised),
    )
    .expect("the store exists, so the write lands");
}

/// **The LOADER honours the override, which is the defect this pair was added for.** A path
/// helper that resolves the right store is worth nothing to a caller that then loads through an
/// override-blind reader — which is what every venue smoke did, silently skipping in a worktree
/// whose gitignored `settings/` holds no store.
///
/// Plants a store in a directory the CWD walk could never reach, so a regression cannot pass by
/// accident of this checkout having credentials of its own.
#[test]
fn the_loader_reads_the_named_settings_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let settings = tmp.path().join("named-settings");
    std::fs::create_dir_all(&settings).unwrap();
    plant_store_with_one_key(&settings, "BINANCE_DEMO_API_KEY", "from-the-override");

    let vars = load_project_secrets(settings.to_str());
    assert_eq!(
        vars.get("BINANCE_DEMO_API_KEY").map(String::as_str),
        Some("from-the-override"),
        "a named settings directory must supply the credentials"
    );

    // No override is the walk's answer: byte-identical to the fallible resolver's map under `None`.
    let walked = crate::resolve_project(None).map(|r| r.secrets.into_map()).unwrap_or_default();
    assert_eq!(load_project_secrets(None), walked);
    // …and so is a blank one: it configured nothing, so it must not resolve settings to `""`.
    for blank in [Some(""), Some("  \t ")] {
        assert_eq!(load_project_secrets(blank), walked);
    }
}

/// A named directory with no store in it is the ordinary unconfigured state, not an error —
/// the live gate (no creds → stay paper), and the reason this loader is infallible.
#[test]
fn a_named_settings_directory_with_no_store_is_an_empty_map() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(load_project_secrets(tmp.path().to_str()).is_empty());
}
