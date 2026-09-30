use super::*;

/// A `repo_default` guaranteed NOT to exist on any machine — the INSTALLED shape, where the
/// compile-time path names a directory on whoever's box did the build. Spelled once, because
/// every rung below the hinge is only reachable when the hinge does not fire.
const NO_CHECKOUT: &str = "/definitely/not/a/real/build/machine/path/market_data/hist";

#[test]
fn explicit_beats_everything() {
    let got = resolve_store_root(
        Some(PathBuf::from("explicit")),
        Some("env".into()),
        Some(Path::new("repo")),
        ProjectDefault::Discovered(PathBuf::from("project")),
        Some(PathBuf::from("user")),
    );
    assert_eq!(got.root, PathBuf::from("explicit"));
    assert_eq!(got.rung, StoreRootRung::Explicit);
}

#[test]
fn env_beats_the_defaults() {
    let got = resolve_store_root(
        None,
        Some("env".into()),
        Some(Path::new(NO_CHECKOUT)),
        ProjectDefault::Discovered(PathBuf::from("project")),
        Some(PathBuf::from("user")),
    );
    assert_eq!(got.root, PathBuf::from("env"));
    assert_eq!(got.rung, StoreRootRung::EnvVar);
}

/// An empty/whitespace `VIKE_HIST_STORE` must not win — it would resolve the store to "" and
/// create a store at the CWD, the exact class of bug this module exists to remove.
#[test]
fn a_blank_env_value_is_ignored() {
    let got = resolve_store_root(
        None,
        Some("   ".into()),
        Some(Path::new(NO_CHECKOUT)),
        ProjectDefault::Discovered(PathBuf::from("project")),
        Some(PathBuf::from("user")),
    );
    assert_eq!(got.root, PathBuf::from("project"));
}

/// THE compatibility hinge: in a dev checkout the answer is unchanged from before this module
/// existed — even though `market_data/hist` itself does not exist yet.
///
/// It outranks the PROJECT rung too, which is the whole reason the hinge survived the project
/// rung landing: a developer's populated `<repo>/market_data/hist` must not silently relocate, and a
/// store that relocates does not merge — the old one just stops being read.
#[test]
fn a_dev_checkout_keeps_the_repo_default_even_before_data_hist_exists() {
    // `<this crate>/market_data/hist` — the leaf does NOT exist, but its repo root does, which is
    // exactly the fresh-clone shape.
    let repo_default =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("market_data").join("hist");
    assert!(!repo_default.is_dir(), "precondition: the leaf must not exist");
    let got = resolve_store_root(
        None,
        None,
        Some(&repo_default),
        ProjectDefault::Discovered(PathBuf::from("project")),
        Some(PathBuf::from("user")),
    );
    assert_eq!(got.root, repo_default, "a checkout keeps resolving to <repo>/market_data/hist");
    assert_eq!(got.rung, StoreRootRung::DevCheckout);
}

/// **THE CONTAINER CASE, and the reason the hinge probes for a MANIFEST rather than for a
/// directory.** `<repo>` here exists and is EMPTY — which is not a contrived shape: a runtime
/// image's `WORKDIR /app` CREATES `/app`, and a builder stage that had the workspace at `/app`
/// bakes `/app/market_data/hist` into every binary it produces. The two collide with no source tree
/// anywhere, and a bare `is_dir` probe reads that empty directory as "the checkout that built
/// this binary is still here".
///
/// The cost of getting this wrong is the one this whole module exists to prevent, wearing its
/// worst clothes: the hinge outranks the project rung, so the tape lands on the container's
/// ephemeral layer instead of in the bind-mounted project, and it is gone at the next
/// `docker run` — a store does not merge, so the operator sees zero rows rather than an error.
#[test]
fn an_empty_directory_at_the_repo_root_is_not_a_checkout() {
    let scratch = Scratch::new("empty-repo-root");
    // Exactly what `WORKDIR /app` leaves behind: the directory, and nothing in it.
    let repo = scratch.path().join("app");
    std::fs::create_dir_all(&repo).unwrap();
    let repo_default = repo.join("market_data").join("hist");
    assert!(!repo_default.is_dir(), "precondition: the store leaf must not exist");
    assert!(repo.is_dir(), "precondition: the repo ROOT must exist — that is the whole trap");

    let got = resolve_store_root(
        None,
        None,
        Some(&repo_default),
        ProjectDefault::Discovered(PathBuf::from("project")),
        Some(PathBuf::from("user")),
    );
    assert_eq!(
        (got.root, got.rung),
        (PathBuf::from("project"), StoreRootRung::Project),
        "an empty directory is not a checkout: the PROJECT rung must answer"
    );
}

