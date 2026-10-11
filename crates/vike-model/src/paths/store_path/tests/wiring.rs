use super::*;

// ---- THE WIRING: `resolve_store_root_from`, the one site rungs 4 and 5 are assembled at ----

/// A shaped environment map, as `std::env::vars().collect()` would produce it. (Declared here
/// as well as beside the `user_data_dir_from_vars` tests below; this block is above them.)
fn vars_of(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect()
}

/// **THE mutation gate for B5.** `resolve_store_root`'s rungs 4 and 5 are two adjacent
/// `Option<PathBuf>`s: transposing them compiles silently and sends gigabytes to the wrong
/// disk. Every binary now assembles them HERE and nowhere else, so this one assertion covers
/// all four call sites at once.
///
/// The two answers are made unmistakably different — a real project directory in a scratch
/// tree, and a per-user directory derived from a fake `$HOME` — so a transposition inside
/// `resolve_store_root_from` reddens on the value, not on a subtle path suffix. Verified by
/// mutation: swapping the two arguments in that function makes this test fail with the
/// `$HOME`-derived path.
#[test]
fn the_wiring_puts_the_project_before_the_user_dir() {
    let scratch = Scratch::new("wiring");
    let project = scratch.path().join("proj");
    std::fs::create_dir_all(project.join(crate::paths::state_path::PROJECT_SETTINGS_DIR)).unwrap();
    let fake_home = scratch.path().join("home");
    // ⚠ A stray `Cargo.toml` declaring `[workspace]` above the system temp dir would capture
    // the walk and make the assertion below mean something else. Say so here rather than
    // failing later with a confusing path mismatch.
    assert_eq!(
        crate::paths::state_path::project_hist_store_dir(&project).as_deref(),
        Some(project.join("market_data").join("hist").as_path()),
        "precondition: the walk must find this scratch project"
    );

    let vars = vars_of(&[
        (HOME_VAR, fake_home.to_str().unwrap()),
        (XDG_DATA_HOME_VAR, fake_home.to_str().unwrap()),
        (LOCALAPPDATA_VAR, fake_home.to_str().unwrap()),
    ]);
    let got =
        resolve_store_root_from(None, None, Some(Path::new(NO_CHECKOUT)), Some(&project), &vars);

    assert_eq!(
        got.root,
        project.join("market_data").join("hist"),
        "the PROJECT rung must answer before the per-user directory"
    );
    assert_eq!(got.rung, StoreRootRung::Project);
    // The user dir is genuinely available and genuinely different — otherwise the assertion
    // above would pass for a transposed wiring too.
    let user = user_data_dir_from_vars(&vars).expect("the fixture supplies a per-user dir");
    assert_ne!(got.root, user, "the two rungs must be distinguishable in this fixture");
}

/// …and with NO project above `cwd`, the SAME call falls to the per-user directory — the other
/// half of the transposition proof, since a wiring that hard-coded either rung would fail one
/// of these two.
#[test]
fn the_wiring_falls_to_the_user_dir_when_there_is_no_project() {
    let scratch = Scratch::new("wiring-noproj");
    let bare = scratch.path().join("nowhere");
    std::fs::create_dir_all(&bare).unwrap();
    // ⚠ A stray marker above the system temp dir would make this vacuous. Skip, never pass.
    if crate::paths::state_path::project_hist_store_dir(&bare).is_some() {
        eprintln!("skipped: a project marker exists above {}", bare.display());
        return;
    }
    let fake_home = scratch.path().join("home");
    let vars = vars_of(&[
        (HOME_VAR, fake_home.to_str().unwrap()),
        (XDG_DATA_HOME_VAR, fake_home.to_str().unwrap()),
        (LOCALAPPDATA_VAR, fake_home.to_str().unwrap()),
    ]);

    let got = resolve_store_root_from(None, None, Some(Path::new(NO_CHECKOUT)), Some(&bare), &vars);
    assert_eq!(got.root, user_data_dir_from_vars(&vars).unwrap());
    assert_eq!(got.rung, StoreRootRung::UserDir);
}

