//! Pure parse tests for the deribit `ReconClient` report parsers (Task 10) — NO network, just
//! representative JSON-RPC RESULT bodies shaped like Deribit's real
//! `private/get_open_orders_by_instrument` / `private/get_positions` /
//! `private/get_user_trades_by_instrument` responses. Asserts the tricky mappings the brief calls
//! out explicitly: `direction` -> signed side, the empty-`label` = externally-placed order
//! SURFACING (kept, with `client_order_id: None`, so `recon::diff::diff` can route it to
//! `Divergence::UnknownOrder` — matching binance's `parse_order_row`), the already-signed
//! position `size`, the fee/fee_currency -> commission/commission_asset fill mapping, and
//! Deribit's own `"M"`/`"T"` liquidity spelling. Order/trade ids use Deribit's real wire FORMAT —
//! plain numeric strings, as its BTC-instrument docs show (`order_id: "146062"`), NOT a currency
//! prefix that mismatched the BTC instruments here.

use vike_deribit::recon_client::{
    parse_account_summary_balance, parse_fee_rate, parse_open_orders, parse_positions,
    parse_user_trades,
};
use vike_model::FeeSchedule;
use vike_model::events::{LiquiditySide, PositionSide};

/// `public/get_instrument` result -> per-instrument FeeSchedule from maker/taker_commission.
/// Options: 0.03% maker == taker (3 bps).
#[test]
fn parses_option_instrument_commissions() {
    let body = r#"{"instrument_name":"BTC-25AUG23-30000-C","kind":"option","maker_commission":0.0003,"taker_commission":0.0003,"tick_size":0.0005}"#;
    let s = parse_fee_rate(body).unwrap().expect("commissions present");
    // 0.0003 is not an exact f64, so compare against the same from_fractions the parser uses
    // (≈3 bps) rather than a literal 3.0 that float×10000 would miss.
    assert_eq!(s, FeeSchedule::from_fractions(0.0003, 0.0003));
    assert!((s.commission(false, 1.0, 10_000.0) - 3.0).abs() < 1e-9);
}

/// Futures/perp: 0% maker / 0.05% taker (0 / 5 bps) — proves the per-instrument-class difference.
#[test]
fn parses_perp_instrument_commissions() {
    let body = r#"{"instrument_name":"BTC-PERPETUAL","kind":"future","maker_commission":0.0,"taker_commission":0.0005}"#;
    let s = parse_fee_rate(body).unwrap().expect("commissions present");
    assert_eq!(s, FeeSchedule::PercentMakerTaker { maker_bps: 0.0, taker_bps: 5.0 });
}

/// Absent commission fields -> None (fail-soft to the static default).
#[test]
fn absent_commissions_is_none() {
    assert_eq!(parse_fee_rate(r#"{"instrument_name":"X"}"#).unwrap(), None);
}

const SYMBOL: &str = "BTC-25AUG23-30000-C";

#[test]
fn parses_open_order_direction_into_signed_side() {
    let body = format!(
        r#"[{{"instrument_name":"{SYMBOL}","order_id":"349280","direction":"sell","label":"vike-coid-1","order_type":"limit","amount":10.0,"filled_amount":0.0,"average_price":0.0,"price":0.045,"order_state":"open","last_update_timestamp":1700000000000}}]"#
    );
    let reports = parse_open_orders(&body).unwrap();
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].side, -1, "sell -> -1");
}

#[test]
fn open_order_maps_every_field_and_seeds_status_from_filled_amount() {
    let body = format!(
        r#"[{{"instrument_name":"{SYMBOL}","order_id":"349281","direction":"buy","label":"vike-coid-2","order_type":"limit","amount":10.0,"filled_amount":4.0,"average_price":0.043,"price":0.045,"order_state":"open","last_update_timestamp":1700000001000}}]"#
    );
    let reports = parse_open_orders(&body).unwrap();
    assert_eq!(reports.len(), 1);
    let r = &reports[0];
    assert_eq!(r.venue, "deribit");
    assert_eq!(r.symbol, SYMBOL);
    assert_eq!(r.venue_order_id.as_str(), "349281");
    assert_eq!(r.client_order_id.as_deref(), Some("vike-coid-2"));
    assert_eq!(r.side, 1, "buy -> +1");
    assert_eq!(r.order_type, "limit");
    assert_eq!(r.qty, 10.0);
    assert_eq!(r.filled_qty, 4.0);
    assert_eq!(r.avg_px, 0.043);
    assert_eq!(r.status, "PARTIALLY_FILLED", "filled_amount > 0 -> PARTIALLY_FILLED");
    assert_eq!(r.ts, 1700000001000);
}

