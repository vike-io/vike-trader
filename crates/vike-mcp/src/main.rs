//! `vike-mcp`: the program. One environment sweep, the boot, what a booted process owes before any
//! flag is read, then the library's server (`crates/vike-mcp/src/lib.rs`).
//!
//! ⚠ **The order is the program's behaviour, and a refusal below comes before the argv is read**, so
//! `--help` under an unsound seal or a removed variable is refused like every other start.

use std::collections::HashMap;
use std::process::ExitCode;

use vike_client::resolved::{
    Resolved, datahub_keyring, hub_keyring, install_user_indicators, seal_refusal_text,
};

/// The name a stderr line opens with.
const BIN: &str = env!("CARGO_PKG_NAME");

fn main() -> ExitCode {
    let resolved = match boot() {
        Ok(resolved) => resolved,
        Err(e) => {
            eprintln!("{BIN}: {e}");
            return ExitCode::FAILURE;
        }
    };
    install_user_indicators(BIN, resolved.user_data_dir.as_deref());
    // This program ACTS on the ceilings, so it refuses where `vike-cli`'s repair verbs still run.
    // `crates/vike-cli/src/dispatch.rs`'s `SEAL_REFUSING_VERBS` is the same rule for `trade`.
    if let Some(why) = resolved.seal_refusal.as_deref() {
        eprintln!("{}", seal_refusal_text(BIN, why));
        return ExitCode::FAILURE;
    }
    // argv[0] is the binary.
    vike_mcp::run(
        std::env::args().skip(1),
        resolved.policy_max_notional,
        resolved.node_ack_wait,
        &hub_keyring(&resolved, BIN),
        resolved.state_dir.as_deref(),
        datahub_keyring(&resolved, BIN),
        resolved.backtest_addr.as_deref(),
        resolved.datahub_addr.as_deref(),
    )
}

/// Refuse a stale environment and load this box's settings: `vike_boot::boot`'s order, with each way
/// this root departs from it a named arm carrying its reason. No boot anchor is written, and
/// `crates/vike-boot/tests/one_owner/boot_journal_wiring.rs`'s row says why.
fn boot() -> Result<Resolved, String> {
    let vars: HashMap<String, String> = std::env::vars().collect();
    let cwd = std::env::current_dir().ok();
    let booted = vike_boot::boot(&vike_boot::BootSpec {
        env: &vars,
        cwd: cwd.as_deref(),
        identity: vike_boot::Identity {
            name: env!("CARGO_PKG_NAME"),
            version: env!("CARGO_PKG_VERSION"),
        },
        removed_env: vike_boot::RemovedEnv::Refuse,
        // REPORT: `Resolved::seal_refusal` carries the mark, and `main` refuses on it after the
        // user's indicators are installed, the order this server has always started in.
        settings: vike_boot::SettingsLoad::Load,
        credentials: vike_boot::Credentials::Deferred(
            "the hub keys are read by `hub_keyring` and `datahub_keyring` from the narrow `hub_key` \
             table, and no venue credential is opened at all: this server reaches none \
             (docs/decisions/0036).",
        ),
        log_home: vike_boot::LogHome::Elsewhere(
            "this server builds no log subscriber: its stdout is a protocol, and everything it has \
             to say goes to stderr.",
        ),
        disclosure: vike_boot::Disclosure::Skip(
            "`boot_lines` re-reads every settings file to recover each row's ORIGIN, a daemon's \
             one-off cost. `vike-cli config show` prints all of it, on request.",
        ),
    })?;
    Ok(Resolved::from_booted(booted, vars, BIN))
}