/// **B4 through the wiring:** `VIKE_SETTINGS_DIR` relocates the project, so the store's default
/// moves with it. A binary cannot forget to honour it, because the lookup happens inside
/// `resolve_store_root_from` rather than at each call site.
#[test]
fn the_wiring_honours_the_settings_dir_override() {
    let scratch = Scratch::new("wiring-override");
    // A resolvable project on the walk, so this proves the override BEAT it.
    let walked = scratch.path().join("walked");
    std::fs::create_dir_all(walked.join(crate::paths::state_path::PROJECT_SETTINGS_DIR)).unwrap();
    let relocated = scratch.path().join("relocated");

    let vars = vars_of(&[(
        crate::paths::state_path::SETTINGS_DIR_ENV,
        relocated.join(crate::paths::state_path::PROJECT_SETTINGS_DIR).to_str().unwrap(),
    )]);
    let got =
        resolve_store_root_from(None, None, Some(Path::new(NO_CHECKOUT)), Some(&walked), &vars);

    assert_eq!(
        got.root,
        relocated.join("market_data").join("hist"),
        "the store must follow VIKE_SETTINGS_DIR, not stay on the walk"
    );
    // ⚠ CHANGED, and the change is the whole point: this used to report `Project`. Setting the
    // variable IS the declaration, so the rung an operator reads now says which project rung
    // answered — and `declared-project` is the one that outranks a dev checkout.
    assert_eq!(got.rung, StoreRootRung::DeclaredProject);
    assert_ne!(got.root, walked.join("market_data").join("hist"));
}

/// **THE DEFECT.** A project DECLARED by `$VIKE_SETTINGS_DIR` must outrank the dev-checkout
/// hinge, which is the program's own INFERENCE that the build tree still exists at the path
/// baked into this binary.
#[test]
fn a_declared_project_outranks_a_dev_checkout() {
    let scratch = Scratch::new("declared-vs-checkout");
    let walked = scratch.path().join("walked");
    std::fs::create_dir_all(walked.join(crate::paths::state_path::PROJECT_SETTINGS_DIR)).unwrap();
    let declared = scratch.path().join("declared");
    let checkout =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("market_data").join("hist");

    let declared_vars = vars_of(&[(
        crate::paths::state_path::SETTINGS_DIR_ENV,
        declared.join(crate::paths::state_path::PROJECT_SETTINGS_DIR).to_str().unwrap(),
    )]);

    // ⚠ The precondition is keyed on the fixture, NOT on the thing under test: with nothing
    // declared, this `repo_default` must genuinely be a live checkout. A guard keyed on the
    // resolution itself would SKIP rather than fail under mutation.
    let without =
        resolve_store_root_from(None, None, Some(&checkout), Some(&walked), &vars_of(&[]));
    assert_eq!(
        (without.root.as_path(), without.rung),
        (checkout.as_path(), StoreRootRung::DevCheckout),
        "precondition: this fixture's repo_default really is a live checkout"
    );

    let got = resolve_store_root_from(None, None, Some(&checkout), Some(&walked), &declared_vars);
    assert_eq!(
        (got.root, got.rung),
        (declared.join("market_data").join("hist"), StoreRootRung::DeclaredProject),
        "a project DECLARED by VIKE_SETTINGS_DIR must outrank the build checkout"
    );
}

