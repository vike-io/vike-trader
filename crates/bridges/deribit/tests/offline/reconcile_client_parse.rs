//! Pure parse tests for the deribit `ReconcileClient` report parsers (Task 10) — NO network, just
//! representative JSON-RPC RESULT bodies shaped like Deribit's real
//! `private/get_open_orders_by_instrument` / `private/get_positions` /
//! `private/get_user_trades_by_instrument` responses. Asserts the tricky mappings the brief calls
//! out explicitly: `direction` -> signed side, the empty-`label` = externally-placed order
//! SURFACING (kept, with `order_client_id: None`, so `recon::diff::diff` can route it to
//! `Divergence::UnknownOrder` — matching binance's `parse_order_row`), the already-signed
//! position `size`, the fee/fee_currency -> commission/commission_asset fill mapping, and
//! Deribit's own `"M"`/`"T"` liquidity spelling. Order/trade ids use Deribit's real wire FORMAT —
//! plain numeric strings, as its BTC-instrument docs show (`order_id: "146062"`), NOT a currency
//! prefix that mismatched the BTC instruments here.

use vike_deribit::reconcile::{
    parse_account_identity, parse_account_summary_balance, parse_account_summary_coin_cash,
    parse_fee_rate, parse_open_orders, parse_positions, parse_user_trades,
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
        r#"[{{"instrument_name":"{SYMBOL}","order_id":"349280","direction":"sell","label":"vike-order-client-id-1","order_type":"limit","amount":10.0,"filled_amount":0.0,"average_price":0.0,"price":0.045,"order_state":"open","last_update_timestamp":1700000000000}}]"#
    );
    let reports = parse_open_orders(&body).unwrap();
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].side, -1, "sell -> -1");
}

#[test]
fn open_order_maps_every_field_and_seeds_status_from_filled_amount() {
    let body = format!(
        r#"[{{"instrument_name":"{SYMBOL}","order_id":"349281","direction":"buy","label":"vike-order-client-id-2","order_type":"limit","amount":10.0,"filled_amount":4.0,"average_price":0.043,"price":0.045,"order_state":"open","last_update_timestamp":1700000001000}}]"#
    );
    let reports = parse_open_orders(&body).unwrap();
    assert_eq!(reports.len(), 1);
    let r = &reports[0];
    assert_eq!(r.venue, "deribit");
    assert_eq!(r.symbol, SYMBOL);
    assert_eq!(r.venue_order_id.as_str(), "349281");
    assert_eq!(r.order_client_id.as_deref(), Some("vike-order-client-id-2"));
    assert_eq!(r.side, 1, "buy -> +1");
    assert_eq!(r.order_type, vike_model::OrderType::Limit);
    assert_eq!(r.qty, 10.0);
    assert_eq!(r.filled_qty, 4.0);
    assert_eq!(r.average_price, 0.043);
    assert_eq!(
        r.status,
        vike_model::OrderStatus::PartiallyFilled,
        "filled_amount > 0 -> PARTIALLY_FILLED"
    );
    assert_eq!(r.timestamp, 1700000001000);
    // a limit's one term is its `price` (in the option's premium unit, as `order_params_on` sent it)
    assert_eq!(
        (r.price, r.trigger_price, r.trail, r.trail_percent),
        (Some(0.045), None, None, None)
    );
}

#[test]
fn open_order_unfilled_seeds_accepted_status() {
    let body = format!(
        r#"[{{"instrument_name":"{SYMBOL}","order_id":"349282","direction":"buy","label":"vike-order-client-id-3","order_type":"limit","amount":5.0,"filled_amount":0.0,"average_price":0.0,"price":0.045,"order_state":"open","last_update_timestamp":1700000002000}}]"#
    );
    let reports = parse_open_orders(&body).unwrap();
    assert_eq!(reports[0].status, vike_model::OrderStatus::Accepted);
}

/// A COMBO book's limit is the signed net (a credit negative, a flat spread zero) and is stated as
/// given; a plain instrument's price at or below zero is no price. Held over EVERY instrument of the
/// captured `public/get_instruments` answer: a resting limit at -0.5 on its name keeps that price
/// exactly when the row's `kind` is a combo kind (`future_combo`, `option_combo`).
#[test]
fn a_combo_books_limit_is_stated_as_given() {
    let order_at = |name: &str, price: f64| {
        let row = serde_json::json!([{
            "instrument_name": name, "order_id": "1", "direction": "buy", "label": "vike-order-client-id-c",
            "order_type": "limit", "amount": 1.0, "filled_amount": 0.0, "price": price,
            "order_state": "open", "last_update_timestamp": 1_700_000_000_000i64,
        }]);
        parse_open_orders(&row.to_string()).unwrap().remove(0).price
    };
    let combo = "BTC-CSR12-6OCT26-86000_86500";
    assert_eq!(order_at(combo, -0.0015), Some(-0.0015), "a credit combo's negative net");
    assert_eq!(order_at(combo, 0.0), Some(0.0), "a flat combo's zero net");
    assert_eq!(order_at("BTC-FS-5OCT26_PERP", 1199.0), Some(1199.0));
    assert_eq!(order_at("BTC-PERPETUAL", 0.0), None, "a plain zero is no price");

    let captured: serde_json::Value =
        serde_json::from_str(include_str!("../fixtures/deribit_get_instruments.json")).unwrap();
    let mut combo_kinds = std::collections::BTreeSet::new();
    for answer in captured.as_object().expect("the captured answers by request").values() {
        for row in answer["result"].as_array().expect("an answer's result list") {
            let (name, kind) =
                (row["instrument_name"].as_str().unwrap(), row["kind"].as_str().unwrap());
            let is_combo = kind.ends_with("_combo");
            if is_combo {
                combo_kinds.insert(kind);
            }
            assert_eq!(order_at(name, -0.5), is_combo.then_some(-0.5), "{name} ({kind})");
        }
    }
    assert_eq!(combo_kinds.into_iter().collect::<Vec<_>>(), ["future_combo", "option_combo"]);
}

