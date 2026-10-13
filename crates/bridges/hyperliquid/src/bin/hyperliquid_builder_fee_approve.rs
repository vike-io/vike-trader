//! One-time on-chain `approveBuilderFee` grant — the operator tool a builder address's **master**
//! wallet runs once before Hyperliquid accepts a nonzero
//! `vike_hyperliquid::signing::action::HlBuilderFee::fee_tenths_bp` on any order attributed to it.
//! Gated behind the `builder-fee-approve` feature — never built by default, never invoked from the live
//! exec/mount path (see `vike_hyperliquid::user_signed::builder_fee`'s module doc for why the master-wallet
//! requirement makes this a deliberately manual, standalone step; an attribution-only mount, fee
//! = 0, never needs this tool at all).
//!
//! Usage:
//!   `cargo run -p vike-hyperliquid --features builder-fee-approve --bin
//!   hyperliquid_builder_fee_approve -- <builder_address> <max_fee_rate> [--mainnet]`
//!
//! `<max_fee_rate>` is HL's percentage-string shape, e.g. `"0.01%"`. Reads the `PRIVATE_KEY` field
//! of the ONE active default hyperliquid account at the tier it operates on, out of the credential
//! store (the settings database, through `CredentialStore::credential_book` and
//! `vike_secrets::account_directory::AccountDirectory`; never argv/logs) — `--mainnet` selects the
//! `live` account (mainnet, real funds); its absence the `demo` account (testnet). ⚠ The loaded key
//! MUST be the account's MASTER key: this bin refuses to run when that account's
//! `ACCOUNT_ADDRESS` field is set (that shape means the key is an agent/API wallet).

use std::process::ExitCode;

use vike_hyperliquid::base::config;
use vike_hyperliquid::base::consts::VENUE;
use vike_hyperliquid::base::transport::HyperliquidTransport;
use vike_hyperliquid::signing::Signer;
use vike_hyperliquid::user_signed::builder_fee::approve_builder_fee;
use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;
use vike_secrets::account_directory::AccountDirectory;
use vike_secrets::{CredentialScope, CredentialStore, StoreLocation};
use vike_util::cli::{CliExit, Program};

/// The command line, read by `clap` through `vike_util::cli::Program` (decision 0155): the two
/// positionals, `--mainnet` anywhere among them, `-h`/`--help` and `-V`/`--version` on stdout with
/// exit 0, any usage error (a missing positional, a third one, an unknown flag) on stderr with
/// exit 2. Nothing here is a credential: the key comes from the credential store, never argv.
#[derive(clap::Parser, Debug, PartialEq, Eq)]
#[command(
    name = "hyperliquid_builder_fee_approve",
    about = "One-time on-chain approveBuilderFee grant, signed by the builder address's master wallet"
)]
struct Request {
    /// The builder address to approve.
    #[arg(value_name = "BUILDER_ADDRESS")]
    builder: String,
    /// The highest fee rate to approve, in Hyperliquid's percentage-string shape (e.g. "0.01%").
    #[arg(value_name = "MAX_FEE_RATE")]
    max_fee_rate: String,
    /// Sign on MAINNET with the LIVE key (real funds); without it, the DEMO key on testnet.
    #[arg(long)]
    mainnet: bool,
}

