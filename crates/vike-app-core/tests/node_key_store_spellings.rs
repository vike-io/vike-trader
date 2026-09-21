//! **The pin that holds the GUI's node-key resolution equal to the CLI's and the daemon's.**
//!
//! # The defect that made this file necessary
//!
//! `docs/decisions/0051-node-keys-live-in-their-own-store.md` moved the two platform node keys out
//! of the venue credential store (`<project>/settings/secrets.env`, 168 venue names) into
//! `<project>/settings/node.env` (four names). Three programs consume that pair.
//!
//! | consumer | spelling | migrated |
//! |---|---|---|
//! | `vike-cli` | `crates/vike-cli/src/lib.rs`'s `node_key_store` → `vike_secrets::resolve_node_keys` | yes |
//! | `vike-tradehub` | `crates/vike-tradehub/src/tradehub_cli.rs`'s `start_observe_server` → the same call | yes |
//! | `vike-desktop` | `workspace_credentials` → `vike_app_core::backend_registry::resolve_keys` | **no** |
//!
//! The desktop looked `VIKE_TRADEHUB_OBSERVE_KEY` up in the map it had built from the VENUE store,
//! so an operator who followed the decision and moved the pair kept a working CLI and got, from the
//! GUI, on every reconnect:
//!
//! ```text
//! ERROR vike_app_core::backend_conn: no observe key: VIKE_TRADEHUB_OBSERVE_KEY is absent from the
//!       credentials map … The key is read from the credential store (<project>/settings/secrets.env)
//! WARN  vike_app_core::observe_bridge: observe: reconnect failed … tradehub observe auth denied: bad mac
//! ```
//!
//! …and the only cure was to put the pair BACK, leaving it duplicated across two files with a
//! rotation that has to touch both. **Nothing asserted that the two binaries read the same file**,
//! which is how a decision record shipped with one of its consumers unmigrated and stayed that way.
//! The DATAHUB half of the same pair of files went through `resolve_node_keys` in this very crate
//! (`backend_registry::datahub_observe_keys`) the whole time, so the asymmetry was an oversight
//! rather than a verdict — and an oversight is exactly what a gate is for.
//!
//! # What is pinned, and why HERE
//!
//! `crates/vike-bridge-core/tests/settings_dir_spellings.rs` is this repo's precedent for a test
//! whose only job is to hold two duplicated resolvers equal, and this is the same shape one plane
//! over. `vike-app-core` is the crate that can see BOTH spellings: the GUI's
//! ([`vike_app_core::backend_registry::fill_node_keys`]) is its own, and the OTHER two are one
//! call — `vike_secrets::resolve_node_keys(dir, is_tradehub_node_key)` — which `vike-cli` and
//! `vike-tradehub` each make verbatim and which this crate depends on. The daemon's own READER,
//! `vike_tradehub_client::auth::from_vars`, is a normal dependency here too, so the comparison can
//! be made in the units that actually matter: **the key BYTES the node will verify against**.
//! Neither of those two crates can be linked from here (a `daemon`-tier binary crate and a
//! trading daemon), and pinning against the shared resolver they both call is the stronger check
//! anyway — it fails for a GUI that reads any other file, however it spells the read.
//!
//! ⚠ **What this file does NOT pin, stated because a reader will assume it does.** [`gui`] below
//! RE-ENACTS `crates/vike-desktop/src/main.rs`'s call — `resolve_project` then
//! [`vike_app_core::backend_registry::fill_node_keys`] — it does not RUN the shell, which no test
//! in the CI roster can: that crate is in `xtask/src/ci/tables.rs`'s `EXCLUDE_FROM_CI` and only the
//! `app-check` job compiles it. So reverting the shell's one line left every assertion here green,
//! and nothing in this file changes that.
//!
//! ⚠ That hole is covered ELSEWHERE and only in part, which is worth spelling out because the first
//! version of this paragraph read as if the shell's line were protected outright. It is not. Of the
//! three reverts, the COMPILER catches one — calling the environment-only rung
//! (`backend_registry`'s `fill_node_keys_from_env`) from the shell again is an `E0603`, because that
//! function is private — and a TEXT rule catches the other two: DELETING the `fill_node_keys` call
//! (it is `pub`, so nothing warns, and this file calls it directly and stays green) and passing
//! `None` for the settings directory. That rule is `crates/vike-ops/tests/node_key_store_gate.rs`'s
//! `SHELL_SITES`, and it is PATH-KEYED, so a rename of `main.rs` must re-key it.
//!
//! Every path below is inside a private scratch directory. No file in any real settings directory
//! is read, written or named, and no test here touches the process environment.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use vike_app_core::backend_registry::{
    self, CONTROL_KEY_NAME, NodeKeyFill, NodeKeyResolution, OBSERVE_KEY_NAME,
};
use vike_secrets::NodeKeySource;
use vike_tradehub_client::proto::Scope;

