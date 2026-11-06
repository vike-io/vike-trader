// Compiled into ONE test binary, the `shipped_cli` group (`tests/shipped_cli/main.rs`); a member that
// stops calling an item should not have to delete it from the shared kit, hence the allow.
#![allow(dead_code)]
//! Shared test-support for the `vike-cli` cases that drive the SHIPPED binary: the plain runners
//! ([`run`], [`run_in`]), the scratch credential store ([`seed_store`]) and the positive-argv pin
//! harness ([`accepted`]). A case that waits for another thread polls through
//! `vike_log::poll::wait_until`.
//!
//! The planted-engine plant and its `ETXTBSY` retry are NOT here: they are
//! `crates/vike-cli/src/test_support.rs` (`vike_cli::test_support`, behind the `test-support`
//! feature), compiled once by the library because `tests/planted_binary_retry.rs` needs them too.

use std::process::{Command, Output};

// ------------------------------------------------------------------------------------------------
// The plain runners: cases that plant nothing
// ------------------------------------------------------------------------------------------------
//
// ⚠ These do NOT retry `ETXTBSY`: nothing they run was planted by the case, so there is no write
// descriptor to race. A case that plants an engine keeps using its file's own runner, which goes
// through `vike_cli::test_support::output_retrying_etxtbsy`.

/// Run the shipped binary against a FRESH, EMPTY settings directory, with `envs` layered on top of
/// the risk-variable and hub-key clears every caller relies on.
///
/// The two removed-variable removals are not decoration: `vike_config::refuse_removed_env` runs
/// before any verb is routed, so an exported `VIKE_MAX_ORDER_NOTIONAL` on the developer's box
/// would make every case exit on the startup-refusal path instead of the rung it is testing.
pub fn run(args: &[&str], envs: &[(&str, &str)]) -> Output {
    let empty = tempfile::tempdir().expect("tempdir");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_vike-cli"));
    cmd.args(args)
        .env("VIKE_SETTINGS_DIR", empty.path())
        .env_remove("VIKE_MAX_ORDER_NOTIONAL")
        .env_remove("VIKE_TRADEHUB_MAX_ORDER_NOTIONAL")
        .env_remove("VIKE_TRADEHUB_OBSERVE_KEY")
        .env_remove("VIKE_TRADEHUB_CONTROL_KEY");
    for (k, v) in envs {
        cmd.env(k, v);
    }
    cmd.output().unwrap_or_else(|e| panic!("run vike-cli {args:?}: {e}"))
}

/// [`run`]'s twin for the cases that need a POPULATED project: `VIKE_SETTINGS_DIR` is `settings`
/// and `VIKE_USER_DATA_DIR` is `user_data`, keeping every `env_remove`. A second helper rather
/// than an edit, because the emptiness of [`run`]'s directory is what its callers rely on.
pub fn run_in(settings: &std::path::Path, user_data: &std::path::Path, args: &[&str]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_vike-cli"));
    cmd.args(args)
        .env("VIKE_SETTINGS_DIR", settings)
        .env("VIKE_USER_DATA_DIR", user_data)
        .env_remove("VIKE_MAX_ORDER_NOTIONAL")
        .env_remove("VIKE_TRADEHUB_MAX_ORDER_NOTIONAL")
        .env_remove("VIKE_TRADEHUB_OBSERVE_KEY")
        .env_remove("VIKE_TRADEHUB_CONTROL_KEY");
    cmd.output().unwrap_or_else(|e| panic!("run vike-cli {args:?}: {e}"))
}

// ---------------------------------------------------------------------------------------------
// The credential store a scratch project needs
// ---------------------------------------------------------------------------------------------