/// …and the other half, which is what stops the test above from being cured by simply deleting
/// the rung: a repo root carrying a `Cargo.toml` IS a checkout, and still answers before the
/// project — on a SYNTHETIC tree, so the claim is about the probe rather than about the one
/// directory this crate happens to be compiled in.
#[test]
fn a_repo_root_carrying_a_manifest_is_a_checkout_even_with_no_data_dir() {
    let scratch = Scratch::new("manifest-repo-root");
    let repo = scratch.path().join("checkout");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::write(repo.join("Cargo.toml"), "[workspace]\n").unwrap();
    let repo_default = repo.join("market_data").join("hist");
    assert!(
        !repo_default.is_dir(),
        "precondition: the fresh-clone shape — no market_data/hist yet"
    );

    let got = resolve_store_root(
        None,
        None,
        Some(&repo_default),
        ProjectDefault::Discovered(PathBuf::from("project")),
        Some(PathBuf::from("user")),
    );
    assert_eq!(
        (got.root, got.rung),
        (repo_default, StoreRootRung::DevCheckout),
        "a checkout still outranks the project rung"
    );
}

/// And when the store already exists, obviously still the repo default.
#[test]
fn an_existing_repo_default_wins_over_the_user_dir() {
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let got = resolve_store_root(
        None,
        None,
        Some(&dir),
        ProjectDefault::Discovered(PathBuf::from("project")),
        Some(PathBuf::from("user")),
    );
    assert_eq!(got.root, dir);
}

/// **THE new rung.** The installed case: the compile-time repo path does not exist on this
/// machine, so the PROJECT's own `market_data/hist` answers — not the per-user directory, which is
/// a second location outside the one folder the owner asked for.
#[test]
fn a_missing_repo_default_falls_through_to_the_project_dir() {
    let got = resolve_store_root(
        None,
        None,
        Some(Path::new(NO_CHECKOUT)),
        ProjectDefault::Discovered(PathBuf::from("project")),
        Some(PathBuf::from("user")),
    );
    assert_eq!(got.root, PathBuf::from("project"));
    assert_eq!(got.rung, StoreRootRung::Project);
}

/// …and with NO project above the working directory (a bare binary run from `/tmp`), the
/// per-user directory is still the answer. This is why that rung is kept rather than deleted:
/// the alternative is inventing a path from the CWD.
#[test]
fn without_a_project_it_falls_through_to_the_user_dir() {
    let got = resolve_store_root(
        None,
        None,
        Some(Path::new(NO_CHECKOUT)),
        ProjectDefault::None,
        Some(PathBuf::from("user")),
    );
    assert_eq!(got.root, PathBuf::from("user"));
    assert_eq!(got.rung, StoreRootRung::UserDir);
}

/// Total function: with neither a project nor a user dir (scrubbed env), it still answers —
/// and answers with the repo path rather than something CWD-relative.
#[test]
fn without_a_project_or_a_user_dir_it_still_answers_with_the_repo_default() {
    let got =
        resolve_store_root(None, None, Some(Path::new(NO_CHECKOUT)), ProjectDefault::None, None);
    assert_eq!(got.root, PathBuf::from(NO_CHECKOUT));
    assert_eq!(got.rung, StoreRootRung::LastResort);
}

/// **A build with NO checkout rung skips the hinge outright** — the shape every `--release`
/// asset ships in, since the shipped call sites pass the rung under `cfg(debug_assertions)`
/// only. Proved with a checkout that EXISTS (this crate's own), so the answer cannot come from
/// the probe declining: with `Some` the hinge fires, and with `None` there is nothing to fire
/// and the DISCOVERED project answers.
#[test]
fn a_build_with_no_checkout_rung_never_answers_with_the_checkout() {
    let checkout =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("market_data").join("hist");
    let with = resolve_store_root(
        None,
        None,
        Some(&checkout),
        ProjectDefault::Discovered(PathBuf::from("project")),
        Some(PathBuf::from("user")),
    );
    assert_eq!(with.rung, StoreRootRung::DevCheckout, "the fixture's checkout must be live");
    let without = resolve_store_root(
        None,
        None,
        None,
        ProjectDefault::Discovered(PathBuf::from("project")),
        Some(PathBuf::from("user")),
    );
    assert_eq!((without.root, without.rung), (PathBuf::from("project"), StoreRootRung::Project));
}

