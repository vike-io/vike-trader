//! **The pin that holds the GUI's hub-key resolution equal to the CLI's and the daemon's.**
//!
//! # The defect that made this file necessary
//!
//! `docs/decisions/0051-node-keys-live-in-their-own-store.md` moved the two platform hub keys out
//! of the venue credential store (168 venue names) into a hub-key store of their own (four names).
//! Three programs consume that pair.
//!
//! | consumer | spelling | migrated |
//! |---|---|---|
//! | `vike-cli` | `crates/vike-cli/src/boot.rs`'s `hub_key_store` → `vike_secrets::CredentialStore::hub_keys` | yes |
//! | `vike-tradehub` | `crates/vike-tradehub/src/hub_server.rs`'s `start_observe_server` → the same call | yes |
//! | `vike-desktop` | `workspace_credentials` → `vike_app_core::backend::backend_registry::resolve_keys` | **no** |
//!
//! The desktop looked `VIKE_TRADEHUB_OBSERVE_KEY` up in the map it had built from the VENUE store,
//! so an operator who followed the decision and moved the pair kept a working CLI and got, from the
//! GUI, on every reconnect:
//!
//! ```text
//! ERROR vike_app_core::backend::backend_conn: no observe key: VIKE_TRADEHUB_OBSERVE_KEY is absent from the
//!       credentials map …
//! WARN  vike_app_core::backend::observe_bridge: observe: reconnect failed … tradehub observe auth denied: bad mac
//! ```
//!
//! …and the only cure was to put the pair BACK, leaving it duplicated across two stores with a
//! rotation that has to touch both. **Nothing asserted that the two binaries read the same store**,
//! which is how a decision record shipped with one of its consumers unmigrated and stayed that way.
//! The DATAHUB half went through the hub-key store read in this very crate
//! (`backend_registry::datahub_observe_keys`) the whole time, so the asymmetry was an oversight
//! rather than a verdict — and an oversight is exactly what a gate is for.
//!
//! # What is pinned, and why HERE
//!
//! `crates/vike-bridge-core/tests/settings_dir_spellings.rs` is this repo's precedent for a test
//! whose only job is to hold two duplicated resolvers equal, and this is the same shape one plane
//! over. `vike-app-core` is the crate that can see BOTH spellings: the GUI's
//! ([`vike_app_core::backend::backend_registry::fill_hub_keys`]) is its own, and the OTHER two are one
//! call — `vike_secrets::CredentialStore::hub_keys(is_tradehub_pair_key)` — which `vike-cli` and
//! `vike-tradehub` each make verbatim and which this crate depends on. The daemon's own READER,
//! `vike_tradehub_client::auth::from_vars`, is a normal dependency here too, so the comparison can
//! be made in the units that actually matter: **the key BYTES the node will verify against**.
//! Neither of those two crates can be linked from here (a `daemon`-tier binary crate and a
//! trading daemon), and pinning against the shared resolver they both call is the stronger check
//! anyway — it fails for a GUI that reads any other store, however it spells the read.
//!
//! ⚠ **What this file does NOT pin, stated because a reader will assume it does.** [`gui`] below
//! RE-ENACTS `crates/vike-desktop/src/main.rs`'s call — `vike_secrets::CredentialStore::read_all` then
//! [`vike_app_core::backend::backend_registry::fill_hub_keys`] — it does not RUN the shell, which no test
//! in the CI roster can: that crate is in `xtask/src/ci/tables/roster.rs`'s `EXCLUDE_FROM_CI` and only the
//! `app-check` job compiles it. So reverting the shell's one line left every assertion here green,
//! and nothing in this file changes that.
//!
//! ⚠ That hole is covered ELSEWHERE and only in part, which is worth spelling out because the first
//! version of this paragraph read as if the shell's line were protected outright. It is not. A TEXT
//! rule catches the reverts that still compile: DELETING the `fill_hub_keys` call (it is `pub`, so
//! nothing warns, and this file calls it directly and stays green) and passing `None` for the
//! settings directory. That rule is `crates/vike-ops/tests/settings_secrets/hub_key_store_gate/shell_rule.rs`'s
//! `SHELL_SITES`, and it is PATH-KEYED, so a rename of `main.rs` must re-key it. (An environment
//! rung is not a revert that compiles any more: `fill_hub_keys` takes no environment map.)
//!
//! Every path below is inside a private scratch directory. No real settings directory is read,
//! written or named, and no test here touches the process environment.
//!
//! # There is ONE place a hub key lives
//!
//! The settings database's `hub_key` table is the only home. What is pinned is the agreement —
//! the GUI and the CLI/daemon resolve the same BYTES out of the same STORE.

