//! **The pin that holds the GUI's node-key resolution equal to the CLI's and the daemon's.**
//!
//! # The defect that made this file necessary
//!
//! `docs/decisions/0051-node-keys-live-in-their-own-store.md` moved the two platform node keys out
//! of the venue credential store (168 venue names) into a node-key store of their own (four names).
//! Three programs consume that pair.
//!
//! | consumer | spelling | migrated |
//! |---|---|---|
//! | `vike-cli` | `crates/vike-cli/src/boot.rs`'s `node_key_store` → `vike_secrets::resolve_node_keys` | yes |
//! | `vike-tradehub` | `crates/vike-tradehub/src/node.rs`'s `start_observe_server` → the same call | yes |
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
//! The DATAHUB half went through `resolve_node_keys` in this very crate
//! (`backend_registry::datahub_observe_keys`) the whole time, so the asymmetry was an oversight
//! rather than a verdict — and an oversight is exactly what a gate is for.
//!
//! # What is pinned, and why HERE
//!
//! `crates/vike-bridge-core/tests/settings_dir_spellings.rs` is this repo's precedent for a test
//! whose only job is to hold two duplicated resolvers equal, and this is the same shape one plane
//! over. `vike-app-core` is the crate that can see BOTH spellings: the GUI's
//! ([`vike_app_core::backend::backend_registry::fill_node_keys`]) is its own, and the OTHER two are one
//! call — `vike_secrets::resolve_node_keys(dir, is_tradehub_node_key)` — which `vike-cli` and
//! `vike-tradehub` each make verbatim and which this crate depends on. The daemon's own READER,
//! `vike_tradehub_client::auth::from_vars`, is a normal dependency here too, so the comparison can
//! be made in the units that actually matter: **the key BYTES the node will verify against**.
//! Neither of those two crates can be linked from here (a `daemon`-tier binary crate and a
//! trading daemon), and pinning against the shared resolver they both call is the stronger check
//! anyway — it fails for a GUI that reads any other store, however it spells the read.
//!
//! ⚠ **What this file does NOT pin, stated because a reader will assume it does.** [`gui`] below
//! RE-ENACTS `crates/vike-desktop/src/main.rs`'s call — `resolve_project` then
//! [`vike_app_core::backend::backend_registry::fill_node_keys`] — it does not RUN the shell, which no test
//! in the CI roster can: that crate is in `xtask/src/ci/tables/roster.rs`'s `EXCLUDE_FROM_CI` and only the
//! `app-check` job compiles it. So reverting the shell's one line left every assertion here green,
//! and nothing in this file changes that.
//!
//! ⚠ That hole is covered ELSEWHERE and only in part, which is worth spelling out because the first
//! version of this paragraph read as if the shell's line were protected outright. It is not. Of the
//! three reverts, the COMPILER catches one — calling the environment-only rung
//! (`backend_registry`'s `fill_node_keys_from_env`) from the shell again is an `E0603`, because that
//! function is private — and a TEXT rule catches the other two: DELETING the `fill_node_keys` call
//! (it is `pub`, so nothing warns, and this file calls it directly and stays green) and passing
//! `None` for the settings directory. That rule is `crates/vike-ops/tests/settings_secrets/node_key_store_gate/shell_rule.rs`'s
//! `SHELL_SITES`, and it is PATH-KEYED, so a rename of `main.rs` must re-key it.
//!
//! Every path below is inside a private scratch directory. No real settings directory is read,
//! written or named, and no test here touches the process environment.
//!
//! # There is ONE place a node key lives
//!
//! The settings database's `node_key` table is the only home. What is pinned is the agreement —
//! the GUI and the CLI/daemon resolve the same BYTES out of the same STORE.

use std::assert_matches;
use std::collections::HashMap;
use std::fs;
use std::path::Path;

use vike_app_core::backend::backend_registry::{
    self, CONTROL_KEY_NAME, NodeKeyFill, NodeKeyResolution, OBSERVE_KEY_NAME,
};
use vike_secrets::Source;
use vike_tradehub_client::proto::Scope;