/// ...and its last resort is the CWD-relative `market_data/hist`, tagged as the last resort so
/// the log line never presents it as an answer — rung 7 of the module doc. Spelled through the
/// same two constants the project rung joins, so the leaf cannot drift from theirs.
#[test]
fn a_build_with_no_checkout_rung_falls_back_to_a_cwd_relative_last_resort() {
    let got = resolve_store_root(None, None, None, ProjectDefault::None, None);
    assert_eq!(
        got.root,
        PathBuf::from(crate::state_path::PROJECT_DATA_DIR).join(crate::state_path::HIST_SUBDIR)
    );
    assert!(got.root.is_relative(), "the no-checkout last resort is CWD-relative by design");
    assert_eq!(got.rung, StoreRootRung::LastResort);
}

/// **The whole precedence in ONE ordered assertion.** Each rung is knocked out in turn, and the
/// answer must step down exactly one place. A mutation that reorders two rungs — or drops one —
/// changes an answer here even when every single-rung test above still passes.
#[test]
fn the_rungs_step_down_in_order() {
    let repo = Path::new(NO_CHECKOUT);
    let checkout =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("market_data").join("hist");
    // ⚠ The two project rungs supply DIFFERENT paths, so a step that answered with the wrong
    // provenance is caught on the VALUE and not only on the rung tag.
    let declared = || ProjectDefault::Declared(PathBuf::from("declared"));
    let proj = || ProjectDefault::Discovered(PathBuf::from("project"));
    let user = || Some(PathBuf::from("user"));

    // 1. explicit — every other rung supplied, and still ignored.
    let got = resolve_store_root(
        Some("explicit".into()),
        Some("env".into()),
        Some(&checkout),
        declared(),
        user(),
    );
    assert_eq!((got.root, got.rung), (PathBuf::from("explicit"), StoreRootRung::Explicit));
    // 2. the env var, once nothing was stated on the command line — still above a DECLARED
    //    project, because `$VIKE_HIST_STORE` names the store itself rather than the project.
    let got = resolve_store_root(None, Some("env".into()), Some(&checkout), declared(), user());
    assert_eq!((got.root, got.rung), (PathBuf::from("env"), StoreRootRung::EnvVar));
    // 3. the DECLARED project, once the env var is gone — ABOVE the dev-checkout hinge, which
    //    is supplied here and must lose to it.
    let got = resolve_store_root(None, None, Some(&checkout), declared(), user());
    assert_eq!((got.root, got.rung), (PathBuf::from("declared"), StoreRootRung::DeclaredProject));
    // 4. the dev-checkout hinge, once nothing is declared — ABOVE the DISCOVERED project rung.
    let got = resolve_store_root(None, None, Some(&checkout), proj(), user());
    assert_eq!((got.root, got.rung), (checkout, StoreRootRung::DevCheckout));
    // 5. the discovered project, once this box has no checkout.
    let got = resolve_store_root(None, None, Some(repo), proj(), user());
    assert_eq!((got.root, got.rung), (PathBuf::from("project"), StoreRootRung::Project));
    // 6. the per-user dir, once there is no project either.
    let got = resolve_store_root(None, None, Some(repo), ProjectDefault::None, user());
    assert_eq!((got.root, got.rung), (PathBuf::from("user"), StoreRootRung::UserDir));
    // 7. and the repo path as a last resort, so the function is total.
    let got = resolve_store_root(None, None, Some(repo), ProjectDefault::None, None);
    assert_eq!((got.root, got.rung), (PathBuf::from(NO_CHECKOUT), StoreRootRung::LastResort));
}