/// **THE NO-REGRESSION ASSERTION, and it must be exhaustive enough that a careless reorder
/// cannot pass it.** A developer sets no variable, so their `<repo>/market_data/hist` still wins — over
/// the DISCOVERED project rung, over the per-user directory, and whether or not the walk found a
/// project at all.
///
/// The three shapes are asserted together because each alone is passable by a different wrong
/// ladder: dropping the hinge below the discovered project passes shape 3, and hoisting the
/// discovered project above the hinge passes shapes 2 and 3.
#[test]
fn without_the_variable_a_dev_checkout_still_beats_every_project_rung() {
    let scratch = Scratch::new("no-regression");
    let walked = scratch.path().join("walked");
    std::fs::create_dir_all(walked.join(crate::paths::state_path::PROJECT_SETTINGS_DIR)).unwrap();
    let checkout =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("market_data").join("hist");
    // ⚠ Keyed on the FIXTURE, not on the resolution: the hinge fires on the repo ROOT's
    // manifest, so that manifest existing is what makes every assertion below non-vacuous.
    assert!(
        checkout.parent().and_then(Path::parent).unwrap().join(REPO_MARKER).is_file(),
        "precondition: the repo root must carry a manifest, or the hinge cannot fire at all"
    );
    assert!(
        !checkout.is_dir(),
        "precondition: the fresh-clone shape — no market_data/hist leaf yet"
    );

    // 1. through the WIRING, with a walkable project beside it and a fully populated user dir.
    let fake_home = scratch.path().join("home");
    let vars = vars_of(&[
        (HOME_VAR, fake_home.to_str().unwrap()),
        (XDG_DATA_HOME_VAR, fake_home.to_str().unwrap()),
        (LOCALAPPDATA_VAR, fake_home.to_str().unwrap()),
    ]);
    let got = resolve_store_root_from(None, None, Some(&checkout), Some(&walked), &vars);
    assert_eq!(
        (got.root.as_path(), got.rung),
        (checkout.as_path(), StoreRootRung::DevCheckout),
        "no variable set: the developer's own checkout still answers"
    );

    // 2. through the LADDER, with the project rung explicitly DISCOVERED.
    let got = resolve_store_root(
        None,
        None,
        Some(&checkout),
        ProjectDefault::Discovered(PathBuf::from("project")),
        Some(PathBuf::from("user")),
    );
    assert_eq!(
        (got.root.as_path(), got.rung),
        (checkout.as_path(), StoreRootRung::DevCheckout),
        "a DISCOVERED project must never displace a live checkout"
    );

    // 3. …and with no project found at all, which must not change the answer either.
    let got = resolve_store_root(
        None,
        None,
        Some(&checkout),
        ProjectDefault::None,
        Some(PathBuf::from("user")),
    );
    assert_eq!(
        (got.root.as_path(), got.rung),
        (checkout.as_path(), StoreRootRung::DevCheckout),
        "and the checkout still beats the per-user directory"
    );
}

/// **A BLANK variable is not a declaration.** An empty `Environment=VIKE_SETTINGS_DIR=` line in
/// a unit file configures nothing, so promoting it above the dev-checkout hinge would relocate a
/// developer's store on the strength of a value nobody set. The workspace rule is
/// `crates/vike-model/src/paths/state_path.rs`'s `project_settings_dir_from`, which ignores a blank
/// override rather than honouring it; this asserts the ladder agrees.
#[test]
fn a_blank_settings_dir_is_not_a_declaration() {
    let scratch = Scratch::new("blank-declaration");
    let walked = scratch.path().join("walked");
    std::fs::create_dir_all(walked.join(crate::paths::state_path::PROJECT_SETTINGS_DIR)).unwrap();
    let checkout =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("market_data").join("hist");

    for blank in ["", "   ", "\t", "\n"] {
        let vars = vars_of(&[(crate::paths::state_path::SETTINGS_DIR_ENV, blank)]);
        let got = resolve_store_root_from(None, None, Some(&checkout), Some(&walked), &vars);
        assert_eq!(
            (got.root.as_path(), got.rung),
            (checkout.as_path(), StoreRootRung::DevCheckout),
            "a blank VIKE_SETTINGS_DIR ({blank:?}) must behave exactly as UNSET"
        );
    }

    // …and the classifier itself, so the rule has a named home rather than only an effect.
    assert_eq!(
        project_default_from(Some("  "), Some(PathBuf::from("p"))),
        ProjectDefault::Discovered(PathBuf::from("p")),
        "a blank override leaves the walk's answer DISCOVERED"
    );
    assert_eq!(
        project_default_from(Some(" /x/settings "), Some(PathBuf::from("p"))),
        ProjectDefault::Declared(PathBuf::from("p")),
        "…and a value with surrounding whitespace is still a declaration"
    );
    assert_eq!(project_default_from(Some("/x/settings"), None), ProjectDefault::None);
}

