//! **The pin that makes the settings-directory duplication safe.**
//!
//! `vike_model::state_path` and `vike_secrets::dotenv` each spell the project-settings resolver
//! independently, on purpose: `vike-secrets` has ZERO `vike-*` dependencies by policy (its manifest
//! carries the argument), so it cannot import `vike-model`'s copy, and `vike-model` will not take a
//! dependency to learn a directory name. Both copies' doc comments have promised "a test pins the
//! two spellings equal" since the duplication landed. **There was no such test.** It could not have
//! lived in either crate — neither can see the other — and nothing else compared them, so the two
//! were free to drift silently into two different answers to "where do my credentials live".
//!
//! `vike-bridge-core` is the only crate in the tree that depends on BOTH, which is why the pin is
//! here. It checks the two things a caller actually relies on:
//!
//! 1. the CONSTANTS agree (`settings`, `VIKE_SETTINGS_DIR`), and
//! 2. the WALKS agree — the same real directory tree, resolved through each copy, yields the same
//!    path, across every shape that distinguishes the two markers: a source checkout, a checkout
//!    seen from a crate sub-directory, a deployment with no `Cargo.toml`, a stray `settings/` above
//!    and below a checkout root, a project nested under an unrelated `[package]` manifest (the
//!    hijack), a chain with no `[workspace]` table at all, a nested workspace, an unreadable
//!    manifest, and no marker at all.
//!
//! (2) is the half that matters. Equal constants and divergent walks is the failure that hurts, and
//! it is exactly what a constants-only pin would miss.
//!
//! Nothing here reads, writes or names the real credential store: every path is inside a private
//! scratch directory under the system temp dir, and no file is ever created with credential-shaped
//! content.

use std::fs;
use std::path::{Path, PathBuf};

use vike_model::state_path;

/// A private scratch directory, removed on drop. Hand-rolled rather than `tempfile::tempdir()`
/// because `vike-bridge-core` carries no `tempfile` dev-dependency and this pin is not worth adding
/// one — the same shape `tests/credential_chain_roots.rs` already uses next door.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let d = std::env::temp_dir().join(format!(
            "vike-settings-spellings-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).expect("scratch dir");
        Self(d)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Both copies' answer for one start directory, as a pair to compare.
fn both(start: &Path) -> (Option<PathBuf>, Option<PathBuf>) {
    (state_path::project_settings_dir(start), vike_secrets::project_settings_dir(start))
}

/// Assert the two copies agree, and (when given) that they agree on the EXPECTED answer.
fn agree(start: &Path, want: Option<&Path>, shape: &str) {
    let (model, secrets) = both(start);
    assert_eq!(
        model, secrets,
        "the two settings-dir walks disagree for the {shape} shape at {start:?}: \
         vike-model says {model:?}, vike-secrets says {secrets:?}"
    );
    assert_eq!(model.as_deref(), want, "the {shape} shape resolved to the wrong directory");
}

/// The directory NAME and the override VARIABLE must be spelled identically in both crates.
#[test]
fn the_two_copies_spell_the_same_constants() {
    assert_eq!(state_path::PROJECT_SETTINGS_DIR, vike_secrets::SETTINGS_DIR);
    assert_eq!(state_path::SETTINGS_DIR_ENV, vike_secrets::SETTINGS_DIR_ENV);
    assert_eq!(state_path::STATE_SUBDIR, vike_secrets::STATE_DIR);
    // …and they are the values every doc comment, deploy unit and runbook names.
    assert_eq!(state_path::PROJECT_SETTINGS_DIR, "settings");
    assert_eq!(state_path::SETTINGS_DIR_ENV, "VIKE_SETTINGS_DIR");
    assert_eq!(state_path::STATE_SUBDIR, "state");
}

