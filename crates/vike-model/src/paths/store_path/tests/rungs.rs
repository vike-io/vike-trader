use super::*;

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
        PathBuf::from(crate::paths::state_path::PROJECT_DATA_DIR)
            .join(crate::paths::state_path::HIST_SUBDIR)
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
