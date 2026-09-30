//! Does the venue still accept an **L1 phantom-agent** class transfer at all?
//!
//! # The question, and why it is worth one request
//!
//! `crates/bridges/hyperliquid/src/transfer.rs` records a falsified verdict — an agent (API) wallet
//! may NOT sign the **user-signed** `usdClassTransfer` for its master, because the venue resolves a
//! user-signed action to the recovered address's own account — and it leaves exactly ONE reopener:
//!
//! > The Rust SDK's `class_transfer` is signed with the **L1 phantom-agent** scheme
//! > (`sign_l1_action`) — the scheme agents genuinely do sign … an agent may well be able to move a
//! > master's USDC between spot and perp through it. **Unverified.**
//!
//! The reopener's premise is sound: `crates/bridges/hyperliquid/src/exec.rs`'s
//! `HyperliquidExecutionClient::spawn` documents that an agent wallet's L1 action resolves to its
//! MASTER at the venue, which is the opposite of the user-signed finding. So if an L1 class transfer
//! exists, the capability plausibly exists with it.
//!
//! This file asks whether it exists. It is not a permissioning test and must not be read as one.
//!
//! # Why a probe rather than a reading of the SDK
//!
//! The L1 form appears in the official **Rust** SDK and in NEITHER the official Python SDK nor the
//! API documentation, whose "Transfer from Spot account to Perp account" section describes only the
//! user-signed `usdClassTransfer`. Two independent reports against the Rust SDK's `master`
//! (`hyperliquid-dex/hyperliquid-rust-sdk` PR #145, issue #183) say the venue answers the L1 form
//! with **HTTP 422** — rejected on shape, before signature verification — and the same repository's
//! merged change for the sibling `sendAsset` action describes the direction of travel outright:
//! *"replacing legacy L1 action signing with EIP-712 typed data for proper API compatibility."*
//!
//! That is three pieces of evidence and none of them is ours. This file makes it ours.
//!
//! # Reading the answer
//!
//! | reply | meaning |
//! |---|---|
//! | HTTP **422** | the venue does not recognise this action's shape. The reopener is CLOSED: there is no L1 route, and the user-signed route resolves to the signer. An agent cannot move a master's USDC either way. |
//! | a 200 naming an ADDRESS | the action is LIVE, and which address it names is the next question — the reopener stands and the work resumes |
//! | anything else | a non-answer; see [`verdict`] |
//!
//! # Nothing moves, in either branch
//!
//! [`PROBE_USDC`] is set far above any testnet balance, so a LIVE action is refused on funds rather
//! than executed. A 422 executes nothing by construction. The account is untouched on every path.
//!
//! # Run it
//!
//! ```sh
//! cargo test -p vike-hyperliquid --test hyperliquid_l1_class_transfer_probe \
//!     -- --ignored --nocapture --test-threads=1
//! ```
//!
//! ⚠ Do not run it beside the other hyperliquid smokes: they sign with the same demo key and take
//! their nonce from the wall clock, and the venue dedups nonces per recovered signer across
//! processes.

use serde::Serialize;

use vike_bridge_core::credentials::load_workspace_dotenv_from;
use vike_bridge_core::http::blocking_agent;
use vike_bridge_core::transport::read_body_ambiguous;
use vike_hyperliquid::config::{self, Env, HlCredentials, Network};
use vike_hyperliquid::consts::{MAINNET_EXCHANGE, TESTNET_EXCHANGE};
use vike_hyperliquid::signing::Signer;

/// Only `HYPERLIQUID_DEMO*` keys are allowed anywhere near this file.
const DEMO_PREFIX: &str = "HYPERLIQUID_DEMO";

/// The transfer amount, in the L1 form's base units (USDC × 1e6) — **one million USDC**.
///
/// Deliberately far above any testnet balance. If the action turns out to be live, the venue
/// refuses it on funds; it never executes. The probe asks whether the action EXISTS, and an
/// existence answer does not need an executable amount.
const PROBE_USDC: u64 = 1_000_000_000_000;