use std::assert_matches;
use std::collections::HashMap;
use std::fs;
use std::path::Path;

use vike_app_core::backend::backend_registry::{
    self, CONTROL_KEY_NAME, HubKeyResolution, OBSERVE_KEY_NAME,
};
use vike_app_core::data::catalog_wire::CatalogDial;
use vike_app_core::data::chart_seed::SeedDial;
use vike_hub_proto::auth::Scope;
use vike_secrets::Source;

/// The hub keys as ROWS of the settings database's `hub_key` table: the store created the way a
/// fresh box gets one (`vike-cli secrets init`'s library half), the rows written through
/// the record-less test door (a scratch store keeps no journal). A hub key belongs to no account,
/// so no classifier is handed in.
fn seed(dir: &Path, lines: &[(&str, &str)]) {
    vike_secrets::create_store(dir.to_str()).expect("create the empty store");
    let rows: Vec<(String, String)> =
        lines.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect();
    vike_secrets::save_credentials_to_store_unjournalled(
        dir,
        vike_secrets::Table::HubKey,
        &rows,
        vike_secrets::UpsertIntent::Add,
    )
    .expect("seed the hub keys");
}

/// **THE GUI's spelling**, run end to end the way `vike-desktop`'s `workspace_credentials` runs it:
/// the VENUE store first, then the hub-key resolution.
fn gui(settings: &Path) -> (HashMap<String, String>, HubKeyResolution) {
    let mut creds = vike_secrets::CredentialStore::open(
        Some(vike_secrets::StoreLocation::new(settings)),
        vike_secrets::CredentialScope::Full,
    )
    .read_all()
    .expect("the scratch venue store is readable")
    .secrets
    .into_map();
    let resolution = backend_registry::fill_hub_keys(&mut creds, settings.to_str());
    (creds, resolution)
}

/// **THE OTHER TWO SPELLINGS**, which are one call: `vike-cli`'s `hub_key_store` and
/// `vike-tradehub`'s `start_observe_server` each make exactly this one, with the TRADEHUB family.
fn cli_and_daemon(settings: &Path) -> (HashMap<String, String>, Source) {
    let resolved = vike_secrets::CredentialStore::open(
        Some(vike_secrets::StoreLocation::new(settings)),
        vike_secrets::CredentialScope::Full,
    )
    .hub_keys(vike_venues::credential_keys::is_tradehub_pair_key)
    .expect("the scratch hub key store is readable");
    (resolved.secrets.into_map(), resolved.source)
}

/// The pair as a `(observe, control)` of `Option<Vec<u8>>`, folded through the DAEMON's own reader
/// — so what is compared is what the node verifies, not what a map happens to hold.
fn signing_pair(vars: &HashMap<String, String>) -> (Option<Vec<u8>>, Option<Vec<u8>>) {
    let Some(keys) = vike_tradehub_client::auth::from_vars(vars) else {
        return (None, None);
    };
    let observe = keys.has(Scope::Read).then(|| keys.key_for(Scope::Read).to_vec());
    let control = keys.has(Scope::Write).then(|| keys.key_for(Scope::Write).to_vec());
    (observe, control)
}

