//! **DOES AN OKX RESPONSE NAME THE ACCOUNT?** — a read-only measurement, printed and not committed.
//!
//!     cargo test -p vike-okx --test okx_identity_probe -- --ignored --nocapture
//!
//! The okx sibling of `crates/bridges/bybit/tests/bybit_identity_probe.rs`, whose module doc carries
//! the full argument for why this is a PROBE rather than a fixture capture.
//!
//! # What is being asked, and what this tree already knows about it
//!
//! `GET /api/v5/account/config` — named exactly ONCE in the whole workspace, in
//! `vike_bridge_core::key_permissions`' module doc, for its `perm` CSV field, and **called by
//! nothing**. Whether its response also carries a uid (and a main/parent uid, which is what would
//! separate a sub-account from its master) is unmeasured here. That is the point: writing a parser
//! from the venue's public docs is the guess this tree has an incident about, recorded in that same
//! module as *the `canWithdraw` trap*.
//!
//! ⚠ Demo and mainnet share one host on this venue — the environment is the `x-simulated-trading`
//! HEADER, not the URL. `UreqOkxTransport::new(true)` is the demo one, spelled exactly as
//! `okx_reconcile_smoke.rs` spells it, so this probe cannot accidentally ask MAINNET.
//!
//! Read-only, double-gated (network + `OKX_DEMO_*`), self-skipping. Places no order.

use vike_bridge_core::capture::capture_frame;
use vike_bridge_core::credentials::{
    Credentials, Environment, load_credentials_from, load_workspace_dotenv_from,
};
use vike_bridge_core::signer::OkxV5Signer;
use vike_model::clock::now_ms;
use vike_okx::perp::REST;
use vike_okx::transport::{OkxTransport, UreqOkxTransport};

/// The candidate. Named here rather than in the bridge because nothing in production calls it —
/// promoting it to a `const` there would assert it is part of the adapter, which is what this probe
/// exists to test.
const PATH_ACCOUNT_CONFIG: &str = "/api/v5/account/config";

fn load_demo_creds() -> Option<Credentials> {
    let vars = load_workspace_dotenv_from(std::env::var("VIKE_SETTINGS_DIR").ok().as_deref());
    let creds = load_credentials_from("okx", Environment::Demo, &vars);
    if creds.is_none() {
        tracing::warn!(target: "vike_okx", "SKIP: OKX_DEMO creds absent");
    }
    creds
}

#[test]
#[ignore = "network + demo creds — a MEASUREMENT, run manually (see module doc)"]
fn does_an_okx_response_name_the_account() {
    vike_log::test_init();
    let Some(creds) = load_demo_creds() else { return };

    // `true` = demo, through the header. See the ⚠ in the module doc.
    let transport =
        UreqOkxTransport::new(true).with_rate_gate(vike_okx::ratelimit::rest_rate_gate());
    let signer = OkxV5Signer::new(&creds, now_ms);

    let raw = match transport.signed(REST, PATH_ACCOUNT_CONFIG, "GET", &[], &signer) {
        Ok(body) => body,
        Err(e) => {
            tracing::warn!(target: "vike_okx", "{PATH_ACCOUNT_CONFIG} refused: {e}");
            println!("\n=== {PATH_ACCOUNT_CONFIG} REFUSED ===\n{e}\n");
            return;
        }
    };

    let sanitized = capture_frame("okx", "account-config", &raw);

    println!("\n=== WHAT THE SANITIZER RECOGNISED AS AN ACCOUNT IDENTIFIER ===");
    if sanitized.redacted_paths.is_empty() {
        println!("  (nothing) — either this response names no account, or it names one under a");
        println!("  spelling REDACT_KEYS does not carry. Read the body below to tell those apart.");
    } else {
        for p in &sanitized.redacted_paths {
            println!("  {p}");
        }
    }

    println!("\n=== THE SANITIZED BODY (read it: an unrecognised name prints IN THE CLEAR) ===");
    println!("{}", serde_json::to_string_pretty(&sanitized.frame).unwrap_or_default());
    println!();
}