/// The node keys as ROWS of the settings database's `node_key` table: the store created the way a
/// fresh box gets one (`vike-cli secrets init`'s library half), the rows written through
/// the one sanctioned writer. A node key belongs to no account, so no classifier is handed in.
fn seed(dir: &Path, lines: &[(&str, &str)]) {
    vike_secrets::create_store(dir.to_str()).expect("create the empty store");
    let rows: Vec<(String, String)> =
        lines.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect();
    vike_secrets::save_credentials_to_store(dir, vike_secrets::Table::NodeKey, &rows, None)
        .expect("seed the node keys");
}

fn map(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect()
}

/// **THE GUI's spelling**, run end to end the way `vike-desktop`'s `workspace_credentials` runs it:
/// the VENUE store first, then the node-key resolution.
fn gui(
    settings: &Path,
    env: &HashMap<String, String>,
) -> (HashMap<String, String>, NodeKeyResolution) {
    let mut creds = vike_secrets::resolve_project(settings.to_str())
        .expect("the scratch venue store is readable")
        .secrets
        .into_map();
    let resolution = backend_registry::fill_node_keys(&mut creds, settings.to_str(), env);
    (creds, resolution)
}

/// **THE OTHER TWO SPELLINGS**, which are one call: `vike-cli`'s `node_key_store` and
/// `vike-tradehub`'s `start_observe_server` each make exactly this one, with the TRADEHUB family.
fn cli_and_daemon(settings: &Path) -> (HashMap<String, String>, Source) {
    let resolved = vike_secrets::resolve_node_keys(
        settings.to_str(),
        vike_model::credential_keys::is_tradehub_node_key,
    )
    .expect("the scratch node store is readable");
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
fn agree(settings: &Path, env: &HashMap<String, String>, shape: &str) -> NodeKeyResolution {
    let (gui_vars, resolution) = gui(settings, env);
    let (store_vars, source) = cli_and_daemon(settings);
    assert_eq!(
        resolution.source, source,
        "the GUI and the CLI/daemon disagree about WHICH STORE answers for the {shape} shape"
    );
    if env.is_empty() {
        assert_eq!(
            signing_pair(&gui_vars),
            signing_pair(&store_vars),
            "the GUI would sign the {shape} shape with different bytes than the daemon verifies"
        );
    }
    resolution
}

/// The scratch settings directory for one shape.
fn scratch() -> tempfile::TempDir {
    tempfile::Builder::new().prefix("vike-node-key-spellings-").tempdir().expect("scratch dir")
}

/// **THE GATE.** Over every shape a box can be in, the GUI resolves the tradehub pair to the same
/// bytes — and out of the same STORE — as `vike-cli` and `vike-tradehub` do.
#[test]
fn the_gui_resolves_a_node_key_out_of_the_same_store_as_the_cli_and_the_daemon() {
    let empty = HashMap::new();

    // --- the box with a settings DATABASE: the `node_key` table holds the pair ------------------
    let stored = scratch();
    seed(stored.path(), &[(OBSERVE_KEY_NAME, "obs-db"), (CONTROL_KEY_NAME, "ctl-db")]);
    let r = agree(stored.path(), &empty, "database");
    assert_matches!(r.source, Source::Database(_), "{:?}", r.source);
    assert!(r.notices.is_empty(), "a box with a store has nothing to be told: {:?}", r.notices);

    // --- the UNCONFIGURED box: no node key anywhere. The ordinary state, and it is SILENT -------
    let bare = scratch();
    let r = agree(bare.path(), &empty, "unconfigured");
    assert_eq!(r.source, Source::None);
    assert!(
        r.notices.is_empty(),
        "an unconfigured box is not a misconfigured one: {:?}",
        r.notices
    );

    // --- NO settings directory at all: both spellings answer "nothing", neither panics ----------
    let missing = scratch();
    let nowhere = missing.path().join("not-a-settings-dir");
    let r = agree(&nowhere, &empty, "no-settings-directory");
    assert_eq!(r.source, Source::None);
}

/// The thin-client image's rung is untouched: with no store at all, an exported observe key still
/// arms the GUI, and an exported CONTROL key still does not.
#[test]
fn the_process_environment_still_gap_fills_the_observe_key_and_never_the_control_key() {
    let container = scratch();

    let (vars, r) = gui(container.path(), &map(&[(OBSERVE_KEY_NAME, "from-the-container")]));
    assert_eq!(r.source, Source::None);
    assert_eq!(r.fill, NodeKeyFill::ObserveFromEnv);
    assert_eq!(vars.get(OBSERVE_KEY_NAME).map(String::as_str), Some("from-the-container"));

    let (vars, r) = gui(container.path(), &map(&[(CONTROL_KEY_NAME, "from-the-container")]));
    assert_eq!(r.fill, NodeKeyFill::ControlIgnored);
    assert!(
        !vars.contains_key(CONTROL_KEY_NAME),
        "a control key from the environment would let a variable on the box arm order placement"
    );
}

/// A STORED key still wins over the environment — the gap-fill never became an override.
#[test]
fn a_stored_observe_key_still_outranks_an_exported_one() {
    let exported = map(&[(OBSERVE_KEY_NAME, "from-the-environment")]);
    let stored = scratch();
    seed(stored.path(), &[(OBSERVE_KEY_NAME, "from-the-database")]);
    let (vars, r) = gui(stored.path(), &exported);
    assert_eq!(vars.get(OBSERVE_KEY_NAME).map(String::as_str), Some("from-the-database"));
    assert_eq!(r.fill, NodeKeyFill::Nothing);
}

/// The two key NAMES the GUI looks up are the ones the SERVER reads, and both are in the TRADEHUB
/// family the resolution is scoped to.
#[test]
fn the_key_names_are_the_servers_own() {
    assert_eq!(OBSERVE_KEY_NAME, vike_tradehub_client::auth::OBSERVE_KEY_ENV);
    assert_eq!(CONTROL_KEY_NAME, vike_tradehub_client::auth::CONTROL_KEY_ENV);
    assert!(vike_model::credential_keys::is_platform_key(OBSERVE_KEY_NAME));
    assert!(vike_model::credential_keys::is_platform_key(CONTROL_KEY_NAME));
    assert!(vike_model::credential_keys::is_tradehub_node_key(OBSERVE_KEY_NAME));
    assert!(vike_model::credential_keys::is_tradehub_node_key(CONTROL_KEY_NAME));
}

/// The datahub half: the GUI's datahub observe key resolves out of the same `node_key` table, a
/// CUSTOM key name included — and the scope holds: a datahub-only table carries no tradehub pair.
#[test]
fn the_datahub_key_resolves_from_the_table_and_the_scope_holds() {
    const CUSTOM: &str = "PROD2_DATAHUB_OBSERVE_KEY";
    let dir = scratch();
    seed(
        dir.path(),
        &[(vike_node_proto::auth::DATAHUB_OBSERVE_KEY_ENV, "dh-obs"), (CUSTOM, "dh-custom")],
    );

    let (keys, notices) = backend_registry::datahub_observe_keys(
        dir.path().to_str(),
        &HashMap::new(),
        backend_registry::DATAHUB_OBSERVE_KEY_NAME,
    );
    assert_eq!(
        keys.expect("the datahub key").key_for(vike_node_proto::auth::Scope::Read),
        b"dh-obs"
    );
    assert!(notices.is_empty(), "{notices:?}");

    let (keys, _) =
        backend_registry::datahub_observe_keys(dir.path().to_str(), &HashMap::new(), CUSTOM);
    assert_eq!(
        keys.expect("the custom datahub key").key_for(vike_node_proto::auth::Scope::Read),
        b"dh-custom"
    );

    let r = agree(dir.path(), &HashMap::new(), "datahub-only-table");
    let (vars, _) = gui(dir.path(), &HashMap::new());
    assert_matches!(r.source, Source::Database(_));
    assert_eq!(vars.get(OBSERVE_KEY_NAME), None, "the tradehub scope read a datahub row");
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
    let r = backend_registry::fill_node_keys(&mut creds, broken.path().to_str(), &HashMap::new());
    assert_eq!(r.source, Source::None, "no store answered");
    let said = r.notices.join("\n");
    assert!(said.contains("vike.db"), "the notice must name the store that failed: {said}");
    assert!(
        !creds.contains_key(OBSERVE_KEY_NAME),
        "an unreadable node store must REMOVE the pair rather than keep a stale copy"
    );
}
