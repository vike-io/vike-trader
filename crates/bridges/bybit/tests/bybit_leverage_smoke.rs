//! LIVE DEMO smokes for bybit's per-account leverage (decision 0182): the READ half
//! (`crates/bridges/bybit/src/leverage_read.rs`'s `leverage_reader`) and the WRITE-AND-READ-BACK round
//! trip through the bridge's one existing writer (`crates/bridges/bybit/src/perp.rs`'s `BybitPerpRest`,
//! whose `set_leverage` the exec thread fires at every start). Neither places an order.
//!
//!     cargo test -p vike-bybit --test bybit_leverage_smoke -- --ignored --nocapture
//!
//! HAND-RUN ONLY: no workflow lists this file. The parsers in `leverage_read.rs` were written from the
//! venue's DOCUMENTED shapes and nothing in the tree had captured them from the demo host; these are the
//! drill that was owed.
//!
//! * `bybit_leverage_read_smoke` — READ-ONLY. Asks the three GETs the reader makes (`account/info`,
//!   `position/list`, `instruments-info`) and PRINTS each answer through
//!   `vike_bridge_core::capture::capture_frame` (identity keys redacted, structure verbatim: the form a
//!   committed fixture takes), then the verdicts of the pure parsers, then asserts the production reader
//!   answers exactly what the parsers do over the same host and that the account is one the feature
//!   accepts (plain cross margin, one-way, a published maximum). READ THE PRINTED BODY BEFORE PASTING
//!   IT ANYWHERE: the sanitizer is an enumeration of keys.
//! * `bybit_leverage_write_read_back_smoke` — WRITES the demo account's leverage for `SYMBOL`, once, and
//!   puts it back. It reads the prior figure, writes a small different one through
//!   `BybitPerpRest::set_leverage`, polls the reader until the venue holds it (`confirm`, the lane's
//!   own poll), then writes the PRIOR figure and reads that back. The restore is armed BEFORE the write
//!   and also runs from a drop guard, so a panic in between still puts the figure back. It refuses
//!   (self-skips) while the account holds an open position on `SYMBOL`: this feature never moves a
//!   leverage under a live position. ⚠ If a daemon runs this account, `SYMBOL`'s leverage is the smoke's
//!   for a few seconds; the daemon's exec thread writes only at its own start, so nothing fights it.
//!
//! Double-gated like every `*_smoke.rs` here: the store's default DEMO account (`TIER`, resolved through
//! `vike_secrets::smoke_directory`, self-skipping with a `SKIP:` warn when its key pair is absent) and
//! the venue's DEMO host (`DEMO_REST` on every request this file makes, pinned by `assert_demo_only`).

use serde_json::{Value, json};
use vike_bridge_core::capture::print_sanitized;
use vike_bridge_core::leverage_read::LeverageReader;
use vike_bridge_core::signer::BybitV5Signer;
use vike_bridge_core::smoke::leverage_write_read_back;
use vike_bybit::leverage_read::{
    PATH_ACCOUNT_INFO, check_margin_mode, leverage_reader, parse_max_leverage, parse_position,
};
use vike_bybit::perp::{BybitPerpRest, DEMO_REST, PATH_INSTRUMENTS, PATH_POSITION_LIST, endpoints};
use vike_bybit::transport::{BybitTransport, UreqBybitTransport, unwrap_envelope};
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::{AccountTier, SymbolProperties, now_ms};
use vike_secrets::credentials::{Credentials, api_credentials};

const SYMBOL: &str = "BTCUSDT";
/// The only tier this file resolves an account at.
const TIER: AccountTier = AccountTier::Demo;

/// The guard's two keys, checked before any request: the tier the account is resolved at is `demo`, and
/// the host the bridge's own selector answers for "not mainnet" is the demo host this file signs against.
fn assert_demo_only() {
    assert_eq!(TIER, AccountTier::Demo, "a leverage smoke resolves the DEMO account only");
    assert_eq!(endpoints(false).0, DEMO_REST, "the demo host moved: re-check before signing");
}

