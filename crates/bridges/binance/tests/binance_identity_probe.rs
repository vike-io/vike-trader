//! **DOES A BINANCE RESPONSE NAME THE ACCOUNT?** — a read-only measurement, printed and not committed.
//!
//!     cargo test -p vike-binance --test binance_identity_probe -- --ignored --nocapture
//!
//! The binance sibling of `crates/bridges/bybit/tests/bybit_identity_probe.rs`, whose module doc
//! carries the full argument for why this is a PROBE rather than a fixture capture (the capture
//! smoke places real orders, and its output is committed into a directory that ships to the public
//! mirror; this asks one GET and prints).
//!
//! # Why binance is the interesting one
//!
//! It is the venue where an account-wide authenticated read ALREADY happens at every live mount:
//! `crates/vike-mount/src/arming.rs`'s `resolve_fee_schedule` calls `fetch_fee_rates`, which on the
//! spot lane issues the parameterless `GET /api/v3/account` — the whole body in hand — and
//! `parse_commission_rates` keeps `commissionRates` and drops everything else. The startup
//! preflight reaches the same endpoint through `fetch_balance`, which keeps one USDT number and
//! then discards even that (`authed_read_probes`' closure ends `.map(|_| ())`).
//!
//! So if that body names the account, the cost of recording it is ZERO extra requests — which is
//! the finding this probe exists to establish or refute. `vike_mount::book_identity`'s binance row
//! says only an authenticated call can answer, and names no field, because nothing in this tree has
//! ever looked.
//!
//! Read-only, double-gated (network + `BINANCE_DEMO_*`), self-skipping. Places no order.

use vike_binance::spot::{DEMO_REST, PATH_ACCOUNT};
use vike_bridge_core::capture::capture_frame;
use vike_bridge_core::credentials::{
    Credentials, Environment, load_credentials_from, load_workspace_dotenv_from,
};
use vike_bridge_core::signer::BinanceHmacSigner;
use vike_bridge_core::transport::{RestTransport, UreqTransport};
use vike_model::clock::now_ms;

fn load_demo_creds() -> Option<Credentials> {
    let vars = load_workspace_dotenv_from(std::env::var("VIKE_SETTINGS_DIR").ok().as_deref());
    let creds = load_credentials_from("binance", Environment::Demo, &vars);
    if creds.is_none() {
        tracing::warn!(target: "vike_binance", "SKIP: BINANCE_DEMO creds absent");
    }
    creds
}

#[test]
#[ignore = "network + demo creds — a MEASUREMENT, run manually (see module doc)"]
fn does_a_binance_response_name_the_account() {
    vike_log::test_init();
    let Some(creds) = load_demo_creds() else { return };

    let transport = UreqTransport::new("binance");
    let signer = BinanceHmacSigner::new(&creds, now_ms);

    let raw = match transport.signed(DEMO_REST, PATH_ACCOUNT, "GET", &[], &signer) {
        Ok(body) => body,
        Err(e) => {
            // A refusal is an ANSWER, not a test failure — an expired key and a permission gap send
            // the next step to different places, and the message is the measurement.
            tracing::warn!(target: "vike_binance", "{PATH_ACCOUNT} refused: {e}");
            println!("\n=== {PATH_ACCOUNT} REFUSED ===\n{e}\n");
            return;
        }
    };

    let sanitized = capture_frame("binance", "account", &raw);

    println!("\n=== WHAT THE SANITIZER RECOGNISED AS AN ACCOUNT IDENTIFIER ===");
    if sanitized.redacted_paths.is_empty() {
        println!("  (nothing) — either this response names no account, or it names one under a");
        println!("  spelling REDACT_KEYS does not carry. Read the body below to tell those apart.");
    } else {
        for p in &sanitized.redacted_paths {
            println!("  {p}");
        }
    }

    // ⚠ `balances` is dropped from the print: it is long, it is not what is being asked, and it is
    // the operator's position. Everything else prints, because an unrecognised identity field is
    // exactly what must be visible.
    let mut body = sanitized.frame.clone();
    if let Some(obj) = body.as_object_mut() {
        obj.insert("balances".into(), serde_json::json!("<omitted by the probe>"));
    }
    println!("\n=== THE SANITIZED BODY (read it: an unrecognised name prints IN THE CLEAR) ===");
    println!("{}", serde_json::to_string_pretty(&body).unwrap_or_default());
    println!();
}