/// The two walks agree on every shape that can distinguish the two markers.
#[test]
fn the_two_copies_walk_identically() {
    let scratch = Scratch::new("walks");
    let base = scratch.path();

    // --- a SOURCE CHECKOUT: the `[workspace]` root wins, from any depth --------------------------
    let root = base.join("checkout");
    let crate_dir = root.join("crates").join("bridges").join("aster");
    fs::create_dir_all(crate_dir.join("src")).unwrap();
    fs::write(root.join("Cargo.toml"), "[workspace]\n").unwrap();
    fs::write(crate_dir.join("Cargo.toml"), "[package]\nname=\"a\"\n").unwrap();
    let checkout_settings = root.join("settings");
    for from in [&root, &crate_dir, &crate_dir.join("src")] {
        agree(from, Some(checkout_settings.as_path()), "source-checkout");
    }

    // …and a `settings/` at the CRATE level does not capture it in either copy.
    fs::create_dir_all(crate_dir.join("settings")).unwrap();
    agree(&crate_dir.join("src"), Some(checkout_settings.as_path()), "checkout-with-inner-stray");

    // --- a DEPLOYMENT: no Cargo.toml anywhere, `settings/` is the marker -------------------------
    let opt_vike = base.join("opt").join("vike");
    fs::create_dir_all(opt_vike.join("bin")).unwrap();
    fs::create_dir_all(opt_vike.join("settings")).unwrap();
    let deployed = opt_vike.join("settings");
    agree(&opt_vike, Some(deployed.as_path()), "deployment");
    agree(&opt_vike.join("bin"), Some(deployed.as_path()), "deployment-subdir");

    // --- THE DEPLOYMENT HIJACK: an unrelated manifest ABOVE a deployment must not capture it -----
    // A `settings/` beside a binary with no `Cargo.toml` at that level, under one stray `[package]`
    // manifest. The chain declares no `[workspace]` anywhere, so the manifest arm used to fall back
    // to the NEAREST manifest — still above the deployment — and the deployment's own populated
    // settings/ vanished. With a stray `settings/` there too, the deployment read the STRANGER'S.
    let dh = base.join("deployhijack");
    let dh_deploy = dh.join("opt-vike");
    fs::create_dir_all(dh_deploy.join("bin")).unwrap();
    fs::create_dir_all(dh_deploy.join("settings")).unwrap();
    fs::write(dh.join("Cargo.toml"), "[package]\nname=\"unrelated\"\n").unwrap();
    for from in [&dh_deploy, &dh_deploy.join("bin")] {
        agree(
            from,
            Some(dh_deploy.join("settings").as_path()),
            "deployment-under-a-stray-manifest",
        );
    }
    fs::create_dir_all(dh.join("settings")).unwrap();
    agree(&dh_deploy, Some(dh_deploy.join("settings").as_path()), "deployment-vs-stray-settings");

    // …and the mirror image, which the cure must NOT break: a stray `settings/` ABOVE a
    // plain-package project loses to the project's own, NEARER, manifest.
    let over = base.join("overreach");
    let over_proj = over.join("proj");
    fs::create_dir_all(over_proj.join("src")).unwrap();
    fs::create_dir_all(over.join("settings")).unwrap();
    fs::write(over.join("Cargo.toml"), "[package]\nname=\"unrelated\"\n").unwrap();
    fs::write(over_proj.join("Cargo.toml"), "[package]\nname=\"proj\"\n").unwrap();
    agree(
        &over_proj.join("src"),
        Some(over_proj.join("settings").as_path()),
        "stray-settings-above",
    );

    // --- THE HIJACK: an unrelated manifest above a project must not capture it -------------------
    // A stray `[package]` manifest one level up used to win outright, so a project's own populated
    // settings/ vanished — or worse, the project silently read the stranger's. Both copies must
    // stop at the project's own `[workspace]` root, from any depth.
    let stray = base.join("stray");
    fs::create_dir_all(stray.join("settings")).unwrap();
    fs::write(stray.join("Cargo.toml"), "[package]\nname=\"unrelated\"\n").unwrap();
    let nested = stray.join("proj");
    fs::create_dir_all(nested.join("src")).unwrap();
    fs::write(nested.join("Cargo.toml"), "[workspace]\nmembers = []\n").unwrap();
    fs::create_dir_all(nested.join("settings")).unwrap();
    let nested_settings = nested.join("settings");
    for from in [&nested, &nested.join("src")] {
        agree(from, Some(nested_settings.as_path()), "nested-under-a-stray-manifest");
    }

    // …and with NO `[workspace]` table anywhere, the NEAREST manifest is the project.
    let plain = base.join("plain");
    let plain_proj = plain.join("proj");
    fs::create_dir_all(plain_proj.join("src")).unwrap();
    fs::write(plain.join("Cargo.toml"), "[package]\nname=\"unrelated\"\n").unwrap();
    fs::write(plain_proj.join("Cargo.toml"), "[package]\nname=\"proj\"\n").unwrap();
    agree(&plain_proj.join("src"), Some(plain_proj.join("settings").as_path()), "plain-package");

    // …while a NESTED workspace still resolves to the OUTERMOST one (the protogen shape).
    let nest_ws = base.join("nestws");
    let inner_ws = nest_ws.join("crates").join("protogen");
    fs::create_dir_all(&inner_ws).unwrap();
    fs::write(nest_ws.join("Cargo.toml"), "[workspace]\nmembers = []\n").unwrap();
    fs::write(inner_ws.join("Cargo.toml"), "[package]\nname=\"p\"\n[workspace]\n").unwrap();
    agree(&inner_ws, Some(nest_ws.join("settings").as_path()), "nested-workspace");

    // …and an UNREADABLE manifest degrades to the shipped outermost rule in BOTH copies.
    let unreadable = base.join("unreadable");
    let unreadable_proj = unreadable.join("proj");
    fs::create_dir_all(&unreadable_proj).unwrap();
    fs::write(unreadable.join("Cargo.toml"), [0xff_u8, 0xfe, 0x00, 0x80]).unwrap();
    fs::write(unreadable_proj.join("Cargo.toml"), "[package]\nname=\"proj\"\n").unwrap();
    agree(&unreadable_proj, Some(unreadable.join("settings").as_path()), "unreadable-manifest");
    // …and it still decides ALONE with a `settings/` at the deeper level — letting that compete is
    // #1089 (the aster crate directory holds both markers).
    fs::create_dir_all(unreadable_proj.join("settings")).unwrap();
    agree(&unreadable_proj, Some(unreadable.join("settings").as_path()), "unreadable-vs-settings");

    // --- a `settings` FILE is not the marker in either copy --------------------------------------
    let filey = base.join("filey");
    fs::create_dir_all(&filey).unwrap();
    fs::write(filey.join("settings"), "not a directory").unwrap();
    agree(&filey, None, "settings-is-a-file");

    // --- NO marker at all ⇒ both refuse ----------------------------------------------------------
    // ⚠ Assumes TMPDIR's own ancestry holds neither marker; a failure here means a stray was
    // created above it, not that a walk regressed.
    let bare = base.join("bare").join("deep");
    fs::create_dir_all(&bare).unwrap();
    agree(&bare, None, "no-marker");
}

