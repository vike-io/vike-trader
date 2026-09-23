//! Pure parse tests for the FXCM `ReconClient` report parsers (ReconFactory seam) — NO SDK, NO
//! network, just representative JSON arrays shaped exactly like the `fcshim.cpp` reconcile table
//! snapshots (`fc_orders` → the Orders table, `fc_trades` → the Trades table). This pure layer is the
//! proven deliverable: the live extraction is native ForexConnect FFI (the C++ table readers + the
//! `fc_*` calls) that CI cannot build (no SDK) and that only the `#[ignore]`d live smoke exercises, so
//! these offline fixtures over hand-built bodies are what actually gate the mapping logic.
//!
//! Asserts the contract the module doc calls out: the FXCM-instrument (`"EUR/USD"`) → canonical
//! symbol (`"EURUSD"`) reverse-map + filter, the always-`None` client-order-id, the always-`BOTH`
//! position side with the sign in `qty`, the per-trade net-fold with a size-weighted `avg_px`, the
//! synthesized flat position row (never empty), the fill `trade_id` (the live-lane dedup key), and
//! that a malformed / non-array body is a hard `Err`, never a panic.

use vike_fxcm::recon_client::{
    normalize_order_status, normalize_order_type, parse_fills, parse_orders, parse_positions,
};
use vike_model::events::{LiquiditySide, PositionSide};

const SYMBOL: &str = "EURUSD";

// --- parse_orders ------------------------------------------------------------------------------

#[test]
fn parses_limit_entry_order_every_field() {
    let body = r#"[
        {"order_id":"O123","instrument":"EUR/USD","buysell":"B","amount":10000,"type":"LE","status":"W"}
    ]"#;
    let r = parse_orders(body, SYMBOL).unwrap();
    assert_eq!(r.len(), 1);
    let o = &r[0];
    assert_eq!(o.venue, "fxcm");
    assert_eq!(o.symbol, SYMBOL, "the canonical symbol is stamped back on the row");
    assert_eq!(o.venue_order_id.as_str(), "O123");
    assert_eq!(o.client_order_id, None, "ForexConnect echoes no client id");
    assert_eq!(o.side, 1, "B -> +1");
    assert_eq!(o.order_type, "limit");
    assert_eq!(o.qty, 10000.0);
    assert_eq!(o.filled_qty, 0.0, "a working order is unfilled by definition");
    assert_eq!(o.avg_px, 0.0);
    assert_eq!(o.status, "ACCEPTED", "a resting working order is ACCEPTED");
    assert_eq!(o.ts, 0);
}

#[test]
fn sell_stop_entry_order() {
    let body = r#"[{"order_id":"O2","instrument":"EUR/USD","buysell":"S","amount":5000,"type":"SE","status":"W"}]"#;
    let r = parse_orders(body, SYMBOL).unwrap();
    assert_eq!(r[0].side, -1, "S -> -1");
    assert_eq!(r[0].order_type, "stop");
}

#[test]
fn orders_filter_out_other_instruments() {
    let body = r#"[
        {"order_id":"keep","instrument":"EUR/USD","buysell":"B","amount":1000,"type":"LE","status":"W"},
        {"order_id":"drop","instrument":"GBP/USD","buysell":"B","amount":1000,"type":"LE","status":"W"}
    ]"#;
    let r = parse_orders(body, SYMBOL).unwrap();
    assert_eq!(r.len(), 1, "only the mounted-symbol row survives");
    assert_eq!(r[0].venue_order_id.as_str(), "keep");
}

#[test]
fn empty_orders_array_is_empty_not_synthesized() {
    // Orders (unlike positions) do NOT synthesize a placeholder — no resting order is the truth.
    assert!(parse_orders("[]", SYMBOL).unwrap().is_empty());
}

#[test]
fn orders_malformed_body_is_an_error_not_a_panic() {
    assert!(parse_orders("not json", SYMBOL).is_err());
    assert!(parse_orders("{}", SYMBOL).is_err(), "a JSON object (not an array) is an error");
    assert!(parse_orders("null", SYMBOL).is_err());
}

