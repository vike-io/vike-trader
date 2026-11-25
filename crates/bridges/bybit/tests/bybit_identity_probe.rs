//! **DOES A BYBIT RESPONSE NAME THE ACCOUNT?** — a read-only measurement, printed and not committed.
//!
//!     cargo test -p vike-bybit --test integration -- --ignored --nocapture bybit_identity_probe::
//!
//! # Why this exists
//!
//! This venue's start declaration (`vike_bybit::adapter::VenueAdapterBybit`'s `declaration`, which
//! carries the bybit row of `crates/vike-gatewayhub/src/trade/adapters/book_identity.rs`'s table) says
//! `BookIdentityRule::Undeterminable`:
//! the credential store
//! holds an HMAC key/secret pair and nothing that NAMES an account, so two API keys minted against
//! ONE sub-account are one book and the start cannot tell. That matters because per-engine risk
//! ceilings then apply twice to one venue ledger, and because the shared-book warning
//! (`vike_tradehub_core::shared_books_for`) can say nothing at all.
//!
//! Since 2026-09-19 the start reads `account.venue_account_id` FIRST, so the moment anything tells
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
//! `GET /v5/user/query-api` — bybit's key introspection, which a live start's withdraw gate reads
//! for `permissions.Wallet` (`vike_bybit::execution::fetch_key_permissions`). Whether its response
//! carries a uid or a parent uid is what this probe asks; writing an identity parser from the
//! venue's public docs is the guess this tree already has an incident about.
//!
//! Read-only and double-gated exactly like every other `*_smoke.rs` here: network plus the
//! default DEMO account's credentials, self-skipping with a warn when absent. It places no order
//! and changes nothing at the venue.

use vike_bridge_core::capture::capture_frame;
use vike_bridge_core::transport::json::RestTransportJson;
use vike_bybit::execution::rest::{PATH_QUERY_API, seed_endpoints};
use vike_bybit::signer::BybitV5Signer;
use vike_bybit::transport::client::rest_transport;
use vike_model::now_ms;

use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;
use vike_secrets::credentials::api_credentials;

#[test]
#[ignore = "network + demo creds — a MEASUREMENT, run manually (see module doc)"]
fn does_a_bybit_response_name_the_account() {
    vike_log::test_init();
    let directory = vike_secrets::smoke_directory();
    let account = directory.credentials_for("bybit", &AccountLabel::Default, AccountTier::Demo);
    let Some(creds) = api_credentials(&account, "bybit") else {
        tracing::warn!(target: "vike_bybit", "SKIP: BYBIT_DEMO creds absent");
        return;
    };

    let transport =
        rest_transport().with_rate_gate(vike_bybit::transport::ratelimit::rest_rate_gate());
    let signer = BybitV5Signer::new(&creds, now_ms);

    let demo = seed_endpoints(AccountTier::Demo).rest; // the demo host, from the release seed's rows
    let raw = match transport.signed(&demo, PATH_QUERY_API, "GET", &[], &signer) {
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