/// Assert the GUI and the CLI/daemon spelling agree on the bytes AND on which store answered, for
/// one planted shape.
fn agree(settings: &Path, shape: &str) -> HubKeyResolution {
    let (gui_vars, resolution) = gui(settings);
    let (store_vars, source) = cli_and_daemon(settings);
    assert_eq!(
        resolution.source, source,
        "the GUI and the CLI/daemon disagree about WHICH STORE answers for the {shape} shape"
    );
    assert_eq!(
        signing_pair(&gui_vars),
        signing_pair(&store_vars),
        "the GUI would sign the {shape} shape with different bytes than the daemon verifies"
    );
    resolution
}

/// The scratch settings directory for one shape.
fn scratch() -> tempfile::TempDir {
    tempfile::Builder::new().prefix("vike-hub-key-spellings-").tempdir().expect("scratch dir")
}

/// **THE GATE.** Over every shape a box can be in, the GUI resolves the tradehub pair to the same
/// bytes — and out of the same STORE — as `vike-cli` and `vike-tradehub` do.
#[test]
fn the_gui_resolves_a_hub_key_out_of_the_same_store_as_the_cli_and_the_daemon() {
    // --- the box with a settings DATABASE: the `hub_key` table holds the pair ------------------
    let stored = scratch();
    seed(stored.path(), &[(OBSERVE_KEY_NAME, "obs-db"), (CONTROL_KEY_NAME, "ctl-db")]);
    let r = agree(stored.path(), "database");
    assert_matches!(r.source, Source::Database(_), "{:?}", r.source);
    assert!(r.notices.is_empty(), "a box with a store has nothing to be told: {:?}", r.notices);

    // --- the UNCONFIGURED box: no hub key anywhere. The ordinary state, and it is SILENT -------
    let bare = scratch();
    let r = agree(bare.path(), "unconfigured");
    assert_eq!(r.source, Source::None);
    assert!(
        r.notices.is_empty(),
        "an unconfigured box is not a misconfigured one: {:?}",
        r.notices
    );

    // --- NO settings directory at all: both spellings answer "nothing", neither panics ----------
    let missing = scratch();
    let nowhere = missing.path().join("not-a-settings-dir");
    let r = agree(&nowhere, "no-settings-directory");
    assert_eq!(r.source, Source::None);
}

/// **The thin-client container's route**: a mounted settings directory holding the `hub_key` row is
/// what arms the GUI's observe key (there is no environment rung any more), and a store holding
/// ONLY the observe key arms the observe half and nothing else — a viewer that cannot trade.
#[test]
fn a_mounted_store_with_only_the_observe_key_arms_the_observe_half_alone() {
    let mounted = scratch();
    seed(mounted.path(), &[(OBSERVE_KEY_NAME, "from-the-mounted-store")]);

    let (vars, r) = gui(mounted.path());
    assert_matches!(r.source, Source::Database(_), "{:?}", r.source);
    assert_eq!(vars.get(OBSERVE_KEY_NAME).map(String::as_str), Some("from-the-mounted-store"));
    assert!(!vars.contains_key(CONTROL_KEY_NAME), "no control key was filed, so none is held");
}

/// With no store at all (a container started without the mount) the GUI holds NO observe key — it
/// does not invent one — and the pair the venue store carried is taken out rather than kept.
#[test]
fn without_a_store_there_is_no_observe_key() {
    let container = scratch();
    let (vars, r) = gui(container.path());
    assert_eq!(r.source, Source::None);
    assert!(!vars.contains_key(OBSERVE_KEY_NAME) && !vars.contains_key(CONTROL_KEY_NAME));
}