fn linear() -> [(&'static str, Value); 2] {
    [("category", json!("linear")), ("symbol", json!(SYMBOL))]
}

fn rest_at(creds: &Credentials, leverage: f64) -> BybitPerpRest<UreqBybitTransport> {
    BybitPerpRest {
        category: vike_bybit::data::Category::Linear,
        signer: BybitV5Signer::new(creds, now_ms),
        transport: UreqBybitTransport::new()
            .with_rate_gate(vike_bybit::ratelimit::rest_rate_gate()),
        base_url: DEMO_REST.to_string(),
        symbol: SYMBOL.to_string(),
        properties: SymbolProperties::default(),
        leverage,
    }
}

#[test]
#[ignore = "network + demo creds — run manually (see module doc)"]
fn bybit_leverage_read_smoke() {
    vike_log::test_init();
    assert_demo_only();
    let directory = vike_secrets::smoke_directory();
    let account = directory.credentials_for("bybit", &AccountLabel::Default, TIER);
    let Some(creds) = api_credentials(&account, "bybit") else {
        tracing::warn!(target: "vike_bybit", "SKIP: BYBIT_DEMO creds absent");
        return;
    };

    let transport =
        UreqBybitTransport::new().with_rate_gate(vike_bybit::ratelimit::rest_rate_gate());
    let signer = BybitV5Signer::new(&creds, now_ms);
    let get = |what: &str, path: &str, params: &[(&str, Value)]| {
        let raw = transport
            .signed(DEMO_REST, path, "GET", params, &signer)
            .unwrap_or_else(|e| panic!("GET {path} failed: {e}"));
        print_sanitized("bybit", what, &raw);
        raw
    };
    let info = get("account-info", PATH_ACCOUNT_INFO, &[]);
    let position = get("position-list", PATH_POSITION_LIST, &linear());
    let instrument = get("instruments-info", PATH_INSTRUMENTS, &linear());

    let mode = unwrap_envelope(info).map_err(|e| e.to_string()).and_then(|r| check_margin_mode(&r));
    let held = unwrap_envelope(position)
        .map_err(|e| e.to_string())
        .and_then(|r| parse_position(&r, SYMBOL));
    let max = parse_max_leverage(&instrument, SYMBOL);
    println!(
        "\n=== VERDICTS ({SYMBOL}, DEMO) ===\nmargin mode : {mode:?}\nheld        : {held:?}\nvenue max   : {max:?}\n"
    );

    // The production reader, over its own transport, must answer what the parsers did over this one.
    let reader = leverage_reader(&account, TIER, SYMBOL).expect("a bybit reader for the account");
    assert_eq!(reader.check_account(), mode, "reader and parser disagree on the margin mode");
    assert_eq!(reader.held(), held, "reader and parser disagree on what is held");
    assert_eq!(reader.venue_max(), max, "reader and parser disagree on the maximum");

    assert_eq!(
        mode,
        Ok(()),
        "the demo account is not one this feature accepts (see the body above)"
    );
    let held = held.expect("the position row is not one this reader accepts (see the body above)");
    assert!(held.held >= 1.0, "{held:?}");
    assert!(matches!(max, Ok(Some(m)) if m >= 1.0), "no usable venue maximum: {max:?}");
    tracing::info!(target: "vike_bybit", "bybit leverage read smoke green: mode, held and maximum all read live");
}

#[test]
#[ignore = "network + demo creds + WRITES the demo leverage, then restores it — run manually (see module doc)"]
fn bybit_leverage_write_read_back_smoke() {
    vike_log::test_init();
    assert_demo_only();
    let directory = vike_secrets::smoke_directory();
    let account = directory.credentials_for("bybit", &AccountLabel::Default, TIER);
    let Some(creds) = api_credentials(&account, "bybit") else {
        tracing::warn!(target: "vike_bybit", "SKIP: BYBIT_DEMO creds absent");
        return;
    };

    let reader = leverage_reader(&account, TIER, SYMBOL).expect("a bybit reader for the account");
    let write = |figure| rest_at(&creds, figure).set_leverage();
    match leverage_write_read_back(&reader, SYMBOL, "DEMO", "leverage", write) {
        Ok((prior, target)) => {
            tracing::info!(target: "vike_bybit", "bybit leverage write/read-back smoke green: {prior}x -> {target}x -> {prior}x")
        }
        Err(skip) => tracing::warn!(target: "vike_bybit", "SKIP: {skip}"),
    }
}