// --- parse_positions ---------------------------------------------------------------------------

#[test]
fn parses_single_long_position_every_field() {
    let body = r#"[
        {"trade_id":"T1","order_id":"O1","instrument":"EUR/USD","buysell":"B","amount":10000,"open_rate":1.0930,"commission":0.0}
    ]"#;
    let r = parse_positions(body, SYMBOL).unwrap();
    assert_eq!(r.len(), 1);
    let p = &r[0];
    assert_eq!(p.venue, "fxcm");
    assert_eq!(p.symbol, SYMBOL);
    assert_eq!(p.position_side, PositionSide::Both, "FX net account keys on BOTH");
    assert_eq!(p.qty, 10000.0, "B -> positive net qty");
    assert_eq!(p.avg_px, 1.0930);
    assert_eq!(p.ts, 0);
}

#[test]
fn short_position_is_negative_qty_still_both_side() {
    let body = r#"[{"trade_id":"T2","order_id":"O2","instrument":"EUR/USD","buysell":"S","amount":3000,"open_rate":1.10,"commission":0.0}]"#;
    let r = parse_positions(body, SYMBOL).unwrap();
    assert_eq!(r[0].qty, -3000.0, "S -> negative net qty");
    assert_eq!(r[0].position_side, PositionSide::Both);
}

#[test]
fn aggregates_multiple_open_trades_into_one_net_row_with_weighted_avg() {
    // FXCM is trade-per-open on a netting account: two BUY opens net to one 30000 report,
    // avg_px = size-weighted mean = (10000*1.10 + 20000*1.15) / 30000.
    let body = r#"[
        {"trade_id":"A","order_id":"O1","instrument":"EUR/USD","buysell":"B","amount":10000,"open_rate":1.10,"commission":0.0},
        {"trade_id":"B","order_id":"O2","instrument":"EUR/USD","buysell":"B","amount":20000,"open_rate":1.15,"commission":0.0}
    ]"#;
    let r = parse_positions(body, SYMBOL).unwrap();
    assert_eq!(r.len(), 1, "one aggregated row, not two trade rows");
    assert_eq!(r[0].qty, 30000.0);
    assert!((r[0].avg_px - (10000.0 * 1.10 + 20000.0 * 1.15) / 30000.0).abs() < 1e-12);
}

#[test]
fn nets_opposing_open_trades() {
    // A BUY 3000 and a SELL 1000 on the same symbol net to +2000.
    let body = r#"[
        {"trade_id":"A","order_id":"O1","instrument":"EUR/USD","buysell":"B","amount":3000,"open_rate":1.10,"commission":0.0},
        {"trade_id":"B","order_id":"O2","instrument":"EUR/USD","buysell":"S","amount":1000,"open_rate":1.12,"commission":0.0}
    ]"#;
    let r = parse_positions(body, SYMBOL).unwrap();
    assert_eq!(r[0].qty, 2000.0);
}

#[test]
fn positions_filter_other_instrument_then_synthesize_flat() {
    let body = r#"[{"trade_id":"g","order_id":"o","instrument":"GBP/USD","buysell":"B","amount":5000,"open_rate":1.0,"commission":0.0}]"#;
    // no row for the mounted symbol -> synthesized flat row (never empty).
    let r = parse_positions(body, SYMBOL).unwrap();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].symbol, SYMBOL);
    assert_eq!(r[0].qty, 0.0);
    assert_eq!(r[0].position_side, PositionSide::Both);
}

#[test]
fn empty_trades_array_synthesizes_a_flat_row() {
    let r = parse_positions("[]", SYMBOL).unwrap();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].qty, 0.0);
    assert_eq!(r[0].avg_px, 0.0);
    assert_eq!(r[0].ts, 0);
}

#[test]
fn positions_malformed_body_is_an_error_not_a_panic() {
    assert!(parse_positions("not json", SYMBOL).is_err());
    assert!(parse_positions("{}", SYMBOL).is_err());
    assert!(parse_positions("null", SYMBOL).is_err());
}

