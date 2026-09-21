//! **DOES A DERIBIT RESPONSE NAME THE ACCOUNT?** — a read-only measurement, printed and not
//! committed.
//!
//!     cargo test -p vike-deribit --test deribit_identity_probe -- --ignored --nocapture
//!
//! The deribit sibling of `crates/bridges/okx/tests/okx_identity_probe.rs`, whose module doc
//! carries the full argument for why this is a PROBE rather than a fixture capture.
//!
//! # ⚠ WHAT IT ANSWERED — MEASURED 2026-09-20 on the live demo account
//!
//! The question was whether the id already rides a body somebody fetches. `private/
//! get_account_summary` is sent by `vike_deribit::DeribitReconClient::fetch_balance` on EVERY
//! reconcile pass, so if the id were in THAT result deribit would be binance-shaped and the
//! identity would cost nothing. It is not.
//!
//! * **The plain form names NOTHING** — no id, no username, no email. 43 keys, all position and
//!   margin figures.
//! * **The `extended` form is a strict SUPERSET of it** — 60 keys, nothing dropped, `balance`
//!   byte-identical — and carries `id` (a NUMBER: the account), `username`, `system_name`,
//!   `email`, `type` (`"main"` / `"subaccount"`, stated OUTRIGHT, which okx does not do) and
//!   `referrer_id`.
//!
//! So **deribit is okx-shaped, not binance-shaped**: the account read is a genuinely new call.
//! `vike_deribit::recon_client::parse_account_identity` is what that measurement produced.
//!
//! # ⚠ …and the sanitizer recognised NONE of it
//!
//! `redacted_paths` came back EMPTY for both forms, so this run printed a real email address in
//! the clear. `vike_bridge_core::capture::REDACT_KEYS` gained `email`, `username` and
//! `system_name` in the same commit as this measurement — before anything captures that body, the
//! rule that module doc already states. The account id itself is spelled `id` and CANNOT join that
//! list (it is every order and trade id too); that residual is pinned by
//! `the_account_id_deribit_spells_id_is_deliberately_not_redacted`.
//!
//! The probe still sends BOTH forms, because the comparison is the evidence.
//!
//! That is also why this is measured rather than read off the venue's documentation: this tree
//! carries an incident from writing a venue parser out of public docs, recorded in
//! `vike_bridge_core::key_permissions`' module doc as *the `canWithdraw` trap*.
//!
//! # ⚠ It is the TESTNET socket, and it cannot be anything else
//!
//! `vike_deribit::transport::TESTNET_WS` is spelled outright, exactly as
//! `deribit_reconcile_smoke.rs` spells it. Unlike okx — where demo and mainnet share one host and
//! differ by a HEADER — this venue separates them by URL, so a probe that named the wrong constant
//! would be obvious rather than silent.
//!
//! Read-only, double-gated (network + `DERIBIT_DEMO_*`), self-skipping. Places no order: both
//! methods are `get_*` reads, which is also what makes them safe to re-send.

use serde_json::json;

use vike_bridge_core::capture::capture_frame;
use vike_bridge_core::credentials::{
    Credentials, Environment, load_credentials_from, load_workspace_dotenv_from,
};
use vike_deribit::transport::{DeribitOrderTransport, TESTNET_WS};

/// The method under measurement. Deliberately NOT imported from the bridge: `recon_client.rs`'s
/// `PATH_ACCOUNT_SUMMARY` is private, and promoting it would assert this probe is part of the
/// adapter — which is the thing being tested, not a premise.
const ACCOUNT_SUMMARY: &str = "private/get_account_summary";

/// The currency every deribit read here is scoped to, matching `exec::currency_of`'s answer for the
/// mounted `BTC-PERPETUAL`.
const CURRENCY: &str = "BTC";

fn load_demo_creds() -> Option<Credentials> {
    let vars = load_workspace_dotenv_from(std::env::var("VIKE_SETTINGS_DIR").ok().as_deref());
    let creds = load_credentials_from("deribit", Environment::Demo, &vars);
    if creds.is_none() {
        tracing::warn!(target: "vike_deribit", "SKIP: DERIBIT_DEMO creds absent");
    }
    creds
}

/// Send one read and print what the sanitizer made of it. `what` names the variant in the output,
/// because the whole point of this probe is comparing two of them.
fn probe(transport: &mut DeribitOrderTransport, what: &str, params: &serde_json::Value) {
    let raw = match transport.call(ACCOUNT_SUMMARY, params) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(target: "vike_deribit", what, "refused: {}", e.msg);
            println!("\n=== {ACCOUNT_SUMMARY} ({what}) REFUSED ===\ncode {} — {}\n", e.code, e.msg);
            return;
        }
    };

    let sanitized = capture_frame("deribit", "account-summary", &raw);

    println!("\n=== {what}: WHAT THE SANITIZER RECOGNISED AS AN ACCOUNT IDENTIFIER ===");
    if sanitized.redacted_paths.is_empty() {
        println!("  (nothing) — either this response names no account, or it names one under a");
        println!("  spelling REDACT_KEYS does not carry. Read the body below to tell those apart.");
    } else {
        for p in &sanitized.redacted_paths {
            println!("  {p}");
        }
    }

    println!(
        "\n=== {what}: THE SANITIZED BODY (read it: an unrecognised name prints IN THE CLEAR) ==="
    );
    println!("{}", serde_json::to_string_pretty(&sanitized.frame).unwrap_or_default());
}

#[test]
#[ignore = "network + demo creds — a MEASUREMENT, run manually (see module doc)"]
fn does_a_deribit_response_name_the_account() {
    vike_log::test_init();
    let Some(creds) = load_demo_creds() else { return };

    let mut transport =
        DeribitOrderTransport::new(TESTNET_WS, &creds.api_key, &creds.api_secret, None);
    if let Err(e) = transport.connect() {
        tracing::warn!(target: "vike_deribit", "SKIP: demo order-WS auth failed: {e}");
        println!("\n=== AUTH FAILED ===\n{e}\n");
        return;
    }

    // 1) EXACTLY what `fetch_balance` sends today. If the id is here, it is free.
    probe(&mut transport, "as fetch_balance sends it", &json!({"currency": CURRENCY}));

    // 2) The wider form. If the id appears only here, the read is a genuinely new call and this
    //    venue is okx-shaped rather than binance-shaped.
    probe(&mut transport, "extended", &json!({"currency": CURRENCY, "extended": true}));

    println!();
}