/// **Rungs 1 and 2 still outrank BOTH project rungs**, proven with a declaration in play and a
/// dev checkout underneath — the configuration where a mis-ordered insert would be invisible to
/// every other test here. The `config.store_root` row names the STORE; `$VIKE_SETTINGS_DIR` names
/// the PROJECT, and naming the store is the more specific statement.
#[test]
fn the_stated_rungs_still_outrank_a_declared_project() {
    let scratch = Scratch::new("stated-vs-declared");
    let walked = scratch.path().join("walked");
    std::fs::create_dir_all(walked.join(crate::paths::state_path::PROJECT_SETTINGS_DIR)).unwrap();
    let declared = scratch.path().join("declared");
    let checkout =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("market_data").join("hist");
    let vars = vars_of(&[(
        crate::paths::state_path::SETTINGS_DIR_ENV,
        declared.join(crate::paths::state_path::PROJECT_SETTINGS_DIR).to_str().unwrap(),
    )]);

    // ⚠ The independent precondition: this fixture really does reach the DECLARED rung when
    // nothing is stated above it. Without this the two assertions below pass vacuously for a
    // ladder that had lost the rung entirely.
    let bare = resolve_store_root_from(None, None, Some(&checkout), Some(&walked), &vars);
    assert_eq!(
        (bare.root, bare.rung),
        (declared.join("market_data").join("hist"), StoreRootRung::DeclaredProject),
        "precondition: the declaration is live in this fixture"
    );

    let got = resolve_store_root_from(
        Some("/x/explicit".into()),
        Some("/y/env".into()),
        Some(&checkout),
        Some(&walked),
        &vars,
    );
    assert_eq!((got.root, got.rung), (PathBuf::from("/x/explicit"), StoreRootRung::Explicit));

    let got =
        resolve_store_root_from(None, Some("/y/env".into()), Some(&checkout), Some(&walked), &vars);
    assert_eq!((got.root, got.rung), (PathBuf::from("/y/env"), StoreRootRung::Configured));
}

/// The wiring changes NO rung above 4: an explicit path and the `config.store_root` row still win,
/// and a dev checkout still outranks the project — proven through the same entry point the
/// binaries call, not only through the ladder underneath it.
#[test]
fn the_wiring_leaves_the_stated_and_checkout_rungs_untouched() {
    let scratch = Scratch::new("wiring-above");
    let project = scratch.path().join("proj");
    std::fs::create_dir_all(project.join(crate::paths::state_path::PROJECT_SETTINGS_DIR)).unwrap();
    let checkout =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("market_data").join("hist");
    let vars = vars_of(&[]);

    let got = resolve_store_root_from(
        Some("/x/explicit".into()),
        Some("/y/env".into()),
        Some(Path::new(NO_CHECKOUT)),
        Some(&project),
        &vars,
    );
    assert_eq!((got.root, got.rung), (PathBuf::from("/x/explicit"), StoreRootRung::Explicit));

    let got = resolve_store_root_from(
        None,
        Some("/y/env".into()),
        Some(Path::new(NO_CHECKOUT)),
        Some(&project),
        &vars,
    );
    assert_eq!((got.root, got.rung), (PathBuf::from("/y/env"), StoreRootRung::Configured));

    let got = resolve_store_root_from(None, None, Some(&checkout), Some(&project), &vars);
    assert_eq!((got.root, got.rung), (checkout, StoreRootRung::DevCheckout));
}

/// A binary that cannot read its own working directory passes `None`, and the resolution still
/// answers — with the per-user directory, never with something CWD-relative.
#[test]
fn the_wiring_survives_an_unknown_working_directory() {
    let vars = vars_of(&[(HOME_VAR, "/home/u"), (XDG_DATA_HOME_VAR, "/xdg")]);
    let got = resolve_store_root_from(None, None, Some(Path::new(NO_CHECKOUT)), None, &vars);
    assert_eq!(got.root, user_data_dir_from_vars(&vars).unwrap());
    assert_eq!(got.rung, StoreRootRung::UserDir);
}