// --- parse_fills -------------------------------------------------------------------------------

#[test]
fn parses_open_trade_as_a_fill_every_field() {
    let body = r#"[
        {"trade_id":"T9","order_id":"O9","instrument":"EUR/USD","buysell":"S","amount":10000,"open_rate":1.0955,"commission":0.05}
    ]"#;
    let r = parse_fills(body, SYMBOL).unwrap();
    assert_eq!(r.len(), 1);
    let f = &r[0];
    assert_eq!(f.venue, "fxcm");
    assert_eq!(f.symbol, SYMBOL);
    assert_eq!(f.trade_id.as_str(), "T9", "the trade id is the live-lane dedup key");
    assert_eq!(f.venue_order_id.as_str(), "O9");
    assert_eq!(f.client_order_id, None);
    assert_eq!(f.side, -1, "S -> -1");
    assert_eq!(f.last_qty, 10000.0);
    assert_eq!(f.last_px, 1.0955);
    assert_eq!(f.commission, 0.05);
    assert_eq!(f.commission_asset, "");
    assert_eq!(f.liquidity_side, LiquiditySide::Unknown);
    assert_eq!(f.ts, 0);
}

#[test]
fn fills_filter_out_other_instruments() {
    let body = r#"[
        {"trade_id":"keep","order_id":"o","instrument":"EUR/USD","buysell":"B","amount":1000,"open_rate":1.1,"commission":0.0},
        {"trade_id":"drop","order_id":"o","instrument":"USD/JPY","buysell":"B","amount":1000,"open_rate":150.0,"commission":0.0}
    ]"#;
    let r = parse_fills(body, SYMBOL).unwrap();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].trade_id.as_str(), "keep");
}

/// A Trades row with no `trade_id` (absent or `""`) is SKIPPED, not reported with an empty id. This
/// gates the `TradeId::new` handling in `parse_fills`: reverting it to a permissive `.into()` turns
/// the asserted 1 back into 3.
///
/// The module's own contract is that this row's `trade_id` is the SAME id the async fill lane
/// reports, so the reconcile dedup lines up; an id-less row has nothing to line up with. It can never
/// match `seen_trade_ids`, so it FABRICATES a `MissingFill` divergence — one of the two kinds the
/// `hybrid` policy AUTO-APPLIES — which books the trade a second time with no operator in front of
/// it.
#[test]
fn trade_rows_without_a_trade_id_are_skipped() {
    let body = r#"[
        {"order_id":"o1","instrument":"EUR/USD","buysell":"B","amount":1000,"open_rate":1.1,"commission":0.0},
        {"trade_id":"","order_id":"o2","instrument":"EUR/USD","buysell":"B","amount":1000,"open_rate":1.1,"commission":0.0},
        {"trade_id":"T3","order_id":"o3","instrument":"EUR/USD","buysell":"B","amount":1000,"open_rate":1.1,"commission":0.0}
    ]"#;
    let r = parse_fills(body, SYMBOL).unwrap();
    assert_eq!(r.len(), 1, "the absent-id and empty-id rows are both skipped");
    assert_eq!(r[0].trade_id, "T3", "only the identifiable trade is reported");
}

#[test]
fn empty_trades_array_yields_no_fills() {
    assert!(parse_fills("[]", SYMBOL).unwrap().is_empty());
}

#[test]
fn fills_malformed_body_is_an_error_not_a_panic() {
    assert!(parse_fills("not json", SYMBOL).is_err());
    assert!(parse_fills("{}", SYMBOL).is_err());
}

// --- normalizers (also unit-tested inline; a couple here pin the public surface) ----------------

#[test]
fn order_type_and_status_normalizers() {
    assert_eq!(normalize_order_type("LE"), "limit");
    assert_eq!(normalize_order_type("SE"), "stop");
    assert_eq!(normalize_order_status("W"), "ACCEPTED");
    assert_eq!(normalize_order_status("C"), "CANCELED");
    assert_eq!(normalize_order_status("F"), "FILLED");
}
