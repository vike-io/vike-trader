//! One-time on-chain `approveBuilderFee` grant — the operator tool a builder address's **master**
//! wallet runs once before Hyperliquid accepts a nonzero
//! `vike_hyperliquid::signing::action::HlBuilderFee::fee_tenths_bp` on any order attributed to it.
//! Mirrors Nautilus's `hyperliquid-builder-fee-approve` CLI. Gated behind the
//! `builder-fee-approve` feature — never built by default, never invoked from the live
//! exec/mount path (see `vike_hyperliquid::builder_fee`'s module doc for why the master-wallet
//! requirement makes this a deliberately manual, standalone step; an attribution-only mount, fee
//! = 0, never needs this tool at all).
//!
//! Usage:
//!   `cargo run -p vike-hyperliquid --features builder-fee-approve --bin
//!   hyperliquid_builder_fee_approve -- <builder_address> <max_fee_rate> [--mainnet]`
//!
//! `<max_fee_rate>` is HL's percentage-string shape, e.g. `"0.01%"`. Reads
//! `HYPERLIQUID_{DEMO,LIVE}_PRIVATE_KEY` from the credential store
//! (`<project>/settings/secrets.env`, never argv/logs) —
//! `--mainnet` selects the `LIVE` tier (mainnet, real funds); its absence the `DEMO` tier
//! (testnet). ⚠ The loaded key MUST be the account's MASTER key: this bin refuses to run when
//! `HYPERLIQUID_{tier}_ACCOUNT_ADDRESS` is set (that shape means the key is an agent/API wallet).

use std::process::ExitCode;

use vike_bridge_core::credentials::load_workspace_secrets_from_env;
use vike_hyperliquid::builder_fee::approve_builder_fee;
use vike_hyperliquid::config::{self, Env};
use vike_hyperliquid::signing::Signer;
use vike_hyperliquid::transport::HyperliquidTransport;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let mainnet = args.iter().any(|a| a == "--mainnet");
    let positional: Vec<&str> =
        args.iter().skip(1).filter(|a| !a.starts_with("--")).map(String::as_str).collect();
    if positional.len() < 2 {
        eprintln!(
            "usage: hyperliquid_builder_fee_approve <builder_address> <max_fee_rate> [--mainnet]"
        );
        eprintln!(r#"  e.g.: hyperliquid_builder_fee_approve 0x0c8d... "0.01%" --mainnet"#);
        return ExitCode::FAILURE;
    }
    let builder = positional[0];
    let max_fee_rate = positional[1];

    // The credential chain, from the ONE process-environment sweep this root owns: the home
    // settings-directory override comes out of `process_env`, and
    // nothing below this line reads the environment again. (`env` is taken — it is the HL
    // testnet/mainnet tier a few lines down.)
    let process_env: std::collections::HashMap<String, String> = std::env::vars().collect();
    let vars = load_workspace_secrets_from_env(&process_env);
    let env = if mainnet { Env::Live } else { Env::Demo };
    let tier = if mainnet { "LIVE" } else { "DEMO" };
    let Some(creds) = config::load(env, &vars) else {
        eprintln!(
            "no HYPERLIQUID_{tier}_PRIVATE_KEY in the credential store — nothing to sign with"
        );
        return ExitCode::FAILURE;
    };
    if creds.account_address.is_some() {
        eprintln!(
            "HYPERLIQUID_{tier}_ACCOUNT_ADDRESS is set — the loaded key is an AGENT wallet, not \
             the master account. approveBuilderFee must be signed by the master; refusing to send \
             a request that would likely be rejected (or silently grant nothing)."
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
