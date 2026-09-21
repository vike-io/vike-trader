//! **DOES A BYBIT RESPONSE NAME THE ACCOUNT?** — a read-only measurement, printed and not committed.
//!
//!     cargo test -p vike-bybit --test bybit_identity_probe -- --ignored --nocapture
//!
//! # Why this exists
//!
//! `vike_mount::book_identity`'s bybit row is `BookIdentity::Undeterminable`: the credential store
//! holds an HMAC key/secret pair and nothing that NAMES an account, so two API keys minted against
//! ONE sub-account are one book and the mount cannot tell. That matters because per-engine risk
//! ceilings then apply twice to one venue ledger, and because the shared-book warning
//! (`vike_mount::shared_books_for`) can say nothing at all.
//!
//! Since 2026-09-19 the mount reads `account.venue_account_id` FIRST, so the moment anything tells
//! the store what the venue answered, the report starts working. What is missing is the answer.
//!
//! # ⚠ Why it is a PROBE and not a fixture capture
//!
//! The obvious instrument is the `VIKE_CAPTURE_FIXTURES=1` capture smoke — but that one places REAL
//! ORDERS (its own doc: rest a limit, cancel it, MARKET BUY, flatten) to provoke private-WS frames,
//! which is a great deal of side effect for a question about a GET. And its output is COMMITTED into
//! `tests/fixtures/captured/`, a directory that ships to the public mirror.
//!
//! This asks one signed GET and PRINTS. Nothing is written, nothing is committed, so the mirror
//! question does not arise at all.
//!
//! # ⚠ It still sanitizes before printing, and that is not belt-and-braces
//!
//! The output goes to a terminal an operator may paste, and to CI logs if anybody ever runs it
//! there. `vike_bridge_core::capture::capture_frame` redacts the known identity keys — including
//! the parent/master spellings added on 2026-09-20 for exactly this measurement — so the answer
//! this probe gives is **`redacted_paths`**: the JSON pointer of every field the sanitizer
//! recognised as an account identifier. That names the field and its location without printing its
//! value.
//!
//! ⚠ **Read the sanitized BODY too, not only the paths.** The list is an enumeration, so a venue
//! field under an unrecognised name prints IN THE CLEAR below. That is the point — it is how a new
//! spelling is discovered — but it means the body is the half that needs a human's eyes before it
//! goes anywhere.
//!
//! # What is being asked
//!
//! `GET /v5/user/query-api` — the one endpoint this workspace has ever named for bybit key
//! introspection (`vike_bridge_core::key_permissions`' module doc cites it for
//! `permissions.Wallet`), called by nothing. Whether its response carries a uid or a parent uid is
//! UNMEASURED here, which is the whole reason for the probe: writing a parser from the venue's
//! public docs is the guess this tree already has an incident about.
//!
//! Read-only and double-gated exactly like every other `*_smoke.rs` here: network plus
//! `BYBIT_DEMO_*` credentials, self-skipping with a warn when absent. It places no order and
//! changes nothing at the venue.

use vike_bridge_core::capture::capture_frame;
use vike_bridge_core::credentials::{
    Credentials, Environment, load_credentials_from, load_workspace_dotenv_from,
};
use vike_bridge_core::signer::BybitV5Signer;
use vike_bybit::perp::DEMO_REST;
use vike_bybit::transport::{BybitTransport, UreqBybitTransport};
use vike_model::clock::now_ms;

/// The candidate endpoint. Named here rather than in the bridge because nothing in production calls
/// it — promoting it to a `const` in `perp.rs` would assert it is part of the adapter, which is the
/// claim this probe exists to test.
const PATH_QUERY_API: &str = "/v5/user/query-api";

/// The double-gate every `*_smoke.rs` in this crate uses.
fn load_demo_creds() -> Option<Credentials> {
    let vars = load_workspace_dotenv_from(std::env::var("VIKE_SETTINGS_DIR").ok().as_deref());
    let creds = load_credentials_from("bybit", Environment::Demo, &vars);
    if creds.is_none() {
        tracing::warn!(target: "vike_bybit", "SKIP: BYBIT_DEMO creds absent");
    }
    creds
}

#[test]
#[ignore = "network + demo creds — a MEASUREMENT, run manually (see module doc)"]
fn does_a_bybit_response_name_the_account() {
    vike_log::test_init();
    let Some(creds) = load_demo_creds() else { return };

    let transport =
        UreqBybitTransport::new().with_rate_gate(vike_bybit::ratelimit::rest_rate_gate());
    let signer = BybitV5Signer::new(&creds, now_ms);

    let raw = match transport.signed(DEMO_REST, PATH_QUERY_API, "GET", &[], &signer) {
        Ok(body) => body,
        Err(e) => {
            // NOT a failure of the test. A refusal is a real answer to "can this key ask?", and the
            // message is the measurement — a permission error and a 404 send the next step to two
            // different places.
            tracing::warn!(target: "vike_bybit", "{PATH_QUERY_API} refused: {e}");
            println!("\n=== {PATH_QUERY_API} REFUSED ===\n{e}\n");
            return;
        }
    };

    let sanitized = capture_frame("bybit", "query-api", &raw);

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