/// The two key NAMES the GUI looks up are the ones the SERVER reads, and both are in the TRADEHUB
/// family the resolution is scoped to.
#[test]
fn the_key_names_are_the_servers_own() {
    assert_eq!(OBSERVE_KEY_NAME, vike_tradehub_client::auth::OBSERVE_KEY_ENV);
    assert_eq!(CONTROL_KEY_NAME, vike_tradehub_client::auth::CONTROL_KEY_ENV);
    assert!(vike_venues::credential_keys::is_platform_key(OBSERVE_KEY_NAME));
    assert!(vike_venues::credential_keys::is_platform_key(CONTROL_KEY_NAME));
    assert!(vike_venues::credential_keys::is_tradehub_pair_key(OBSERVE_KEY_NAME));
    assert!(vike_venues::credential_keys::is_tradehub_pair_key(CONTROL_KEY_NAME));
}

/// The datahub half: the GUI's datahub observe key resolves out of the same `hub_key` table, a
/// CUSTOM key name included — and the scope holds: a datahub-only table carries no tradehub pair.
#[test]
fn the_datahub_key_resolves_from_the_table_and_the_scope_holds() {
    const CUSTOM: &str = "PROD2_DATAHUB_OBSERVE_KEY";
    let dir = scratch();
    seed(
        dir.path(),
        &[(vike_hub_proto::auth::DATAHUB_OBSERVE_KEY_ENV, "dh-obs"), (CUSTOM, "dh-custom")],
    );

    let (keys, notices) = backend_registry::datahub_observe_keys(
        dir.path().to_str(),
        backend_registry::DATAHUB_OBSERVE_KEY_NAME,
    );
    assert_eq!(
        keys.expect("the datahub key").key_for(vike_hub_proto::auth::Scope::Read),
        b"dh-obs"
    );
    assert!(notices.is_empty(), "{notices:?}");

    let (keys, _) = backend_registry::datahub_observe_keys(dir.path().to_str(), CUSTOM);
    assert_eq!(
        keys.expect("the custom datahub key").key_for(vike_hub_proto::auth::Scope::Read),
        b"dh-custom"
    );

    let r = agree(dir.path(), "datahub-only-table");
    let (vars, _) = gui(dir.path());
    assert_matches!(r.source, Source::Database(_));
    assert_eq!(vars.get(OBSERVE_KEY_NAME), None, "the tradehub scope read a datahub row");
}

/// The `hub_key` table is the one source of the datahub observe key: a stored key resolves, and a
/// blank row or an absent one is not a key (an empty key would sign a mac the server refuses and
/// replace "no key, here is why" with `bad mac` on a reconnect loop).
#[test]
fn the_hub_key_store_supplies_the_datahub_observe_key_and_a_blank_row_is_none() {
    let name = backend_registry::DATAHUB_OBSERVE_KEY_NAME;
    let dir = scratch();
    seed(dir.path(), &[(name, "sekrit")]);
    let (keys, notices) = backend_registry::datahub_observe_keys(dir.path().to_str(), name);
    assert_eq!(keys.expect("a stored key resolves").key_for(Scope::Read), b"sekrit");
    assert!(notices.is_empty(), "a readable store produces no notice: {notices:?}");

    for blank in ["", "   ", "\t"] {
        let dir = scratch();
        seed(dir.path(), &[(name, blank)]);
        let (keys, _) = backend_registry::datahub_observe_keys(dir.path().to_str(), name);
        assert!(keys.is_none(), "{blank:?} was treated as a key");
    }

    let empty = scratch();
    vike_secrets::create_store(empty.path().to_str()).expect("create the empty store");
    let (keys, notices) = backend_registry::datahub_observe_keys(empty.path().to_str(), name);
    assert!(keys.is_none(), "an empty store holds no key");
    assert!(notices.is_empty(), "an unconfigured box is silent: {notices:?}");
}

/// ⚠ The record's own override NAME is what is looked up, not the platform one — otherwise a
/// per-backend key would silently resolve to whatever the default name happened to hold.
#[test]
fn a_custom_key_name_is_the_one_looked_up() {
    let dir = scratch();
    seed(
        dir.path(),
        &[
            ("PROD2_DATAHUB_KEY", "the-right-one"),
            (backend_registry::DATAHUB_OBSERVE_KEY_NAME, "the-wrong-one"),
        ],
    );
    let (keys, _) =
        backend_registry::datahub_observe_keys(dir.path().to_str(), "PROD2_DATAHUB_KEY");
    assert_eq!(
        keys.expect("the override name resolves").key_for(Scope::Read),
        b"the-right-one",
        "the platform name won over the record's own override"
    );
}