/// **The rung a caller LOGS must be the rung that answered.** The path and the rung are two
/// fields of one struct, so a copy-paste that returned the right path with a neighbouring
/// rung would report a store as "explicit" while it came from the project walk — an operator
/// then trusts a value nobody stated. Every variant is reachable and distinct, and each
/// [`StoreRootRung::why`] sentence is non-empty, because an empty explanation in a log line is
/// the same as no log line.
#[test]
fn every_rung_is_reachable_and_carries_its_own_explanation() {
    use std::collections::BTreeSet;
    let checkout =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("market_data").join("hist");
    let repo = Path::new(NO_CHECKOUT);
    let none = || ProjectDefault::None;
    let seen: Vec<StoreRootRung> = vec![
        resolve_store_root(Some("x".into()), None, Some(repo), none(), None).rung,
        resolve_store_root(None, Some("y".into()), Some(repo), none(), None).rung,
        // ⚠ `repo`, not `checkout`: the DECLARED rung must report itself with NO checkout in
        // play, so this row proves the variant is reachable rather than proving the ordering
        // (which `the_rungs_step_down_in_order` owns, with the checkout supplied).
        resolve_store_root(None, None, Some(repo), ProjectDefault::Declared("d".into()), None).rung,
        resolve_store_root(None, None, Some(&checkout), none(), None).rung,
        resolve_store_root(None, None, Some(repo), ProjectDefault::Discovered("p".into()), None)
            .rung,
        resolve_store_root(None, None, Some(repo), none(), Some("u".into())).rung,
        resolve_store_root(None, None, Some(repo), none(), None).rung,
    ];
    assert_eq!(
        seen,
        vec![
            StoreRootRung::Explicit,
            StoreRootRung::EnvVar,
            StoreRootRung::DeclaredProject,
            StoreRootRung::DevCheckout,
            StoreRootRung::Project,
            StoreRootRung::UserDir,
            StoreRootRung::LastResort,
        ],
        "each rung must be reachable and report ITSELF"
    );
    let tags: BTreeSet<&str> = seen.iter().map(|r| r.as_str()).collect();
    assert_eq!(tags.len(), seen.len(), "the log tags must be distinct");
    for rung in &seen {
        assert!(!rung.why().trim().is_empty(), "{rung:?} must explain itself");
    }
}

/// The `Display` line a binary logs carries BOTH halves: an operator who only sees the path
/// cannot tell a deliberate `--store` from a walk that quietly moved.
#[test]
fn the_display_line_names_the_path_and_the_reason() {
    let got = resolve_store_root(
        None,
        None,
        Some(Path::new(NO_CHECKOUT)),
        ProjectDefault::Discovered("/p/market_data/hist".into()),
        None,
    );
    let line = got.to_string();
    assert!(line.contains("/p/market_data/hist"), "the path must be in the line: {line}");
    assert!(line.contains(StoreRootRung::Project.why()), "the reason must be too: {line}");
}

// ---- the OPERATIONAL cases: the real walk feeding the real precedence -----------------------

/// A private scratch directory under the system temp dir — this crate has no `tempfile`
/// dev-dependency, so the two deployment tests below make (and remove) their own.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let p = std::env::temp_dir().join(format!("vike-store-path-{tag}-{nanos}"));
        std::fs::create_dir_all(&p).expect("scratch dir");
        Self(p)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// **THE deployment case, end to end.** A project root holds a binary and a `settings/`
