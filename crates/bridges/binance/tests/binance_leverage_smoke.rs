//! LIVE DEMO smokes for binance USDⓈ-M futures' per-account leverage (decision 0182): the READ half
//! (`crates/bridges/binance/src/leverage_read.rs`'s `leverage_reader`) and the WRITE-AND-READ-BACK round
//! trip through the bridge's one existing writer (`crates/bridges/binance/src/perp.rs`'s
//! `BinancePerpRest`, whose `set_leverage` posts `POST /fapi/v1/leverage` and which the exec thread fires
//! at every start). Neither places an order.
//!
//!     cargo test -p vike-binance --test binance_leverage_smoke -- --ignored --nocapture
//!
//! HAND-RUN ONLY: no workflow lists this file. `positionRisk` is read by the reconcile client
//! (`binance_reconcile_smoke.rs`), but `leverageBracket` had only ever been proven as the unauthenticated
//! `-2014` answer; these are the drill that was owed.
//!
//! * `binance_leverage_read_smoke` — READ-ONLY. Asks the two GETs the reader makes (`positionRisk`, the
//!   signed `leverageBracket`) and PRINTS each answer through `vike_bridge_core::capture::capture_frame`
//!   (identity keys redacted, structure verbatim: the form a committed fixture takes), then the verdicts
//!   of the pure parsers, then asserts the production reader answers exactly what the parsers do and
//!   that the account is one the feature accepts (one-way, cross). The bracket is OPTIONAL by design
//!   (an unreadable one is "no maximum known"), so a missing maximum is printed loudly, not asserted.
//!   READ THE PRINTED BODY BEFORE PASTING IT ANYWHERE: the sanitizer is an enumeration of keys.
//! * `binance_leverage_write_read_back_smoke` — WRITES the demo account's leverage for `API_SYMBOL`,
//!   once, and puts it back. It reads the prior figure, writes a small different one through
//!   `BinancePerpRest::set_leverage`, polls the reader until the venue holds it (`confirm`, the lane's
//!   own poll), then writes the PRIOR figure and reads that back. The restore is armed BEFORE the write
//!   and also runs from a drop guard, so a panic in between still puts the figure back. It refuses
//!   (self-skips) while the account holds an open position on `API_SYMBOL`. ⚠ If a daemon runs this
//!   account, `API_SYMBOL`'s leverage is the smoke's for a few seconds; the daemon's exec thread writes
//!   only at its own start.
//!
//! Double-gated like every `*_smoke.rs` here: the store's default DEMO account (`TIER`, resolved through
//! `vike_secrets::smoke_directory`, self-skipping with a `SKIP:` warn when its key pair is absent) and
//! the venue's DEMO futures host (`DEMO_FAPI_REST` on every request this file makes, pinned by
//! `assert_demo_only`).

use vike_binance::family::perp_series_symbol;
use vike_binance::leverage_read::{
    PATH_LEVERAGE_BRACKET, leverage_reader, parse_bracket_max, parse_position_risk,
};
use vike_binance::perp::{BinancePerpRest, DEMO_FAPI_REST, PATH_POSITIONS};
use vike_bridge_core::capture::capture_frame;
use vike_bridge_core::leverage_read::{LeverageReader, confirm, same_leverage};
use vike_bridge_core::signer::BinanceHmacSigner;
use vike_bridge_core::transport::{RestTransport, UreqTransport};
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::{AccountTier, SymbolProperties, now_ms};
use vike_secrets::credentials::{Credentials, api_credentials};

/// The instrument as the venue names it on the wire — what the fapi REST clients are built on.
const API_SYMBOL: &str = "BTCUSDT";
/// The only tier this file resolves an account at.
const TIER: AccountTier = AccountTier::Demo;

/// The guard's two keys, checked before any request: the tier the account is resolved at is `demo`, and
/// the futures host this file signs against still names the demo one.
fn assert_demo_only() {
    assert_eq!(TIER, AccountTier::Demo, "a leverage smoke resolves the DEMO account only");
    assert!(DEMO_FAPI_REST.contains("demo-fapi"), "{DEMO_FAPI_REST} is not a demo host");
}

fn transport() -> UreqTransport {
    UreqTransport::new("binance").with_rate_gate(vike_binance::ratelimit::perp_rest_gate())
}

/// Print one answer the way a fixture would hold it: sanitized, and with what was redacted listed.
fn show(what: &str, raw: &serde_json::Value) {
    let s = capture_frame("binance", what, raw);
    println!(
        "\n=== {what}: sanitized answer (read it before pasting: an unrecognised key prints in the clear) ===\nredacted: {:?}\n{}",
        s.redacted_paths,
        serde_json::to_string_pretty(&s.frame).unwrap_or_default()
    );
}

fn rest_at(
    creds: &Credentials,
    leverage: f64,
) -> BinancePerpRest<BinanceHmacSigner, UreqTransport> {
    BinancePerpRest {
        link_id: None,
        signer: BinanceHmacSigner::new(creds, now_ms),
        transport: transport(),
        base_url: DEMO_FAPI_REST.to_string(),
        algo_coids: Default::default(),
        symbol: API_SYMBOL.to_string(),
        properties: SymbolProperties::default(),
        leverage,
    }
}

/// Puts the prior leverage back: explicitly (`run`, whose failure the test reports) or, if the test
/// panics first, from `Drop`. Never panics itself (a panic in `drop` during unwinding aborts).
struct Restore {
    rest: BinancePerpRest<BinanceHmacSigner, UreqTransport>,
    done: bool,
}

impl Restore {
    fn run(&mut self) -> Result<(), String> {
        self.rest.set_leverage().map_err(|e| e.to_string())?;
        self.done = true;
        Ok(())
    }
}