/// Write one `.env`-shaped file into the scratch settings directory.
fn plant(dir: &Path, file: &str, lines: &[(&str, &str)]) {
    let path = dir.join(file);
    let body: String = lines.iter().map(|(k, v)| format!("{k}={v}\n")).collect();
    fs::write(&path, body).expect("plant a scratch store");
    restrict(&path);
}

/// 0600 on the planted files.
///
/// ⚠ Not tidiness: `vike_secrets`' own permission FINDING fires on `mode & 0o077`, and the Linux
/// runner's default umask writes 0644 — so without this every assertion about the NOTICES below
/// would be reading a permission warning about a temp file rather than the migration sentence it
/// means to be about.
#[cfg(unix)]
fn restrict(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).expect("chmod 600");
}

#[cfg(not(unix))]
fn restrict(_path: &Path) {}

fn map(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect()
}

/// **THE GUI's spelling**, run end to end the way `vike-desktop`'s `workspace_credentials` runs it:
/// the VENUE store first (which is what `load_workspace_secrets_from_env` resolves to — that
/// wrapper's own store read is `vike_secrets::resolve_project`), then the node-key resolution.
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
/// `vike-tradehub`'s `start_observe_server` each make exactly this one and use its map directly.
///
/// ⚠ The predicate is `is_tradehub_node_key`, and it is what those two pass — verbatim — since the
/// family split. It was the four-name `is_platform_key` in all three spellings, which is what made
/// [`a_datahub_only_node_env_does_not_strip_the_tradehub_pair`] fail everywhere at once; narrowing
/// only the GUI would have left this reference disagreeing with the very binaries it stands for.
fn cli_and_daemon(settings: &Path) -> (HashMap<String, String>, NodeKeySource) {
    let (resolved, source) = vike_secrets::resolve_node_keys(
        settings.to_str(),
        vike_model::credential_keys::is_tradehub_node_key,
    )
    .expect("the scratch node store is readable");
    (resolved.secrets.into_map(), source)
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

/// Assert the GUI and the CLI/daemon spelling agree on the bytes AND on which file answered, for
/// one planted shape. The failure message names the shape, because a bare pair of byte vectors
/// says nothing about which of four configurations produced it.
fn agree(settings: &Path, env: &HashMap<String, String>, shape: &str) -> NodeKeyResolution {
    let (gui_vars, resolution) = gui(settings, env);
    let (store_vars, source) = cli_and_daemon(settings);
    assert_eq!(
        resolution.source, source,
        "the GUI and the CLI/daemon disagree about WHICH FILE answers for the {shape} shape: \
         the GUI says {:?}, `vike_secrets::resolve_node_keys` says {source:?}",
        resolution.source
    );
    // The environment rung is the ONE documented difference between the two ladders (the GUI
    // gap-fills the observe key for the thin-client image; the CLI lets an export WIN). With an
    // empty env map neither applies, so the bytes must match exactly.
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

/// **THE GATE.** Over every shape a box can actually be in, the GUI resolves the tradehub pair to
/// the same bytes — and out of the same FILE — as `vike-cli` and `vike-tradehub` do.
///
/// A GUI that reads the pair out of the venue store fails the `node.env` shapes here, which is the
/// bug this file was written for.
#[test]
fn the_gui_resolves_a_node_key_out_of_the_same_file_as_the_cli_and_the_daemon() {
    let empty = HashMap::new();

    // --- the MIGRATED box: `node.env` holds the pair, `secrets.env` holds venue keys ------------
    let migrated = scratch();
    plant(migrated.path(), "secrets.env", &[("BINANCE_LIVE_API_KEY", "venue-key")]);
    plant(
        migrated.path(),
        "node.env",
        &[(OBSERVE_KEY_NAME, "obs-new"), (CONTROL_KEY_NAME, "ctl-new")],
    );
    let r = agree(migrated.path(), &empty, "migrated");
    assert_eq!(r.source, NodeKeySource::NodeFile);
    assert!(r.notices.is_empty(), "a migrated box has nothing to be told: {:?}", r.notices);

    // --- the UNMIGRATED box: the pair is still in the venue store, and it must still work -------
    let legacy = scratch();
    plant(
        legacy.path(),
        "secrets.env",
        &[
            ("BINANCE_LIVE_API_KEY", "venue-key"),
            (OBSERVE_KEY_NAME, "obs-old"),
            (CONTROL_KEY_NAME, "ctl-old"),
        ],
    );
    let r = agree(legacy.path(), &empty, "legacy");
    assert_eq!(r.source, NodeKeySource::LegacyCredentialStore);

    // --- the UNCONFIGURED box: no node key anywhere. The ordinary state, and it is SILENT -------
    let bare = scratch();
    plant(bare.path(), "secrets.env", &[("BINANCE_LIVE_API_KEY", "venue-key")]);
    let r = agree(bare.path(), &empty, "unconfigured");
    assert_eq!(r.source, NodeKeySource::Absent);
    assert!(
        r.notices.is_empty(),
        "an unconfigured box is not a misconfigured one: {:?}",
        r.notices
    );

    // --- NO settings directory at all: both spellings answer "nothing", neither panics ----------
    let missing = scratch();
    let nowhere = missing.path().join("not-a-settings-dir");
    let r = agree(&nowhere, &empty, "no-settings-directory");
    assert_eq!(r.source, NodeKeySource::Absent);
}

/// ⚠ **The node store answers WHOLLY — a pair is never stitched across the two files.**
///
/// A half-migrated box (the operator moved the observe line and not the control line) must come out
/// as an observe key and NO control key, exactly as `vike_secrets::resolve_node_keys`' own contract
/// says. Stitching `node.env`'s observe together with `secrets.env`'s leftover control produces a
/// MISMATCHED pair, whose whole symptom at the node is an opaque `bad mac` — the failure
/// `crates/vike-cli/tests/node_cli.rs` records as the expensive one.
#[test]
fn a_node_env_key_is_never_stitched_together_with_a_leftover_venue_store_key() {
    let half = scratch();
    plant(
        half.path(),
        "secrets.env",
        &[(OBSERVE_KEY_NAME, "obs-old"), (CONTROL_KEY_NAME, "ctl-old")],
    );
    plant(half.path(), "node.env", &[(OBSERVE_KEY_NAME, "obs-new")]);

    let r = agree(half.path(), &HashMap::new(), "half-migrated");
    assert_eq!(r.source, NodeKeySource::NodeFile);

    let (vars, _) = gui(half.path(), &HashMap::new());
    assert_eq!(vars.get(OBSERVE_KEY_NAME).map(String::as_str), Some("obs-new"));
    assert_eq!(
        vars.get(CONTROL_KEY_NAME),
        None,
        "`secrets.env`'s leftover control key was stitched onto `node.env`'s observe key — that is \
         a mismatched pair and an opaque `bad mac` at the node"
    );
}

/// The legacy fallback keeps a live box working and SAYS SO, naming the two files and the key
/// family — the one-time migration notice `vike-cli` prints on stderr, reaching the GUI's log
/// instead. Returned as DATA so this can assert it without a subscriber.
#[test]
fn the_legacy_credential_store_still_answers_and_the_migration_is_announced() {
    let legacy = scratch();
    plant(legacy.path(), "secrets.env", &[(OBSERVE_KEY_NAME, "obs-old")]);

    let (vars, r) = gui(legacy.path(), &HashMap::new());
    assert_eq!(vars.get(OBSERVE_KEY_NAME).map(String::as_str), Some("obs-old"));
    assert_eq!(r.source, NodeKeySource::LegacyCredentialStore);

    let said = r.notices.join("\n");
    assert!(said.contains("secrets.env"), "the notice must name the file to move FROM: {said}");
    assert!(said.contains("node.env"), "…and the file to move TO: {said}");
    assert!(!said.contains("obs-old"), "a key value leaked into an operator notice: {said}");
    // …and it is the one place that sentence is written: five binaries must not word it five ways.
    assert_eq!(
        said,
        vike_secrets::legacy_node_key_notice(legacy.path().to_str().unwrap()),
        "the GUI has grown its own wording for the decision-0051 migration"
    );
}

/// The thin-client image's rung is untouched: with no store at all, an exported observe key still
/// arms the GUI, and an exported CONTROL key still does not. That asymmetry is the deliberate one
/// `fill_node_keys_from_env` announces, and the node-store rung must not have quietly widened it.
#[test]
fn the_process_environment_still_gap_fills_the_observe_key_and_never_the_control_key() {
    let container = scratch();

    let (vars, r) = gui(container.path(), &map(&[(OBSERVE_KEY_NAME, "from-the-container")]));
    assert_eq!(r.source, NodeKeySource::Absent);
    assert_eq!(r.fill, NodeKeyFill::ObserveFromEnv);
    assert_eq!(vars.get(OBSERVE_KEY_NAME).map(String::as_str), Some("from-the-container"));

    let (vars, r) = gui(container.path(), &map(&[(CONTROL_KEY_NAME, "from-the-container")]));
    assert_eq!(r.fill, NodeKeyFill::ControlIgnored);
    assert!(
        !vars.contains_key(CONTROL_KEY_NAME),
        "a control key from the environment would let a variable on the box arm order placement"
    );
}

/// A STORE entry still wins over the environment, on both files — the gap-fill never became an
/// override. An exported key that silently replaced one an operator wrote into a file is the class
/// this workspace already fought over the risk ceilings.
#[test]
fn a_stored_observe_key_still_outranks_an_exported_one() {
    let exported = map(&[(OBSERVE_KEY_NAME, "from-the-environment")]);

    let from_node = scratch();
    plant(from_node.path(), "node.env", &[(OBSERVE_KEY_NAME, "from-node-env")]);
    let (vars, r) = gui(from_node.path(), &exported);
    assert_eq!(vars.get(OBSERVE_KEY_NAME).map(String::as_str), Some("from-node-env"));
    assert_eq!(r.fill, NodeKeyFill::Nothing);

    let from_legacy = scratch();
    plant(from_legacy.path(), "secrets.env", &[(OBSERVE_KEY_NAME, "from-secrets-env")]);
    let (vars, r) = gui(from_legacy.path(), &exported);
    assert_eq!(vars.get(OBSERVE_KEY_NAME).map(String::as_str), Some("from-secrets-env"));
    assert_eq!(r.fill, NodeKeyFill::Nothing);
}

/// The two key NAMES the GUI looks up are the ones the SERVER reads. The crate-local copies are
/// duplicated on purpose (see [`OBSERVE_KEY_NAME`]'s own doc), and this file's whole subject is
/// which file those names are read FROM — which is worth nothing if the names themselves drift.
#[test]
fn the_key_names_are_the_servers_own() {
    assert_eq!(OBSERVE_KEY_NAME, vike_tradehub_client::auth::OBSERVE_KEY_ENV);
    assert_eq!(CONTROL_KEY_NAME, vike_tradehub_client::auth::CONTROL_KEY_ENV);
    // …and both are PLATFORM keys, which is what routes them to `node.env` rather than to the
    // venue grid: `resolve_node_keys` decides which file answers with exactly this predicate.
    assert!(vike_model::credential_keys::is_platform_key(OBSERVE_KEY_NAME));
    assert!(vike_model::credential_keys::is_platform_key(CONTROL_KEY_NAME));
    // …and both are in the TRADEHUB family, which is the narrower predicate the resolution now
    // probes with. A name that fell out of the family would be looked up in a file chosen by a
    // probe that cannot see it — the failure `a_datahub_only_node_env_…` below is about.
    assert!(vike_model::credential_keys::is_tradehub_node_key(OBSERVE_KEY_NAME));
    assert!(vike_model::credential_keys::is_tradehub_node_key(CONTROL_KEY_NAME));
}

/// ⚠ **A `node.env` holding only the DATAHUB pair must not take the TRADEHUB pair away from a box
/// that has one — and this was a NEW `bad mac`, created by the fix for the old one.**
///
/// The shape is reached by two documented commands on a normal box: `vike-cli datahub setup` writes
/// the datahub pair into `node.env`, while the tradehub pair is still where it always was, in
/// `secrets.env`. `vike_secrets::resolve_node_keys` answers WHICH FILE by probing `node.env` with
/// the predicate it is handed; with the four-name `is_platform_key` that file "answered" for the
/// tradehub pair too, `overlay_node_store` found neither tradehub name in it and REMOVED both from
/// the credentials map, and the GUI signed with an empty key. Silently — the migration sentence
/// fires only on `NodeKeySource::LegacyCredentialStore`, and the source here was `NodeFile`.
///
/// The fix is decision 0051's own rule read literally: *whichever file answers, answers wholly* is
/// about the PAIR that is used together, so the probe is a SERVICE FAMILY
/// (`vike_model::credential_keys::is_tradehub_node_key`). Both halves are asserted — the pair still
/// resolves, AND the legacy notice still fires for it, because a box reading its node keys out of
/// `secrets.env` is exactly the box that has something to do.
#[test]
fn a_datahub_only_node_env_does_not_strip_the_tradehub_pair() {
    let split = scratch();
    plant(
        split.path(),
        "secrets.env",
        &[
            ("BINANCE_LIVE_API_KEY", "venue-key"),
            (OBSERVE_KEY_NAME, "obs-legacy"),
            (CONTROL_KEY_NAME, "ctl-legacy"),
        ],
    );
    // What `vike-cli datahub setup` writes, and nothing else.
    plant(
        split.path(),
        "node.env",
        &[
            (vike_datahub_client::node_auth::DATAHUB_OBSERVE_KEY_ENV, "dh-obs"),
            (vike_datahub_client::node_auth::DATAHUB_CONTROL_KEY_ENV, "dh-ctl"),
        ],
    );

    let r = agree(split.path(), &HashMap::new(), "datahub-only-node-env");
    assert_eq!(
        r.source,
        NodeKeySource::LegacyCredentialStore,
        "the DATAHUB pair's migration decided where the TRADEHUB pair is read from"
    );

    let (vars, _) = gui(split.path(), &HashMap::new());
    assert_eq!(
        vars.get(OBSERVE_KEY_NAME).map(String::as_str),
        Some("obs-legacy"),
        "a working tradehub observe key in `secrets.env` was removed because ANOTHER service's \
         pair had been migrated — the client then signs with an empty key and the node answers \
         `bad mac`"
    );
    assert_eq!(vars.get(CONTROL_KEY_NAME).map(String::as_str), Some("ctl-legacy"));
    assert!(
        r.notices.iter().any(|n| n.contains("node.env")),
        "the legacy fallback answered and must still say so: {:?}",
        r.notices
    );
}

/// The MIRROR IMAGE, on the datahub half of the same file pair: a `node.env` holding only the
/// TRADEHUB pair (what `vike-cli backend setup` writes) must not take a datahub key away from a box
/// whose datahub key is still in `secrets.env`.
///
/// `backend_registry::datahub_observe_keys` had the identical wide probe, so the hole was symmetric
/// — and its consequence is quieter and therefore worse: not a removed key but a key that never
/// resolves, so the market-data plane dials UNAUTHENTICATED and the handshake fails against any
/// keyed datahub, with nothing said about why.
#[test]
fn a_tradehub_only_node_env_does_not_hide_the_datahub_key() {
    let split = scratch();
    plant(
        split.path(),
        "secrets.env",
        &[(vike_datahub_client::node_auth::DATAHUB_OBSERVE_KEY_ENV, "dh-legacy")],
    );
    plant(
        split.path(),
        "node.env",
        &[(OBSERVE_KEY_NAME, "obs-new"), (CONTROL_KEY_NAME, "ctl-new")],
    );

    let (keys, notices) = backend_registry::datahub_observe_keys(
        split.path().to_str(),
        &HashMap::new(),
        backend_registry::DATAHUB_OBSERVE_KEY_NAME,
    );
    let keys = keys.expect(
        "the datahub key in `secrets.env` did not resolve — the TRADEHUB pair's migration decided \
         which file this one is read from",
    );
    assert_eq!(keys.key_for(vike_datahub_client::Scope::Read), b"dh-legacy");
    assert!(
        notices.iter().any(|n| n.contains("node.env")),
        "the legacy fallback answered for the datahub key and must say so: {notices:?}"
    );
}

/// ⚠ **THE SAME DEFECT WEARING THE OTHER SIGN — a CUSTOM datahub key name resolves out of whichever
/// file carries it, and narrowing the probe to the platform family alone broke exactly that.**
///
/// `backend_registry::datahub_observe_keys` reads `key_name`, which is a record's own
/// `BackendRecord::datahub_observe_key` when it names one. The probe handed to
/// `vike_secrets::resolve_node_keys` decides WHICH FILE is then read, so a probe that cannot match
/// `key_name` answers for a file that does not carry it:
///
///   - the WIDE `is_platform_key` matched the tradehub pair, so a `node.env` written by
///     `vike-cli backend setup` with a custom datahub name added beside it answered, and the custom
///     name was found there — this layout WORKED;
///   - the narrow `is_datahub_node_key` that fixed
///     [`a_tradehub_only_node_env_does_not_hide_the_datahub_key`] can never match a custom name, so
///     that same file stopped answering, resolution fell through to `secrets.env`, the name is not
///     there, and the key resolved to `None`. The market-data plane then dials UNAUTHENTICATED and
///     fails the handshake with nothing said — no notice fires for a name outside the two families.
///
/// The probe is therefore `is_datahub_node_key(k) || k == key_name`: exactly the names this caller
/// takes out of the file it chose. Three shapes are asserted — the regression's own layout, a
/// `node.env` carrying the custom name ALONE, and the custom name left behind in `secrets.env` —
/// because a probe narrowed or widened again fails at least one of them.
#[test]
fn a_custom_datahub_key_name_resolves_out_of_the_file_that_carries_it() {
    const CUSTOM: &str = "PROD2_DATAHUB_OBSERVE_KEY";

    let observe = |dir: &Path| -> (Option<Vec<u8>>, Vec<String>) {
        let (keys, notices) =
            backend_registry::datahub_observe_keys(dir.to_str(), &HashMap::new(), CUSTOM);
        (keys.map(|k| k.key_for(vike_datahub_client::Scope::Read).to_vec()), notices)
    };

    // THE REGRESSION'S OWN LAYOUT: `backend setup` wrote the tradehub pair into `node.env` and the
    // operator added their per-backend datahub name beside it. Nothing is in `secrets.env`.
    let beside = scratch();
    plant(beside.path(), "secrets.env", &[("BINANCE_LIVE_API_KEY", "venue-key")]);
    plant(
        beside.path(),
        "node.env",
        &[(OBSERVE_KEY_NAME, "obs-new"), (CONTROL_KEY_NAME, "ctl-new"), (CUSTOM, "dh-custom")],
    );
    let (key, notices) = observe(beside.path());
    assert_eq!(
        key.as_deref(),
        Some(&b"dh-custom"[..]),
        "a custom datahub name sitting in `node.env` beside the TRADEHUB pair did not resolve — the \
         probe cannot match it, so the file that carries it was never chosen, and the market-data \
         plane dials unauthenticated in silence"
    );
    assert!(notices.is_empty(), "`node.env` answered; there is nothing to migrate: {notices:?}");

    // …and ALONE in `node.env`, with no platform key of any family beside it.
    let alone = scratch();
    plant(alone.path(), "node.env", &[(CUSTOM, "dh-alone")]);
    let (key, _) = observe(alone.path());
    assert_eq!(
        key.as_deref(),
        Some(&b"dh-alone"[..]),
        "a `node.env` holding only the custom name must answer for it: the probe is the set this \
         call READS, and that set includes `key_name`"
    );

    // …and the legacy shape is unchanged: a custom name still in `secrets.env`, with no `node.env`
    // at all, resolves through the fallback and says so.
    let legacy = scratch();
    plant(legacy.path(), "secrets.env", &[(CUSTOM, "dh-legacy")]);
    let (key, notices) = observe(legacy.path());
    assert_eq!(key.as_deref(), Some(&b"dh-legacy"[..]));
    assert!(
        notices.iter().any(|n| n.contains("node.env")),
        "the legacy fallback answered and must still say so: {notices:?}"
    );

    // ⚠ …and the widening is SCOPED: a custom datahub name in `node.env` must not make that file
    // the answer for the TRADEHUB pair, which `overlay_node_store` resolves through its own family
    // predicate. This is the property that made the wide `is_platform_key` a defect, and it has to
    // survive the fix for the narrow one.
    let scoped = scratch();
    plant(
        scoped.path(),
        "secrets.env",
        &[(OBSERVE_KEY_NAME, "obs-legacy"), (CONTROL_KEY_NAME, "ctl-legacy")],
    );
    plant(scoped.path(), "node.env", &[(CUSTOM, "dh-custom")]);
    let r = agree(scoped.path(), &HashMap::new(), "custom-datahub-name-only");
    assert_eq!(
        r.source,
        NodeKeySource::LegacyCredentialStore,
        "a CUSTOM datahub name is in neither service family, so it must not decide where the \
         tradehub pair is read from"
    );
    let (vars, _) = gui(scoped.path(), &HashMap::new());
    assert_eq!(vars.get(OBSERVE_KEY_NAME).map(String::as_str), Some("obs-legacy"));
    assert_eq!(vars.get(CONTROL_KEY_NAME).map(String::as_str), Some("ctl-legacy"));
}

/// ⚠ **The unreadable-store notice names the file that ACTUALLY failed.**
///
/// `resolve_node_keys` opens `node.env` and then, for a family that file does not carry,
/// `secrets.env` — so its `Err` can come from either, and the notice used to assert one of them
/// unconditionally: *"the node-key store is PRESENT but UNREADABLE … the venue credential store is
/// deliberately NOT read in its place."* With a broken VENUE store that is wrong three times over:
/// `node.env` may not exist at all, the venue store WAS read, and reading it is what failed — while
/// the interpolated error, whose `Display` names the real path, contradicted the sentence around it.
///
/// ⚠ The fixture plants a DIRECTORY where the store file goes rather than playing with permissions:
/// a read of one fails on every platform this tree builds for, never with `NotFound`, and needs no
/// mode bits, no umask assumption and no non-root runner. `chmod 000` gives none of that.
#[test]
fn the_unreadable_store_notice_names_the_file_that_actually_failed() {
    // The VENUE store is the broken one, and `node.env` does not exist at all.
    let broken_venue = scratch();
    fs::create_dir(broken_venue.path().join("secrets.env")).expect("plant a directory");

    let mut creds = HashMap::new();
    let r =
        backend_registry::fill_node_keys(&mut creds, broken_venue.path().to_str(), &HashMap::new());
    assert_eq!(r.source, NodeKeySource::Absent, "no file answered");
    let said = r.notices.join("\n");
    assert!(
        said.contains("secrets.env"),
        "the notice must name the file that actually failed to read: {said}"
    );
    assert!(
        !said.contains("node.env"),
        "the notice claims a file the code never found unreadable — `node.env` is absent here: \
         {said}"
    );
    assert!(
        !said.contains("deliberately NOT read"),
        "the venue store WAS read and that read is what failed; claiming it was withheld sends an \
         operator to the wrong file: {said}"
    );

    // …and the other direction: a broken NODE store names `node.env` and not the venue file.
    let broken_node = scratch();
    plant(broken_node.path(), "secrets.env", &[(OBSERVE_KEY_NAME, "obs-legacy")]);
    fs::create_dir(broken_node.path().join("node.env")).expect("plant a directory");

    let mut creds = HashMap::new();
    creds.insert(OBSERVE_KEY_NAME.to_string(), "obs-legacy".to_string());
    let r =
        backend_registry::fill_node_keys(&mut creds, broken_node.path().to_str(), &HashMap::new());
    let said = r.notices.join("\n");
    assert!(said.contains("node.env"), "the broken file is the node store here: {said}");
    assert!(
        !creds.contains_key(OBSERVE_KEY_NAME),
        "an unreadable node store must REMOVE the pair rather than degrade to the venue store's \
         copy — a permissions bug that silently keeps working stops working the day the legacy \
         fallback is deleted"
    );
}