/// The load-bearing case: an order with an empty `label` was never placed by vike (`label` IS
/// our `order_client_id` on submit — see `execution/rest.rs::build_order_params`), but it must SURVIVE
/// in the returned Vec (not be dropped) with `order_client_id: None`, so `recon::diff::diff` —
/// which can only classify orders present in the report slice — routes it to
/// `Divergence::UnknownOrder`. A non-empty label keeps mapping to `Some(...)`.
#[test]
fn empty_label_order_surfaces_as_unknown_order() {
    let body = format!(
        r#"[
        {{"instrument_name":"{SYMBOL}","order_id":"350001","direction":"buy","label":"vike-order-client-id-4","order_type":"limit","amount":1.0,"filled_amount":0.0,"average_price":0.0,"price":0.04,"order_state":"open","last_update_timestamp":1700000003000}},
        {{"instrument_name":"{SYMBOL}","order_id":"350002","direction":"sell","label":"","order_type":"limit","amount":2.0,"filled_amount":0.0,"average_price":0.0,"price":0.05,"order_state":"open","last_update_timestamp":1700000004000}}
    ]"#
    );
    let reports = parse_open_orders(&body).unwrap();
    assert_eq!(reports.len(), 2, "both rows must survive — none dropped");
    assert_eq!(reports[0].venue_order_id.as_str(), "350001");
    assert_eq!(reports[0].order_client_id.as_deref(), Some("vike-order-client-id-4"));
    assert_eq!(reports[1].venue_order_id.as_str(), "350002");
    assert!(
        reports[1].order_client_id.is_none(),
        "empty-label row must surface with order_client_id: None, not be dropped"
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
    assert_eq!(reports[0].average_price, 0.04);
    assert_eq!(reports[0].position_side, PositionSide::Both, "options are one-way");
    assert_eq!(
        reports[0].timestamp, 1700000005000,
        "no per-row venue timestamp -> caller's now_ms"
    );
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
        r#"[{{"instrument_name":"{SYMBOL}","trade_id":"2696060","order_id":"349280","direction":"sell","label":"vike-order-client-id-9","amount":10.0,"price":0.045,"fee":0.0002,"fee_currency":"BTC","liquidity":"M","timestamp":1700000007000}}]"#
    );
    let reports = parse_user_trades(&body).unwrap();
    assert_eq!(reports.len(), 1);
    let r = &reports[0];
    assert_eq!(r.venue, "deribit");
    assert_eq!(r.symbol, SYMBOL);
    assert_eq!(r.trade_id.as_str(), "2696060");
    assert_eq!(r.venue_order_id.as_str(), "349280");
    assert_eq!(r.order_client_id.as_deref(), Some("vike-order-client-id-9"), "label echoes back");
    assert_eq!(r.side, -1, "sell -> -1");
    assert_eq!(r.last_qty, 10.0);
    assert_eq!(r.last_price, 0.045);
    assert_eq!(r.commission, 0.0002);
    assert_eq!(r.commission_asset, "BTC");
    assert_eq!(r.liquidity_side, LiquiditySide::Maker, "\"M\" -> Maker");
    assert_eq!(r.timestamp, 1700000007000);
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
        r.order_client_id, None,
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