/// The L1 action exactly as `hyperliquid-dex/hyperliquid-rust-sdk`'s `src/exchange/actions.rs`
/// declares it — transcribed, not invented, on the same footing as
/// `crates/bridges/hyperliquid/tests/signing_vectors.rs`'s ported vectors.
///
/// ⚠ **Field declaration order is the signature.** The L1 hash is taken over
/// `rmp_serde::to_vec_named`, which emits a msgpack map keyed by field name in declaration order,
/// so reordering these fields does not produce an invalid signature — it produces a valid signature
/// over a different action.
///
/// This type lives in the test rather than in `crates/bridges/hyperliquid/src/signing/action.rs`
/// ON PURPOSE: the product must not grow a variant for an action the venue may no longer accept,
/// and `Signer::sign_l1_action` is generic over `Serialize`, so nothing needs it to.
#[derive(Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
enum L1Action {
    SpotUser(SpotUser),
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SpotUser {
    class_transfer: ClassTransfer,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ClassTransfer {
    /// USDC WITHOUT decimals — the SDK's own comment is *"payload expects usdc without decimals"*,
    /// and it multiplies by `1e6` before assigning. The user-signed twin sends a decimal STRING
    /// instead; that the two disagree on the amount's very type is part of why this probe exists.
    usdc: u64,
    to_perp: bool,
}

/// Load the demo credentials with every non-demo key discarded BEFORE the loader sees them.
///
/// The sanctioned way to run this is `VIKE_SETTINGS_DIR` pointed at the real settings directory,
/// which also holds the LIVE keys. The `retain` is what makes `Env::Live` structurally unreachable
/// from this file — the same shape `hyperliquid_permissioning_smoke.rs` uses and for the same
/// reason.
fn demo_creds() -> Option<HlCredentials> {
    let mut vars = load_workspace_dotenv_from(std::env::var("VIKE_SETTINGS_DIR").ok().as_deref());
    vars.retain(|k, _| k.starts_with(DEMO_PREFIX));
    assert!(
        vars.keys().all(|k| k.starts_with(DEMO_PREFIX)),
        "the demo-only filter let a non-demo key through: {:?}",
        vars.keys().collect::<Vec<_>>()
    );

    let creds = config::load(Env::Demo, &vars);
    if creds.is_none() {
        tracing::warn!(
            target: "vike_hyperliquid",
            "SKIP: {DEMO_PREFIX}_PRIVATE_KEY absent from the credential store \
             (point VIKE_SETTINGS_DIR at the settings directory holding it)"
        );
    }
    creds
}

/// Milliseconds since the epoch — the nonce, matching what the one-shot actions in this crate use.
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("the system clock is after 1970")
        .as_millis() as u64
}

/// POST raw `/exchange` bytes to TESTNET and read the verdict back as (status, VERBATIM body).
///
/// A bare agent rather than `HyperliquidTransport::post`, which is `pub(crate)` and parses the body
/// into a `Value`: this probe's whole result is the venue's reply exactly as sent, and the bytes it
/// posts are bytes the production API deliberately offers no way to build. Same rustls agent and
/// same body reader the real transport uses, so only the bytes differ.
fn post_testnet_exchange(payload: &[u8]) -> (u16, String) {
    let agent = blocking_agent();
    let mut resp = agent
        .post(TESTNET_EXCHANGE)
        .header("Content-Type", "application/json")
        .send(payload)
        .expect("testnet /exchange is reachable");
    let status = resp.status().as_u16();
    let text = read_body_ambiguous(&mut resp).expect("response body reads");
    (status, text)
}

/// Print the verdict and return whether the venue RECOGNISED the action.
///
/// ⚠ A 422 is this probe's INFORMATIVE answer, not its failure. That inversion is the whole point
/// and is stated here because every other file in this crate treats a 422 as a protocol break to
/// fix — there, the shape is supposed to be right; here, whether it is right at all is the question.
fn verdict(status: u16, body: &str) -> bool {
    println!("\n--- venue reply: HTTP {status} ---\n{body}\n---");

    if status == 422 {
        println!(
            "VERDICT: the venue does NOT recognise the L1 `spotUser`/`classTransfer` action.\n\
             It is refused on shape, before signature verification, so this says nothing about \
             permissions — and it does not need to. With no L1 route and a user-signed route that \
             resolves to the SIGNER, an agent wallet cannot move a master's USDC between spot and \
             perp at all. `transfer.rs`'s reopener is CLOSED."
        );
        return false;
    }

    println!(
        "VERDICT: the venue ANSWERED this action rather than refusing its shape — the L1 form is \
         LIVE and `transfer.rs`'s reopener STANDS.\n\
         The next question is which ACCOUNT the reply names: the agent's own, or its master's. \
         Nothing moved here ({PROBE_USDC} base units is far above any testnet balance), so the \
         work resumes from the design rather than from this file."
    );
    true
}

/// The premise every network case in this file rests on, and it runs offline.
#[test]
fn the_demo_tier_is_testnet_and_only_testnet() {
    assert!(matches!(Env::Demo.network(), Network::Testnet));
    assert_eq!(Env::Demo.prefix(), DEMO_PREFIX);

    let (_, exchange, _) = Network::Testnet.urls();
    assert_eq!(exchange, TESTNET_EXCHANGE);
    assert_ne!(exchange, MAINNET_EXCHANGE, "this file may never address mainnet");
}

/// Post the L1 class transfer once and record what the venue says.
#[test]
#[ignore = "network (Hyperliquid TESTNET) + demo creds — run manually; see the module doc"]
fn does_the_venue_still_accept_an_l1_class_transfer() {
    vike_log::test_init();

    let Some(creds) = demo_creds() else { return };
    assert!(
        matches!(creds.network, Network::Testnet),
        "the DEMO tier must resolve to testnet; refusing to sign anything otherwise"
    );

    let signer = Signer::from_private_key(&creds.private_key, Network::Testnet)
        .expect("the demo private key parses");

    let action = L1Action::SpotUser(SpotUser {
        class_transfer: ClassTransfer { usdc: PROBE_USDC, to_perp: true },
    });
    let nonce = now_ms();
    let signature = signer.sign_l1_action(&action, nonce, None, None);

    // The envelope the L1 path posts: action, the SAME nonce the hash covered, and the signature.
    // `vaultAddress` is omitted rather than sent as null — this probe names no vault.
    let body = serde_json::json!({
        "action": serde_json::to_value(&action).expect("the action serializes"),
        "nonce": nonce,
        "signature": serde_json::to_value(&signature).expect("the signature serializes"),
    });
    let payload = serde_json::to_vec(&body).expect("the envelope serializes");

    println!("\n--- posting (nonce {nonce}) ---\n{body:#}\n---");
    let (status, reply) = post_testnet_exchange(&payload);

    let recognised = verdict(status, &reply);

    // Neither outcome is a test failure: both are answers, and the run's product is the printed
    // verdict. What WOULD be a failure is an unreadable one.
    assert!(
        status == 422 || !reply.trim().is_empty(),
        "the venue neither refused the shape nor said anything — this run proves nothing either \
         way, and must not be recorded as if it did (HTTP {status})"
    );
    let _ = recognised;
}