/// The OVERRIDE behaves identically in both copies: it wins over the walk, and blank falls through.
#[test]
fn the_two_copies_honour_the_override_identically() {
    let scratch = Scratch::new("override");
    let root = scratch.path();
    fs::write(root.join("Cargo.toml"), "[workspace]\n").unwrap();
    let elsewhere = root.join("elsewhere");

    let explicit = elsewhere.to_str();
    assert_eq!(
        state_path::project_settings_dir_from(explicit, root),
        vike_secrets::project_settings_dir_from(explicit, root),
    );
    assert_eq!(
        state_path::project_settings_dir_from(explicit, root).as_deref(),
        Some(elsewhere.as_path()),
    );

    for blank in [None, Some(""), Some("   ")] {
        assert_eq!(
            state_path::project_settings_dir_from(blank, root),
            vike_secrets::project_settings_dir_from(blank, root),
            "a blank override must fall through to the walk in BOTH copies",
        );
        assert_eq!(
            state_path::project_settings_dir_from(blank, root).as_deref(),
            Some(root.join("settings").as_path()),
        );
    }
}

/// The credential FILE the project slot resolves to is the settings dir's `secrets.env` — the one
/// place the two crates' answers are actually consumed together.
#[test]
fn the_project_credential_path_sits_inside_the_agreed_settings_dir() {
    let scratch = Scratch::new("credpath");
    let opt_vike = scratch.path().join("opt").join("vike");
    fs::create_dir_all(opt_vike.join("settings")).unwrap();

    let dir = state_path::project_settings_dir(&opt_vike).expect("the deployment shape resolves");
    let file = vike_secrets::project_secrets_path(&opt_vike).expect("…and so does its store");
    assert_eq!(file, dir.join(vike_secrets::SECRETS_FILE));
    assert_eq!(file, opt_vike.join("settings").join("secrets.env"));
}