/// An inverse engine's coin cash (decision 0258) is `balance` plus the session's realized PnL, which Deribit
/// moves into `balance` only at the 08:00 UTC settlement; `session_upl` stays out (the engine values its own
/// positions). The worked example: 0.1 BTC, a long closed at a gain of 0.0015151515 BTC less 0.0000111363 BTC
/// of fees, mid-session.
#[test]
fn an_inverse_mounts_coin_cash_adds_the_sessions_realized_pnl() {
    let body = r#"{"currency":"BTC","balance":0.1,"session_rpl":0.0015040152,"session_upl":0.0,"equity":0.1015040152}"#;
    let cash = parse_account_summary_coin_cash(body).unwrap().expect("a balance");
    assert!((cash - 0.101_504_015_2).abs() < 1e-12, "{cash}");
    assert_eq!(
        parse_account_summary_balance(body).unwrap(),
        Some(0.1),
        "the plain read is unchanged"
    );
    let no_rpl = r#"{"currency":"BTC","balance":0.1}"#;
    assert_eq!(
        parse_account_summary_coin_cash(no_rpl).unwrap(),
        Some(0.1),
        "no session PnL reads zero"
    );
    assert_eq!(parse_account_summary_coin_cash(r#"{"session_rpl":1.0}"#).unwrap(), None);
    assert!(parse_account_summary_coin_cash("[]").is_err());
}

// --- the account identity ------------------------------------------------------------------
//
// ⚠ **THE BODIES BELOW ARE THE MEASURED SHAPE**, trimmed to the fields the parser reads.
// `crates/bridges/deribit/tests/deribit_smokes/deribit_identity_probe.rs` asked the live demo endpoint on
// 2026-09-20 and its module doc carries the full result; the VALUES here are invented, because the
// real ones are the operator's account and this file ships to the public mirror.

/// ⚠ **THE MEASUREMENT THAT DECIDED THE SHAPE OF THE CODE.** The PLAIN
/// `private/get_account_summary` — exactly what `fetch_balance` sends on every reconcile pass —
/// names no account at all, which is why `fetch_account_identity` is a separate `extended` call
/// rather than a free read off a body somebody already fetched.
#[test]
fn the_plain_account_summary_names_no_account() {
    let body = r#"{"currency":"BTC","balance":1.05,"equity":1.08,"available_funds":0.95}"#;
    assert_eq!(parse_account_identity(body).unwrap(), None);
}

/// The `extended` form answers the account — a NUMBER, rendered as the string
/// `vike_secrets::AccountRow::venue_account_id` holds.
#[test]
fn the_extended_account_summary_names_the_account() {
    let body = r#"{"currency":"BTC","balance":1.05,"id":84789,"type":"main","username":"x"}"#;
    let id = parse_account_identity(body).unwrap().expect("id present");
    assert_eq!(id.id, "84789");
}

/// A sub-account's book is ITS OWN id, whatever `type` it states.
#[test]
fn a_subaccount_reports_its_own_id() {
    let body = r#"{"id":84790,"type":"subaccount"}"#;
    let id = parse_account_identity(body).unwrap().expect("id present");
    assert_eq!(id.id, "84790", "the BOOK is this account");
}

/// A blank or absent `id` degrades to *not yet known* rather than failing a start — the same
/// fail-soft rule `parse_account_summary_balance` has.
#[test]
fn a_blank_account_id_is_not_yet_known() {
    assert_eq!(parse_account_identity(r#"{"id":"   ","type":"main"}"#).unwrap(), None);
    assert_eq!(parse_account_identity(r#"{"type":"main"}"#).unwrap(), None);
    assert_eq!(parse_account_identity(r#"{"id":null}"#).unwrap(), None);
}

#[test]
fn account_identity_malformed_body_is_an_error_not_a_panic() {
    assert!(parse_account_identity("not json").is_err());
    assert!(parse_account_identity("[]").is_err(), "an array, not an object, is an error");
    assert!(parse_account_identity("null").is_err());
}

/// F-7: a `fee` the venue sent that is not a number is COUNTED (`get_fee_f64`) on the stream's
/// `user.trades` leg and on the reconcile read of the same rows, not booked as a silent `0.0`. Red
/// before the fix: both read it with `get_f64` and counted nothing.
#[test]
fn a_fee_that_is_not_a_number_is_counted_on_the_stream_and_the_reconcile_read() {
    use serde_json::{Value, json};
    use vike_bridge_core::json::malformed_fee_count;
    let row = |fee: Value| {
        json!({"trade_id": "ETH-1", "order_id": "9", "label": "c1", "instrument_name": "ETH-PERPETUAL",
               "direction": "sell", "amount": 1.0, "price": 2000.0, "fee": fee, "fee_currency": "ETH",
               "liquidity": "T", "state": "filled", "timestamp": 5})
    };
    let stream = |fee: Value| {
        vike_bridge_core::execution_drive::first_commission(
            &vike_deribit::execution::event_mapper::map_deribit_trade(
                &row(fee),
                "deribit",
                "ETH-PERPETUAL",
            ),
        )
    };
    let reconcile =
        |fee: Value| parse_user_trades(&json!([row(fee)]).to_string()).unwrap()[0].commission;
    type Read<'a> = &'a dyn Fn(Value) -> f64;
    let legs: [(&str, Read<'_>); 2] = [("stream", &stream), ("reconcile", &reconcile)];
    for (leg, read) in legs {
        let before = malformed_fee_count();
        assert_eq!(read(json!("oops")), 0.0, "{leg}: the fill still books, fee-free");
        assert!(malformed_fee_count() > before, "{leg}: the unreadable `fee` must be counted");
        assert_eq!(read(json!(0.25)), 0.25, "{leg}: a readable fee is booked as stated");
    }
}