impl Drop for Restore {
    fn drop(&mut self) {
        if self.done {
            return;
        }
        match self.run() {
            Ok(()) => {
                eprintln!("restored {API_SYMBOL} to {}x from the drop guard", self.rest.leverage)
            }
            Err(why) => eprintln!(
                "RESTORE FAILED: set the DEMO account's {API_SYMBOL} leverage back to {}x by hand: {why}",
                self.rest.leverage
            ),
        }
    }
}

#[test]
#[ignore = "network + demo creds — run manually (see module doc)"]
fn binance_leverage_read_smoke() {
    vike_log::test_init();
    assert_demo_only();
    let directory = vike_secrets::smoke_directory();
    let account = directory.credentials_for("binance", &AccountLabel::Default, TIER);
    let Some(creds) = api_credentials(&account, "binance") else {
        tracing::warn!(target: "vike_binance", "SKIP: BINANCE_DEMO creds absent");
        return;
    };

    let transport = transport();
    let signer = BinanceHmacSigner::new(&creds, now_ms);
    let params = [("symbol", API_SYMBOL.to_string())];
    let position = transport
        .signed(DEMO_FAPI_REST, PATH_POSITIONS, "GET", &params, &signer)
        .unwrap_or_else(|e| panic!("GET {PATH_POSITIONS} failed: {e}"));
    show("position-risk", &position);
    // The bracket is optional to the reader: a refusal here is an ANSWER to print, not a failure.
    let bracket = transport.signed(DEMO_FAPI_REST, PATH_LEVERAGE_BRACKET, "GET", &params, &signer);
    let bracket_max = match &bracket {
        Ok(body) => {
            show("leverage-bracket", body);
            parse_bracket_max(body, API_SYMBOL)
        }
        Err(e) => {
            println!("\n=== leverage-bracket REFUSED ===\n{e}\n");
            None
        }
    };

    let held = parse_position_risk(&position, API_SYMBOL);
    println!(
        "\n=== VERDICTS ({API_SYMBOL}, DEMO) ===\nheld (mode + leverage): {held:?}\nbracket max           : {bracket_max:?}\n"
    );
    if bracket_max.is_none() {
        println!(
            "NOTE: no maximum known — the bracket was refused or its shape is not the one \
             `parse_bracket_max` reads. The reader treats that as 'unknown' and the poll is the \
             judge of a rejected figure; paste the body above if it was an answer."
        );
    }

    // The production reader, over its own transport, must answer what the parsers did over this one.
    let reader = leverage_reader(&account, TIER, &perp_series_symbol(API_SYMBOL))
        .expect("a binance reader for the account");
    assert_eq!(
        reader.check_account(),
        held.clone().map(|_| ()),
        "reader and parser disagree on the mode"
    );
    assert_eq!(reader.held(), held, "reader and parser disagree on what is held");
    assert_eq!(reader.venue_max(), Ok(bracket_max), "reader and parser disagree on the maximum");

    let held =
        held.expect("the positionRisk row is not one this feature accepts (see the body above)");
    assert!(held.held >= 1.0, "{held:?}");
    tracing::info!(target: "vike_binance", "binance leverage read smoke green: mode and held read live");
}

#[test]
#[ignore = "network + demo creds + WRITES the demo leverage, then restores it — run manually (see module doc)"]
fn binance_leverage_write_read_back_smoke() {
    vike_log::test_init();
    assert_demo_only();
    let directory = vike_secrets::smoke_directory();
    let account = directory.credentials_for("binance", &AccountLabel::Default, TIER);
    let Some(creds) = api_credentials(&account, "binance") else {
        tracing::warn!(target: "vike_binance", "SKIP: BINANCE_DEMO creds absent");
        return;
    };

    let reader = leverage_reader(&account, TIER, &perp_series_symbol(API_SYMBOL))
        .expect("a binance reader for the account");
    reader.check_account().unwrap_or_else(|why| panic!("account refused: {why}"));
    let before = reader.held().unwrap_or_else(|why| panic!("held could not be read: {why}"));
    if before.position_open {
        tracing::warn!(target: "vike_binance", "SKIP: the demo account holds an open {API_SYMBOL} position");
        return;
    }
    let prior = before.held;
    let target = if same_leverage(prior, 3.0) { 4.0 } else { 3.0 };
    if let Some(max) = reader.venue_max().expect("the venue maximum") {
        assert!(target <= max, "{target}x is above the venue maximum {max}x");
    }
    println!(
        "\n=== {API_SYMBOL} (DEMO): prior {prior}x, writing {target}x, restoring {prior}x ==="
    );

    let mut restore = Restore { rest: rest_at(&creds, prior), done: false };
    rest_at(&creds, target).set_leverage().expect("set_leverage through the bridge's writer");
    let seen = confirm(&reader, target, std::thread::sleep)
        .unwrap_or_else(|why| panic!("the venue never held the written figure: {why}"));
    println!("read back after the write: {seen}x");
    assert!(same_leverage(seen, target));

    restore.run().unwrap_or_else(|why| {
        panic!(
            "RESTORE FAILED: set {API_SYMBOL} back to {prior}x on the demo account by hand: {why}"
        )
    });
    let restored = confirm(&reader, prior, std::thread::sleep).unwrap_or_else(|why| {
        panic!("the prior figure was not read back after the restore: {why}")
    });
    println!("read back after the restore: {restored}x");
    assert!(same_leverage(restored, prior));
    tracing::info!(target: "vike_binance", "binance leverage write/read-back smoke green: {prior}x -> {target}x -> {prior}x");
}