/// Read the command line (WITHOUT the program name).
///
/// # Errors
/// The [`CliExit`] a process would have printed: `--help`/`--version` (code 0) or a usage error
/// (code 2).
fn parse_args_from(args: &[String]) -> Result<Request, CliExit> {
    Program::new(
        "hyperliquid_builder_fee_approve",
        concat!("hyperliquid_builder_fee_approve ", env!("CARGO_PKG_VERSION")),
    )
    .try_parse_from(args)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Request { builder, max_fee_rate, mainnet } = match parse_args_from(&args) {
        Ok(request) => request,
        Err(exit) => exit.emit(),
    };
    let (builder, max_fee_rate) = (builder.as_str(), max_fee_rate.as_str());

    // The credential chain, from the ONE process-environment sweep this root owns: the
    // settings-directory override and the `VIKE_NO_LIVE_KEYS` scope come out of `process_env`, and
    // nothing below this line reads the environment again. (`env` is taken — it is the HL
    // testnet/mainnet tier a few lines down.) A scope word that is neither `true` nor `false`
    // refuses the run with `ScopeError`'s own sentence.
    let process_env: std::collections::HashMap<String, String> = std::env::vars().collect();
    let scope = match CredentialScope::parse(
        process_env.get(vike_secrets::NO_LIVE_KEYS_ENV).map(String::as_str),
    ) {
        Ok(scope) => scope,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    let settings_dir = vike_secrets::workspace_settings_dir_from(
        process_env.get(vike_secrets::SETTINGS_DIR_ENV).map(String::as_str),
    );
    let store = CredentialStore::open(Some(StoreLocation::new(settings_dir)), scope);
    // The account's credentials, by FIELD (decision 0156 item 6): the ONE active default
    // hyperliquid account at the tier this run operates on, and its fields only — no credential
    // name is composed, and no other account's key can reach this tool.
    let directory =
        AccountDirectory::read(store.accounts(), store.account_keys(), store.credential_book());
    let tier = if mainnet { AccountTier::Live } else { AccountTier::Demo };
    let account = directory.credentials_for(VENUE, &AccountLabel::Default, tier);
    let Some(creds) = config::load(tier, &account) else {
        eprintln!(
            "no {tier} hyperliquid account holding a PRIVATE_KEY in the credential store — nothing \
             to sign with"
        );
        return ExitCode::FAILURE;
    };
    if creds.account_address.is_some() {
        // ⚠ "would LIKELY be rejected" until 2026-09-14 — it is now MEASURED. The venue answers an
        // agent-signed approveBuilderFee with `Must deposit before performing actions. User:
        // <the agent>`, naming the AGENT as the principal, and `/info maxBuilderFee` is unchanged
        // afterwards: `crates/bridges/hyperliquid/tests/hyperliquid_permissioning_smoke.rs`.
        eprintln!(
            "the {tier} hyperliquid account's ACCOUNT_ADDRESS is set — the loaded key is an AGENT \
             wallet, not the master account. approveBuilderFee must be signed by the master; \
             refusing to send a request the venue is measured to reject (it resolves a user-signed \
             action to the SIGNER's own account, so the grant would never reach this master)."
        );
        return ExitCode::FAILURE;
    }
    let Ok(signer) = Signer::from_private_key(&creds.private_key, creds.network) else {
        eprintln!("invalid private key");
        return ExitCode::FAILURE;
    };
    let transport = HyperliquidTransport::new(creds.network);
    let net_label = if mainnet { "MAINNET (real funds)" } else { "testnet" };
    let master = signer.address();
    println!(
        "approving builder {builder} up to {max_fee_rate} on {net_label} for master {master}..."
    );

    match approve_builder_fee(&transport, &signer, builder, max_fee_rate, creds.network) {
        Ok(resp) => {
            println!("{}", serde_json::to_string_pretty(&resp).unwrap_or_default());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("approveBuilderFee failed: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(line: &[&str]) -> Vec<String> {
        line.iter().map(|s| (*s).to_string()).collect()
    }

    fn request(builder: &str, rate: &str, mainnet: bool) -> Option<Request> {
        Some(Request { builder: builder.into(), max_fee_rate: rate.into(), mainnet })
    }

    /// POSITIVE argv pins (decision 0155, written against the hand-rolled scanner and kept through
    /// the `clap` migration): the module doc's line, with `--mainnet` before, between or after the
    /// two positionals, and without it.
    #[test]
    fn the_documented_line_is_accepted_with_its_meaning() {
        assert_eq!(
            parse_args_from(&args(&["0x0c8d1234", "0.01%"])).ok(),
            request("0x0c8d1234", "0.01%", false)
        );
        for line in [
            ["0x0c8d1234", "0.01%", "--mainnet"],
            ["--mainnet", "0x0c8d1234", "0.01%"],
            ["0x0c8d1234", "--mainnet", "0.01%"],
        ] {
            assert_eq!(
                parse_args_from(&args(&line)).ok(),
                request("0x0c8d1234", "0.01%", true),
                "{line:?}"
            );
        }
    }

    /// A missing positional, a third one and an unknown flag are usage errors (stderr, exit 2). The
    /// scanner this replaced dropped an unknown `--flag` and a third positional in silence.
    #[test]
    fn fewer_than_two_positionals_more_than_two_and_an_unknown_flag_are_usage_errors() {
        for line in [
            &[][..],
            &["0x0c8d1234"],
            &["--mainnet", "0x0c8d1234"],
            &["a", "b", "c"],
            &["a", "b", "--nope"],
        ] {
            let exit = parse_args_from(&args(line)).expect_err("this line must be refused");
            assert_eq!((exit.code, exit.to_stdout), (2, false), "{line:?}: {exit:?}");
        }
    }

    #[test]
    fn help_and_version_are_a_success_on_stdout() {
        let exit = parse_args_from(&args(&["--help"])).expect_err("help is an exit");
        assert_eq!((exit.code, exit.to_stdout), (0, true));
        assert!(exit.text.contains("--mainnet"), "{}", exit.text);
        let exit = parse_args_from(&args(&["-V"])).expect_err("version is an exit");
        assert_eq!(
            exit.text,
            concat!("hyperliquid_builder_fee_approve ", env!("CARGO_PKG_VERSION"), "\n")
        );
    }
}