#[test]
fn open_order_unfilled_seeds_accepted_status() {
    let body = format!(
        r#"[{{"instrument_name":"{SYMBOL}","order_id":"349282","direction":"buy","label":"vike-coid-3","order_type":"limit","amount":5.0,"filled_amount":0.0,"average_price":0.0,"price":0.045,"order_state":"open","last_update_timestamp":1700000002000}}]"#
    );
    let reports = parse_open_orders(&body).unwrap();
    assert_eq!(reports[0].status, "ACCEPTED");
}

/// The load-bearing case: an order with an empty `label` was never placed by vike (`label` IS
/// our `client_order_id` on submit — see `client.rs::build_order_params`), but it must SURVIVE
/// in the returned Vec (not be dropped) with `client_order_id: None`, so `recon::diff::diff` —
/// which can only classify orders present in the report slice — routes it to
/// `Divergence::UnknownOrder`. A non-empty label keeps mapping to `Some(...)`.
#[test]
fn empty_label_order_surfaces_as_unknown_order() {
    let body = format!(
        r#"[
        {{"instrument_name":"{SYMBOL}","order_id":"350001","direction":"buy","label":"vike-coid-4","order_type":"limit","amount":1.0,"filled_amount":0.0,"average_price":0.0,"price":0.04,"order_state":"open","last_update_timestamp":1700000003000}},
        {{"instrument_name":"{SYMBOL}","order_id":"350002","direction":"sell","label":"","order_type":"limit","amount":2.0,"filled_amount":0.0,"average_price":0.0,"price":0.05,"order_state":"open","last_update_timestamp":1700000004000}}
    ]"#
    );
    let reports = parse_open_orders(&body).unwrap();
    assert_eq!(reports.len(), 2, "both rows must survive — none dropped");
    assert_eq!(reports[0].venue_order_id.as_str(), "350001");
    assert_eq!(reports[0].client_order_id.as_deref(), Some("vike-coid-4"));
    assert_eq!(reports[1].venue_order_id.as_str(), "350002");
    assert!(
        reports[1].client_order_id.is_none(),
        "empty-label row must surface with client_order_id: None, not be dropped"
    );
}

#[test]
fn positions_keep_already_signed_size_and_flat_rows() {
    let body = format!(
        r#"[
        {{"instrument_name":"{SYMBOL}","kind":"option","direction":"buy","size":0.5,"average_price":0.04,"mark_price":0.045}},
        {{"instrument_name":"ETH-25AUG23-2000-P","kind":"option","direction":"buy","size":1.0,"average_price":100.0,"mark_price":110.0}},
        {{"instrument_name":"{SYMBOL}","kind":"option","direction":"zero","size":0.0,"average_price":0.0,"mark_price":0.045}}
    ]"#
    );
    let reports = parse_positions(&body, SYMBOL, 1700000005000).unwrap();
    assert_eq!(reports.len(), 2, "only rows matching the requested symbol survive the filter");
    assert_eq!(reports[0].qty, 0.5);
    assert_eq!(reports[0].avg_px, 0.04);
    assert_eq!(reports[0].position_side, PositionSide::Both, "options are one-way");
    assert_eq!(reports[0].ts, 1700000005000, "no per-row venue timestamp -> caller's now_ms");
    assert_eq!(reports[1].qty, 0.0, "the flat row must survive parsing, not be filtered out");
}

#[test]
fn position_delta_is_parsed_for_a_perp_row_and_none_when_absent() {
    // A Deribit PERP position row: `size` is USD NOTIONAL (inverse contract), but `delta` carries
    // the true COIN delta — surfaced so a linear hedge leg folds into net greeks (Wave 5d). The
    // fetch no longer filters `kind:"option"`, so a future/perp row now flows through parsing.
    let body = r#"[
        {"instrument_name":"BTC-PERPETUAL","kind":"future","direction":"buy","size":52000.0,"average_price":104000.0,"mark_price":104000.0,"delta":0.5}
    ]"#;
    let reports = parse_positions(body, "BTC-PERPETUAL", 1700000010000).unwrap();
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].qty, 52000.0, "perp size is USD notional, kept as-is (already signed)");
    assert_eq!(reports[0].delta, Some(0.5), "the venue coin delta is surfaced onto the report");

    // A row WITHOUT `delta` parses to `delta: None` — never a fabricated value.
    let body_no_delta = format!(
        r#"[{{"instrument_name":"{SYMBOL}","kind":"option","direction":"buy","size":0.5,"average_price":0.04,"mark_price":0.045}}]"#
    );
    let reports = parse_positions(&body_no_delta, SYMBOL, 1700000011000).unwrap();
    assert_eq!(reports[0].delta, None, "no `delta` field -> None, never fabricated");
}