/// ⚠ **No datahub dial resolved in this crate ever holds a Write key, whatever the store holds** —
/// the store, the chart seed, the venue catalog, the market-data session and Studio's named run
/// all resolve through [`backend_registry::datahub_observe_keys`], and its Write half is empty by
/// construction (the map handed to `hub_keys_from_vars` carries exactly one entry).
///
/// ⚠ This test was `the_datahub_control_key_is_never_resolved_here` until 2026-09-26, and the
/// NAME stopped being true rather than the assertion: the owner ruled
/// (`docs/decisions/0083-the-runtime-plugin-join-lands.md`, question 1, option (a)) that the
/// desktop DOES resolve the datahub Control key — for Studio's COMPUTE dial alone, through
/// `vike_studio::backtest_key_from_vars`, into a type none of these dials accepts. What remains
/// true, and is asserted here, is the narrower rule: THIS crate's dials never see it. So the store
/// below carries the full datahub PAIR (the control key included), and every resolver that feeds a
/// dial is asked, including the two dial constructors that wrap it, so a future dial that stopped
/// going through the observe-only resolver would have to leave this test to do it.
#[test]
fn no_datahub_dial_here_resolves_a_write_key_whatever_the_store_holds() {
    let dir = scratch();
    seed(
        dir.path(),
        &[
            (backend_registry::DATAHUB_OBSERVE_KEY_NAME, "obs"),
            (vike_hub_proto::auth::DATAHUB_CONTROL_KEY_ENV, "ctl"),
        ],
    );
    let settings = dir.path().to_str();
    let name = backend_registry::DATAHUB_OBSERVE_KEY_NAME;

    let keys =
        backend_registry::datahub_observe_keys(settings, name).0.expect("the observe key resolves");
    assert!(
        keys.key_for(Scope::Write).is_empty(),
        "a datahub CONTROL key reached a desktop datahub dial — that scope compiles \
         client-supplied Rhai and, on the data plane, backfills and deletes"
    );

    let seed_dial = SeedDial::resolve(
        "127.0.0.1:1".to_string(),
        settings,
        name,
        std::sync::Arc::new(std::sync::Mutex::new(None)),
    );
    let catalog_dial = CatalogDial::resolve("127.0.0.1:1".to_string(), settings, name);
    for (dial, keys) in
        [("the chart seed", seed_dial.keys), ("the venue catalog", catalog_dial.keys)]
    {
        let keys = keys.unwrap_or_else(|| panic!("{dial}: the observe key resolves"));
        assert!(keys.key_for(Scope::Write).is_empty(), "{dial} holds a datahub Write key");
    }
}

/// ⚠ **An unreadable store names the store that failed, and removes the pair** rather than degrading
/// to anything else. Bytes that are not a database where the database belongs: present, and never
/// openable, on every platform.
#[test]
fn the_unreadable_store_notice_names_the_store_and_removes_the_pair() {
    let broken = scratch();
    fs::create_dir_all(broken.path().join("db")).expect("db dir");
    fs::write(broken.path().join("db").join("vike.db"), b"not a sqlite database, and not empty")
        .expect("plant");

    let mut creds = HashMap::new();
    creds.insert(OBSERVE_KEY_NAME.to_string(), "obs-stale".to_string());
    let r = backend_registry::fill_hub_keys(&mut creds, broken.path().to_str());
    assert_eq!(r.source, Source::None, "no store answered");
    let said = r.notices.join("\n");
    assert!(said.contains("vike.db"), "the notice must name the store that failed: {said}");
    assert!(
        !creds.contains_key(OBSERVE_KEY_NAME),
        "an unreadable hub key store must REMOVE the pair rather than keep a stale copy"
    );
}