/// directory and no source tree — so the hinge cannot fire — and the tape must land in that
/// same folder,
/// not under `$HOME`. The walk and the precedence are exercised together here precisely because
/// each is separately correct in the shipped tree and the pairing is what was wrong.
///
/// ⚠ The `settings/` directory is what makes this resolve at all: it is the deployment MARKER
/// (`crates/vike-model/src/state_path.rs`'s `nearest_project_marker`), which is why the install
/// recipe creates it even when empty.
#[test]
fn a_deployment_with_a_settings_marker_stores_inside_the_project() {
    let scratch = Scratch::new("deployment");
    let opt_vike = scratch.path().join("opt").join("vike");
    std::fs::create_dir_all(opt_vike.join("settings")).unwrap();

    let project = crate::state_path::project_hist_store_dir(&opt_vike);
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
    if crate::state_path::project_hist_store_dir(&bare).is_some() {
        eprintln!("skipped: a project marker exists above {}", bare.display());
        return;
    }

    let got = resolve_store_root(
        None,
        None,
        Some(Path::new(NO_CHECKOUT)),
        project_default_from(None, crate::state_path::project_hist_store_dir(&bare)),
        Some(PathBuf::from("/home/u/.local/share/vike-data")),
    );
    assert_eq!(got.root, PathBuf::from("/home/u/.local/share/vike-data"));
    assert_eq!(got.rung, StoreRootRung::UserDir);
}

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
    std::fs::create_dir_all(project.join(crate::state_path::PROJECT_SETTINGS_DIR)).unwrap();
    let fake_home = scratch.path().join("home");
    // ⚠ A stray `Cargo.toml` declaring `[workspace]` above the system temp dir would capture
    // the walk and make the assertion below mean something else. Say so here rather than
    // failing later with a confusing path mismatch.
    assert_eq!(
        crate::state_path::project_hist_store_dir(&project).as_deref(),
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
    if crate::state_path::project_hist_store_dir(&bare).is_some() {
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
    std::fs::create_dir_all(walked.join(crate::state_path::PROJECT_SETTINGS_DIR)).unwrap();
    let relocated = scratch.path().join("relocated");

    let vars = vars_of(&[(
        crate::state_path::SETTINGS_DIR_ENV,
        relocated.join(crate::state_path::PROJECT_SETTINGS_DIR).to_str().unwrap(),
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
    std::fs::create_dir_all(walked.join(crate::state_path::PROJECT_SETTINGS_DIR)).unwrap();
    let declared = scratch.path().join("declared");
    let checkout =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("market_data").join("hist");

    let declared_vars = vars_of(&[(
        crate::state_path::SETTINGS_DIR_ENV,
        declared.join(crate::state_path::PROJECT_SETTINGS_DIR).to_str().unwrap(),
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
    std::fs::create_dir_all(walked.join(crate::state_path::PROJECT_SETTINGS_DIR)).unwrap();
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
/// `crates/vike-model/src/state_path.rs`'s `project_settings_dir_from`, which ignores a blank
/// override rather than honouring it; this asserts the ladder agrees.
#[test]
fn a_blank_settings_dir_is_not_a_declaration() {
    let scratch = Scratch::new("blank-declaration");
    let walked = scratch.path().join("walked");
    std::fs::create_dir_all(walked.join(crate::state_path::PROJECT_SETTINGS_DIR)).unwrap();
    let checkout =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("market_data").join("hist");

    for blank in ["", "   ", "\t", "\n"] {
        let vars = vars_of(&[(crate::state_path::SETTINGS_DIR_ENV, blank)]);
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
/// every other test here. `$VIKE_HIST_STORE` names the STORE; `$VIKE_SETTINGS_DIR` names the
/// PROJECT, and naming the store is the more specific statement.
#[test]
fn the_stated_rungs_still_outrank_a_declared_project() {
    let scratch = Scratch::new("stated-vs-declared");
    let walked = scratch.path().join("walked");
    std::fs::create_dir_all(walked.join(crate::state_path::PROJECT_SETTINGS_DIR)).unwrap();
    let declared = scratch.path().join("declared");
    let checkout =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("market_data").join("hist");
    let vars = vars_of(&[(
        crate::state_path::SETTINGS_DIR_ENV,
        declared.join(crate::state_path::PROJECT_SETTINGS_DIR).to_str().unwrap(),
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
    assert_eq!((got.root, got.rung), (PathBuf::from("/y/env"), StoreRootRung::EnvVar));
}

/// The wiring changes NO rung above 4: an explicit path and `$VIKE_HIST_STORE` still win, and a
/// dev checkout still outranks the project — proven through the same entry point the binaries
/// call, not only through the ladder underneath it.
#[test]
fn the_wiring_leaves_the_stated_and_checkout_rungs_untouched() {
    let scratch = Scratch::new("wiring-above");
    let project = scratch.path().join("proj");
    std::fs::create_dir_all(project.join(crate::state_path::PROJECT_SETTINGS_DIR)).unwrap();
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
    assert_eq!((got.root, got.rung), (PathBuf::from("/y/env"), StoreRootRung::EnvVar));

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

#[cfg(not(windows))]
#[test]
fn unix_user_dir_prefers_xdg_then_home() {
    assert_eq!(
        user_data_dir(Some("/xdg"), Some("/home/u"), None),
        Some(PathBuf::from("/xdg/vike-data"))
    );
    assert_eq!(
        user_data_dir(None, Some("/home/u"), None),
        Some(PathBuf::from("/home/u/.local/share/vike-data"))
    );
    assert_eq!(user_data_dir(None, None, None), None);
    assert_eq!(user_data_dir(Some(""), Some(""), None), None, "empty is absent");
}

/// A shaped environment map, as `std::env::vars().collect()` would produce it.
fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect()
}

/// **The behaviour-preservation gate for the map form.** The four callers that used to paste
/// three `std::env::var` lines now pass a map; this asserts the map form answers EXACTLY what
/// those three arguments answered, on a unix-shaped AND a windows-shaped environment, on
/// whichever platform the test runs. Absent, present and blank values all agree by
/// construction, because the map form only chooses the three arguments — it re-implements none
/// of the precedence.
#[test]
fn the_map_form_answers_exactly_what_the_three_argument_form_answers() {
    let cases: &[&[(&str, &str)]] = &[
        // unix-shaped: XDG set, HOME set, no LOCALAPPDATA
        &[("XDG_DATA_HOME", "/home/u/.local/share"), ("HOME", "/home/u")],
        // unix-shaped, XDG absent — the `~/.local/share` arm
        &[("HOME", "/home/u")],
        // windows-shaped: LOCALAPPDATA + HOME, no XDG
        &[("LOCALAPPDATA", "C:\\Users\\u\\AppData\\Local"), ("HOME", "C:\\Users\\u")],
        // windows-shaped, LOCALAPPDATA absent — the bare-home arm
        &[("HOME", "C:\\Users\\u")],
        // both platforms' variables present at once (MSYS/Git-Bash on Windows)
        &[
            ("XDG_DATA_HOME", "/xdg"),
            ("HOME", "/home/u"),
            ("LOCALAPPDATA", "C:\\Users\\u\\AppData\\Local"),
        ],
        // blank values must behave exactly like absent ones
        &[("XDG_DATA_HOME", ""), ("HOME", "/home/u"), ("LOCALAPPDATA", "")],
        // a scrubbed daemon environment
        &[],
    ];
    for pairs in cases {
        let vars = env(pairs);
        let want = user_data_dir(
            vars.get("XDG_DATA_HOME").map(String::as_str),
            vars.get("HOME").map(String::as_str),
            vars.get("LOCALAPPDATA").map(String::as_str),
        );
        assert_eq!(user_data_dir_from_vars(&vars), want, "diverged for {pairs:?}");
    }
}

/// …and the concrete answers are PINNED, not merely self-consistent: a refactor that changed
/// both forms together would still pass the equivalence test above. These are the paths a live
/// install resolves to today.
#[cfg(not(windows))]
#[test]
fn the_pinned_unix_answers() {
    assert_eq!(
        user_data_dir_from_vars(&env(&[("XDG_DATA_HOME", "/xdg"), ("HOME", "/home/u")])),
        Some(PathBuf::from("/xdg/vike-data"))
    );
    assert_eq!(
        user_data_dir_from_vars(&env(&[("HOME", "/home/u")])),
        Some(PathBuf::from("/home/u/.local/share/vike-data"))
    );
    assert_eq!(user_data_dir_from_vars(&env(&[])), None);
}

#[cfg(windows)]
#[test]
fn the_pinned_windows_answers() {
    assert_eq!(
        user_data_dir_from_vars(&env(&[
            ("LOCALAPPDATA", "C:\\Users\\u\\AppData\\Local"),
            ("HOME", "C:\\Users\\u"),
        ])),
        Some(PathBuf::from("C:\\Users\\u\\AppData\\Local").join("vike-data"))
    );
    assert_eq!(
        user_data_dir_from_vars(&env(&[("HOME", "C:\\Users\\u")])),
        Some(PathBuf::from("C:\\Users\\u").join("vike-data"))
    );
    assert_eq!(user_data_dir_from_vars(&env(&[])), None);
}

#[cfg(windows)]
#[test]
fn windows_user_dir_prefers_localappdata() {
    assert_eq!(
        user_data_dir(None, Some("C:\\Users\\u"), Some("C:\\Users\\u\\AppData\\Local")),
        Some(PathBuf::from("C:\\Users\\u\\AppData\\Local").join("vike-data"))
    );
    assert_eq!(
        user_data_dir(None, Some("C:\\Users\\u"), None),
        Some(PathBuf::from("C:\\Users\\u").join("vike-data"))
    );
    assert_eq!(user_data_dir(None, None, None), None);
}

/// Every reserved character is refused — the whole set, not the two that motivated it.
///
/// ⚠ Spelled as a LOOP over the const rather than as nine cases, so a character added to
/// [`PATH_HOSTILE_IN_A_SYMBOL`] is covered the moment it is added. The reverse — a case list
/// that silently stops covering a new member — is the shape this repo's ratchets exist against.
#[test]
fn every_reserved_character_is_refused_wherever_it_sits() {
    for bad in PATH_HOSTILE_IN_A_SYMBOL {
        for candidate in [format!("{bad}BTC"), format!("BT{bad}C"), format!("BTC{bad}")] {
            let err = refuse_a_path_hostile_symbol(&candidate)
                .expect_err("a reserved character must be refused wherever it sits");
            assert!(
                err.contains(&format!("{bad:?}")),
                "the refusal must NAME the offending character, or an operator cannot act on \
                     it: {err}"
            );
        }
    }
}

/// The symbols this rule exists for, and the ones it must not disturb.
///
/// The passing half is the point: this function is a fence around a defect, and a fence that
/// also refuses working symbols is worse than none. Every spelling below is one this workspace
/// stores TODAY or would store after the catalog renders it.
#[test]
fn the_real_symbols_fall_on_the_sides_they_must() {
    for refused in [
        "HYPE/USDC", // hyperliquid spot, our unified BASE/QUOTE spelling — 328 of them
        "BTC/USD",   // alpaca crypto, the venue's own wire spelling
        "xyz:TSLA",  // a hyperliquid builder-dex perp — 289 across eleven dexes
        "para:GOLD", // ...and a second dex, so the case is not one literal
    ] {
        assert!(
            refuse_a_path_hostile_symbol(refused).is_err(),
            "{refused:?} cannot be a directory name and must be refused"
        );
    }
    for allowed in [
        "BTC",                 // hyperliquid core perp — bare coin, 234 of them
        "kPEPE",               // ...including the k-prefixed ones
        "HYPE-USDC",           // the rendered spot pair: one character changed from the venue's
        "HYPE-USDT0",          // ...and its second quote, since 12 bases have more than one
        "TSLA.d-xyz",          // the rendered builder-dex perp
        "BTCUSDT",             // binance spot
        "BTCUSDT.P",           // binance perp, the suffix that already exists
        "EUR_USD",             // oanda FX — its slash is only in `displayName`
        "BTC-1JAN27-100000-C", // deribit option
        "btc-updown-5m",       // polymarket, which keys on a group rather than a symbol
    ] {
        assert_eq!(
            refuse_a_path_hostile_symbol(allowed),
            Ok(()),
            "{allowed:?} is a symbol this workspace stores; refusing it would be a regression"
        );
    }
}

/// The two failure modes are NOT interchangeable, and the message has to say which one it is —
/// a slash is a silent mis-partition on every platform, a colon is a loud Windows-only refusal.
#[test]
fn the_refusal_distinguishes_a_silent_split_from_a_windows_refusal() {
    let slash = refuse_a_path_hostile_symbol("HYPE/USDC").expect_err("a slash is refused");
    // ⚠ Matched case-INSENSITIVELY on purpose. Written as `contains("separator")` it failed on
    // its first run against a message that says `SEPARATOR` — the assertion was pinning this
    // sentence's typography rather than its content, which is the wrong thing for a test about
    // whether an operator is told the right cause.
    assert!(
        slash.to_lowercase().contains("separator"),
        "a slash must be explained as a separator: {slash}"
    );
    let colon = refuse_a_path_hostile_symbol("xyz:TSLA").expect_err("a colon is refused");
    assert!(colon.contains("Windows"), "a colon must name the platform it breaks: {colon}");
    assert_ne!(slash, colon, "the two modes must not share one message");
}