#[test]
fn position_short_size_stays_negative_never_resigned_by_direction() {
    let body = format!(
        r#"[{{"instrument_name":"{SYMBOL}","kind":"option","direction":"sell","size":-0.75,"average_price":0.04,"mark_price":0.045}}]"#
    );
    let reports = parse_positions(&body, SYMBOL, 1700000006000).unwrap();
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].qty, -0.75, "already signed by the venue — never re-signed by direction");
}

#[test]
fn user_trade_maps_fee_and_liquidity_and_label() {
    let body = format!(
        r#"[{{"instrument_name":"{SYMBOL}","trade_id":"2696060","order_id":"349280","direction":"sell","label":"vike-coid-9","amount":10.0,"price":0.045,"fee":0.0002,"fee_currency":"BTC","liquidity":"M","timestamp":1700000007000}}]"#
    );
    let reports = parse_user_trades(&body).unwrap();
    assert_eq!(reports.len(), 1);
    let r = &reports[0];
    assert_eq!(r.venue, "deribit");
    assert_eq!(r.symbol, SYMBOL);
    assert_eq!(r.trade_id.as_str(), "2696060");
    assert_eq!(r.venue_order_id.as_str(), "349280");
    assert_eq!(r.client_order_id.as_deref(), Some("vike-coid-9"), "label echoes back");
    assert_eq!(r.side, -1, "sell -> -1");
    assert_eq!(r.last_qty, 10.0);
    assert_eq!(r.last_px, 0.045);
    assert_eq!(r.commission, 0.0002);
    assert_eq!(r.commission_asset, "BTC");
    assert_eq!(r.liquidity_side, LiquiditySide::Maker, "\"M\" -> Maker");
    assert_eq!(r.ts, 1700000007000);
}

#[test]
fn user_trade_taker_liquidity_and_missing_label() {
    let body = format!(
        r#"[{{"instrument_name":"{SYMBOL}","trade_id":"2696061","order_id":"349290","direction":"buy","label":"","amount":3.0,"price":0.05,"fee":0.0001,"fee_currency":"BTC","liquidity":"T","timestamp":1700000008000}}]"#
    );
    let reports = parse_user_trades(&body).unwrap();
    let r = &reports[0];
    assert_eq!(r.side, 1, "buy -> +1");
    assert_eq!(r.liquidity_side, LiquiditySide::Taker, "\"T\" -> Taker");
    assert_eq!(
        r.client_order_id, None,
        "empty label -> None (fill rows are NOT skipped, unlike orders)"
    );
}

#[test]
fn malformed_body_is_an_error_not_a_panic() {
    assert!(parse_open_orders("not json").is_err());
    assert!(parse_open_orders("{}").is_err(), "an object, not an array, is an error");
    assert!(parse_positions("null", SYMBOL, 0).is_err());
    assert!(parse_user_trades("42").is_err());
}

/// `private/get_account_summary` -> the currency's `balance` (raw wallet cash — realized,
/// EXCLUDING unrealized PnL from open option positions; `equity` is the mark-to-market,
/// position-inclusive figure and is deliberately NOT used here — see the parser's doc comment for
/// why `balance` is the cross-venue-consistent pin).
#[test]
fn parses_account_summary_balance() {
    let body = r#"{"currency":"BTC","balance":1.05000000,"equity":1.08123456,"available_funds":0.95000000,"margin_balance":1.06,"total_pl":0.03}"#;
    assert_eq!(parse_account_summary_balance(body).unwrap(), Some(1.05));
}

#[test]
fn account_summary_missing_balance_field_is_none() {
    let body = r#"{"currency":"BTC","equity":1.08123456}"#;
    assert_eq!(parse_account_summary_balance(body).unwrap(), None);
}

#[test]
fn account_summary_malformed_body_is_an_error_not_a_panic() {
    assert!(parse_account_summary_balance("not json").is_err());
    assert!(parse_account_summary_balance("[]").is_err(), "an array, not an object, is an error");
    assert!(parse_account_summary_balance("null").is_err());
}