/// **Create a scratch settings directory's settings DATABASE and write `rows` into it** — the one
/// credential store since the credential FILE store was removed (2026-10-07). The store is created
/// the way a fresh box creates it (`vike_secrets::create_store`
///, the library half of `vike-cli secrets init`),
/// so an empty `rows` still leaves the
/// (empty) store a fixture asked for. Each row lands where the production writer puts it (through
/// its record-less body, `vike_secrets::save_credentials_to_store_unjournalled`: a seed is not an
/// operator act, so no ledger line is written): a
/// platform key (`vike_venues::credential_keys::is_platform_key`, the node pairs among them) in the
/// `hub_key` table, every other name in the `credential` table as a DEPLOYMENT row — no account,
/// no venue: a name-addressed write creates no account (decision 0156 item 6). The database is
/// created 0600 in a 0700 directory, so no permission finding buries a case's own transcript.
pub fn seed_store(settings: &std::path::Path, rows: &[(&str, &str)]) {
    vike_secrets::create_store(Some(settings.to_str().expect("a UTF-8 scratch path")))
        .expect("create the scratch settings database");
    let mut node = Vec::new();
    let mut credential = Vec::new();
    for (k, v) in rows {
        let row = (k.to_string(), v.to_string());
        if vike_venues::credential_keys::is_platform_key(k) {
            node.push(row);
        } else {
            credential.push(row);
        }
    }
    if !node.is_empty() {
        vike_secrets::save_credentials_to_store_unjournalled(
            settings,
            vike_secrets::Table::HubKey,
            &node,
            vike_secrets::UpsertIntent::Add,
        )
        .expect("write the fixture's hub keys");
    }
    if !credential.is_empty() {
        vike_secrets::save_credentials_to_store_unjournalled(
            settings,
            vike_secrets::Table::Credential,
            &credential,
            vike_secrets::UpsertIntent::Add,
        )
        .expect("write the fixture's credentials");
    }
}

// ---------------------------------------------------------------------------------------------
// A REAL paper node and the store that addresses it
// ---------------------------------------------------------------------------------------------

/// Far-future resolution so the A-S horizon is positive.
const PAPER_NODE_RESOLUTION_TS: i64 = 3_000_000_000;

/// A PAPER `vike-tradehub` node mounted on `venue`/`token` and served on an ephemeral loopback port
/// with CONTROL enabled, keyed by `observe_key`/`control_key`: the smallest fixture that can
/// genuinely REFUSE (or accept) a command server-side. Hold the returned mount for the case's
/// lifetime. `tests/shipped_cli/exit_codes.rs`'s `venue_reject_fixture` and
/// `tests/shipped_cli/trade_plane_cli.rs`'s `json_read_fixture` name their own venue, token and
/// keys over it.
pub fn spawn_paper_node(
    venue: &str,
    token: &str,
    observe_key: &str,
    control_key: &str,
) -> (vike_tradehub_core::PaperMakerCore, std::net::SocketAddr) {
    use vike_tradehub::{publish, server};

    let cfg = vike_tradehub_core::MakerConfig::outcome_token(
        venue,
        token,
        Some(PAPER_NODE_RESOLUTION_TS),
    );
    let mount = vike_tradehub_core::build_paper_maker_core(&cfg);
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let publisher = publish::spawn(mount.handle.snapshot_cell(), None);
    let commands = Some(mount.handle.command_sink());
    let keys = vike_hub_proto::auth::HubKeys::new(
        observe_key.as_bytes().to_vec(),
        control_key.as_bytes().to_vec(),
    );
    std::thread::spawn(move || {
        let _ = server::serve(
            listener,
            publisher,
            keys,
            commands,
            server::control::ControlLimitsConfig::default(),
            None,
            None,
            None,
        );
    });
    (mount, addr)
}

/// A throwaway `<project>/settings` whose settings database holds both hub keys, so `vike-cli`'s
/// dispatcher resolves them exactly as it would against a real daemon's store.
pub fn hub_key_settings_dir(observe_key: &str, control_key: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    seed_store(
        dir.path(),
        &[("VIKE_TRADEHUB_OBSERVE_KEY", observe_key), ("VIKE_TRADEHUB_CONTROL_KEY", control_key)],
    );
    dir
}

// ---------------------------------------------------------------------------------------------
// The positive-argv pin harness (decision 0155)
// ---------------------------------------------------------------------------------------------

pub mod accepted;
