//! Pure parse tests for the FXCM `ReconcileClient` report parsers (ReconFactory seam) — NO SDK, NO
//! network, just representative JSON arrays shaped exactly like the `fcshim.cpp` reconcile table
//! snapshots (`forex_connect_orders` → the Orders table, `forex_connect_trades` → the Trades table). This pure layer is the
//! proven deliverable: the live extraction is native ForexConnect FFI (the C++ table readers + the
//! `fc_*` calls) that CI cannot build (no SDK) and that only the `#[ignore]`d live smoke exercises, so
//! these offline fixtures over hand-built bodies are what actually gate the mapping logic.
//!
//! Asserts the contract the module doc calls out: the FXCM-instrument (`"EUR/USD"`) → USER name
//! (`"EURUSD"`, decision 0290: its kept row's, the list held as the catalog makes it of the measured Offers table,
//! `vike_fxcm::instrument::names::kept_list_for_tests`) reverse-map + filter, the always-`None` order-client-id, the
//! `BOTH` position side
//! with the sign in `qty` for trades in one direction (a `LONG` and a `SHORT` row for a locked pair), the
//! per-trade net-fold with a size-weighted `average_price`, the
//! synthesized flat position row (never empty), the fill `trade_id` (the live-lane dedup key), and
//! that a malformed / non-array body is a hard `Err`, never a panic.

use vike_fxcm::reconcile::{normalize_order_status, normalize_order_type};
use vike_model::events::{LiquiditySide, PositionSide};
use vike_model::{FillReport, OrderStatusReport, PositionStatusReport};

const SYMBOL: &str = "EURUSD";

// Every parse of this file names a row by its kept row (decision 0290), so each first holds fxcm's kept list.

fn parse_orders(body: &str, symbol: &str) -> Result<Vec<OrderStatusReport>, String> {
    vike_fxcm::instrument::names::kept_list_for_tests();
    vike_fxcm::reconcile::parse_orders(body, symbol)
}

fn parse_positions(body: &str, symbol: &str) -> Result<Vec<PositionStatusReport>, String> {
    vike_fxcm::instrument::names::kept_list_for_tests();
    vike_fxcm::reconcile::parse_positions(body, symbol)
}

/// 2026-10-26 12:00 UTC as an OLE date (the SDK's `getOpenTime()`), and in epoch ms.
const OPENED_OLE: f64 = 46_321.5;
const OPENED_MS: i64 = 1_793_016_000_000;

/// The Trades body as the reconcile client hands it over: every row carries its `open_time` (the client joins it in from
/// `forex_connect_open_trades`) unless the row states its own. The stamp's own tests call the crate's `parse_fills` bare.
fn parse_fills(body: &str, symbol: &str) -> Result<Vec<FillReport>, String> {
    vike_fxcm::instrument::names::kept_list_for_tests();
    let body = match serde_json::from_str::<serde_json::Value>(body) {
        Ok(serde_json::Value::Array(mut rows)) => {
            for row in rows.iter_mut().filter_map(serde_json::Value::as_object_mut) {
                row.entry("open_time").or_insert(OPENED_OLE.into());
            }
            serde_json::Value::Array(rows).to_string()
        }
        _ => body.to_string(),
    };
    vike_fxcm::reconcile::parse_fills(&body, symbol)
}

/// **A position and a fill of the venue's `USOil` come back under the CFD's USER name `USOIL.CFD`** (decision 0290),
/// the mixed-case row's own, and `EUR/USD`'s under `EURUSD`; the old upper-cased engine spelling matches nothing.
#[test]
fn a_mixed_case_cfd_position_and_fill_come_back_under_the_user_name() {
    let body = r#"[
      {"trade_id":"T1","order_id":"O1","instrument":"USOil","buysell":"B","amount":10,"open_rate":70.5},
      {"trade_id":"T2","order_id":"O2","instrument":"EUR/USD","buysell":"S","amount":1000,"open_rate":1.1}
    ]"#;
    let oil = parse_positions(body, "USOIL.CFD").unwrap();
    assert_eq!(oil.len(), 1);
    assert_eq!((oil[0].symbol.as_str(), oil[0].qty), ("USOIL.CFD", 10.0));
    let fills = parse_fills(body, "USOIL.CFD").unwrap();
    assert_eq!((fills.len(), fills[0].symbol.as_str()), (1, "USOIL.CFD"));
    let pair = parse_positions(body, SYMBOL).unwrap();
    assert_eq!((pair[0].symbol.as_str(), pair[0].qty), (SYMBOL, -1000.0));
    let old = parse_positions(body, "USOIL").unwrap();
    assert_eq!(old[0].qty, 0.0, "no row is `USOIL`: the flat row");
}

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
    assert_eq!(o.order_client_id, None, "ForexConnect echoes no client id");
    assert_eq!(o.side, 1, "B -> +1");
    assert_eq!(o.order_type, vike_model::OrderType::Limit);
    assert_eq!(o.qty, 10000.0);
    assert_eq!(o.filled_qty, 0.0, "a working order is unfilled by definition");
    assert_eq!(o.average_price, 0.0);
    assert_eq!(o.status, vike_model::OrderStatus::Accepted, "a resting working order is ACCEPTED");
    assert_eq!(o.timestamp, 0);
}

#[test]
fn sell_stop_entry_order() {
    let body = r#"[{"order_id":"O2","instrument":"EUR/USD","buysell":"S","amount":5000,"type":"SE","status":"W"}]"#;
    let r = parse_orders(body, SYMBOL).unwrap();
    assert_eq!(r[0].side, -1, "S -> -1");
    assert_eq!(r[0].order_type, vike_model::OrderType::Stop);
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
    assert_eq!(p.average_price, 1.0930);
    assert_eq!(p.timestamp, 0);
}

#[test]
fn short_position_is_negative_quantity_still_both_side() {
    let body = r#"[{"trade_id":"T2","order_id":"O2","instrument":"EUR/USD","buysell":"S","amount":3000,"open_rate":1.10,"commission":0.0}]"#;
    let r = parse_positions(body, SYMBOL).unwrap();
    assert_eq!(r[0].qty, -3000.0, "S -> negative net qty");
    assert_eq!(r[0].position_side, PositionSide::Both);
}

#[test]
fn aggregates_multiple_open_trades_into_one_net_row_with_weighted_average() {
    // FXCM is trade-per-open on a netting account: two BUY opens net to one 30000 report,
    // average_price = size-weighted mean = (10000*1.10 + 20000*1.15) / 30000.
    let body = r#"[
        {"trade_id":"A","order_id":"O1","instrument":"EUR/USD","buysell":"B","amount":10000,"open_rate":1.10,"commission":0.0},
        {"trade_id":"B","order_id":"O2","instrument":"EUR/USD","buysell":"B","amount":20000,"open_rate":1.15,"commission":0.0}
    ]"#;
    let r = parse_positions(body, SYMBOL).unwrap();
    assert_eq!(r.len(), 1, "one aggregated row, not two trade rows");
    assert_eq!(r[0].qty, 30000.0);
    assert!((r[0].average_price - (10000.0 * 1.10 + 20000.0 * 1.15) / 30000.0).abs() < 1e-12);
}

/// **Trades in both directions (a locked pair on the HEDGING demo) are a `Long` and a `Short` row**, each at its
/// own trades' rate (owner, 2026-11-11: the locked-pair rule, through `PositionStatusReport::from_legs`). One net
/// row read long 10000 @ 1.1000 + short 4000 @ 1.1100 as 6000 @ 1.10286, the unsigned mean of every rate: a cost
/// basis no trade has, which `hybrid` would have booked. The two rows still sum to the net.
#[test]
fn opposing_open_trades_are_a_long_and_a_short_row_each_at_its_own_rate() {
    let body = r#"[
        {"trade_id":"A","order_id":"O1","instrument":"EUR/USD","buysell":"B","amount":10000,"open_rate":1.1000,"commission":0.0},
        {"trade_id":"B","order_id":"O2","instrument":"EUR/USD","buysell":"S","amount":4000,"open_rate":1.1100,"commission":0.0}
    ]"#;
    let r = parse_positions(body, SYMBOL).unwrap();
    let rows: Vec<_> = r.iter().map(|p| (p.position_side, p.qty)).collect();
    assert_eq!(rows, [(PositionSide::Long, 10000.0), (PositionSide::Short, -4000.0)], "{r:?}");
    assert!(
        (r[0].average_price - 1.1000).abs() < 1e-12 && (r[1].average_price - 1.1100).abs() < 1e-12,
        "{r:?}"
    );
    assert_eq!(r[0].qty + r[1].qty, 6000.0, "the rows still sum to the net");
    assert!(r.iter().all(|p| p.symbol == SYMBOL && p.venue == "fxcm" && p.timestamp == 0));
}

/// A trade with no usable open rate makes its row's price "unknown" (`0.0`), never a blend with a zero rate:
/// reconcile then holds the row and books nothing at it.
#[test]
fn a_trade_with_no_open_rate_leaves_its_row_unpriced() {
    let body = r#"[
        {"trade_id":"A","order_id":"O1","instrument":"EUR/USD","buysell":"B","amount":1000,"open_rate":1.10,"commission":0.0},
        {"trade_id":"B","order_id":"O2","instrument":"EUR/USD","buysell":"B","amount":1000,"commission":0.0}
    ]"#;
    let r = parse_positions(body, SYMBOL).unwrap();
    assert_eq!((r.len(), r[0].qty, r[0].average_price), (1, 2000.0, 0.0), "{r:?}");
}

#[test]
fn positions_filter_other_instrument_then_synthesize_flat() {
    let body = r#"[{"trade_id":"g","order_id":"o","instrument":"GBP/USD","buysell":"B","amount":5000,"open_rate":1.0,"commission":0.0}]"#;
    // no row for the engine symbol -> synthesized flat row (never empty).
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
    assert_eq!(r[0].average_price, 0.0);
    assert_eq!(r[0].timestamp, 0);
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
    assert_eq!(f.order_client_id, None);
    assert_eq!(f.side, -1, "S -> -1");
    assert_eq!(f.last_qty, 10000.0);
    assert_eq!(f.last_price, 1.0955);
    assert_eq!(f.commission, 0.05);
    assert_eq!(f.commission_asset, "");
    assert_eq!(f.liquidity_side, LiquiditySide::Unknown);
    assert_eq!(f.timestamp, OPENED_MS, "the trade's open time in epoch ms");
}

/// **A Trades row with no open time is NOT a fill row** (no `open_time`, or the shim's `0.0` for a time it could not
/// read): a confirmed `MissingFill` books its fill at the row's stamp, so a `0` booked it in 1970. Never "now" either.
#[test]
fn an_open_trade_with_no_open_time_is_not_reported() {
    vike_fxcm::instrument::names::kept_list_for_tests();
    let body = r#"[
        {"trade_id":"U1","order_id":"O1","instrument":"EUR/USD","buysell":"B","amount":1000,"open_rate":1.1,"commission":0.0},
        {"trade_id":"U2","order_id":"O2","instrument":"EUR/USD","buysell":"B","amount":1000,"open_rate":1.1,"commission":0.0,"open_time":0.0},
        {"trade_id":"U3","order_id":"O3","instrument":"EUR/USD","buysell":"B","amount":1000,"open_rate":1.1,"commission":0.0,"open_time":46321.5}
    ]"#;
    let r = vike_fxcm::reconcile::parse_fills(body, SYMBOL).unwrap();
    let got: Vec<(&str, i64)> = r.iter().map(|f| (f.trade_id.as_str(), f.timestamp)).collect();
    assert_eq!(got, [("U3", OPENED_MS)]);
    assert_eq!(
        parse_positions(body, SYMBOL).unwrap()[0].qty,
        3000.0,
        "the position still holds all three"
    );
}

/// The sign of a venue commission is not trusted (`crates/bridges/fxcm/src/execution/commission.rs`'s `charge`): a report of `-0.05`
/// books as a charge of `0.05`, as the exec lane's fill does, never as a rebate.
#[test]
fn a_negative_commission_is_reported_as_a_charge() {
    let body = r#"[
        {"trade_id":"T9","order_id":"O9","instrument":"EUR/USD","buysell":"S","amount":10000,"open_rate":1.0955,"commission":-0.05}
    ]"#;
    assert_eq!(parse_fills(body, SYMBOL).unwrap()[0].commission, 0.05);
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

/// **A REMAINDER is reported under its ROOT's id.** A partial close split `rm-T0` and the rest came back as `rm-R1` (the Trades
/// row's `trade_id_origin` names the old one). The core folded the trade under `rm-T0`, the id the opening fill carried, so a
/// report under `rm-R1` raised a `MissingFill` for a trade it already holds. The size, rate and open order stay the
/// remainder's own.
#[test]
fn a_remainder_is_reported_under_the_trade_it_was_split_from() {
    let body = r#"[
        {"trade_id":"rm-R1","order_id":"O1","instrument":"EUR/USD","buysell":"B","amount":1000,"open_rate":1.1,"commission":0.0,"trade_id_origin":"rm-T0"}
    ]"#;
    let r = parse_fills(body, SYMBOL).unwrap();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].trade_id.as_str(), "rm-T0", "the root, which the core folded");
    assert_eq!(r[0].last_qty, 1000.0, "the remainder's own size");
    assert_eq!(r[0].venue_order_id.as_str(), "O1", "the SAME open order the original had");
}

/// A second split names the first remainder as its origin: it reads as the FIRST trade once a pass has seen the first
/// remainder (or the exec lane heard its split), at any depth.
#[test]
fn a_second_split_reads_as_the_first_trade() {
    let first = r#"[{"trade_id":"rm-B1","order_id":"O","instrument":"EUR/USD","buysell":"B","amount":1500,"open_rate":1.1,"commission":0.0,"trade_id_origin":"rm-B0"}]"#;
    assert_eq!(parse_fills(first, SYMBOL).unwrap()[0].trade_id.as_str(), "rm-B0");
    let second = r#"[{"trade_id":"rm-B2","order_id":"O","instrument":"EUR/USD","buysell":"B","amount":1000,"open_rate":1.1,"commission":0.0,"trade_id_origin":"rm-B1"}]"#;
    assert_eq!(
        parse_fills(second, SYMBOL).unwrap()[0].trade_id.as_str(),
        "rm-B0",
        "the remainder of a remainder is still the trade the opening fill carried"
    );
}

/// A shim older than the key reports no origin, and an empty one is an ordinary trade: every id is its own.
#[test]
fn a_trade_with_no_origin_keeps_its_own_id() {
    let body = r#"[
        {"trade_id":"rm-N1","order_id":"O","instrument":"EUR/USD","buysell":"B","amount":1000,"open_rate":1.1,"commission":0.0},
        {"trade_id":"rm-N2","order_id":"O","instrument":"EUR/USD","buysell":"B","amount":1000,"open_rate":1.1,"commission":0.0,"trade_id_origin":""}
    ]"#;
    let ids: Vec<String> = parse_fills(body, SYMBOL)
        .unwrap()
        .iter()
        .map(|f| f.trade_id.as_str().to_string())
        .collect();
    assert_eq!(ids, ["rm-N1", "rm-N2"]);
}

/// Two open rows never share a root (a split closes its parent). A table that says so anyway reports the trade ONCE: a second
/// report under one id would be booked twice by `hybrid`.
#[test]
fn two_rows_with_one_root_are_reported_once() {
    let body = r#"[
        {"trade_id":"rm-D1","order_id":"O","instrument":"EUR/USD","buysell":"B","amount":500,"open_rate":1.1,"commission":0.0,"trade_id_origin":"rm-D0"},
        {"trade_id":"rm-D2","order_id":"O","instrument":"EUR/USD","buysell":"B","amount":500,"open_rate":1.1,"commission":0.0,"trade_id_origin":"rm-D0"}
    ]"#;
    let r = parse_fills(body, SYMBOL).unwrap();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].trade_id.as_str(), "rm-D0");
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

/// F-7: a `commission` that is not a number is COUNTED (`get_fee_f64`) on the shim's fill envelope
/// and on the reconcile Trades row, not booked as a silent `0.0`. Red before the fix: both read it
/// with `as_f64` and counted nothing.
#[test]
fn a_commission_that_is_not_a_number_is_counted_on_the_fill_and_the_reconcile_read() {
    use serde_json::{Value, json};
    use vike_bridge_core::json::malformed_fee_count;
    let fill = |fee: Value| {
        let envelope = json!({"kind": "fill", "trade_id": "T1", "instrument": "EUR/USD", "side": "B",
                              "amount": 1000.0, "rate": 1.1, "commission": fee, "timestamp": 5});
        vike_bridge_core::execution_drive::first_commission(
            &vike_fxcm::execution::event_mapper::map_fxcm_event(&envelope, "c1"),
        )
    };
    let reconcile = |fee: Value| {
        let row = json!([{"trade_id": "T1", "order_id": "O1", "instrument": "EUR/USD", "buysell": "B",
                          "amount": 1000, "open_rate": 1.1, "commission": fee}]);
        parse_fills(&row.to_string(), SYMBOL).unwrap()[0].commission
    };
    type Read<'a> = &'a dyn Fn(Value) -> f64;
    let legs: [(&str, Read<'_>); 2] = [("fill", &fill), ("reconcile", &reconcile)];
    for (leg, read) in legs {
        let before = malformed_fee_count();
        assert_eq!(read(json!("oops")), 0.0, "{leg}: the fill still books, fee-free");
        assert!(
            malformed_fee_count() > before,
            "{leg}: the unreadable `commission` must be counted"
        );
        assert_eq!(read(json!(0.25)), 0.25, "{leg}: a readable commission is booked as stated");
    }
}

// --- normalizers (also unit-tested inline; a couple here pin the public surface) ----------------

#[test]
fn order_type_and_status_normalizers() {
    assert_eq!(normalize_order_type("LE"), vike_model::OrderType::Limit);
    assert_eq!(normalize_order_type("SE"), vike_model::OrderType::Stop);
    assert_eq!(normalize_order_status("W"), vike_model::OrderStatus::Accepted);
    assert_eq!(normalize_order_status("C"), vike_model::OrderStatus::Canceled);
    assert_eq!(normalize_order_status("F"), vike_model::OrderStatus::Filled);
}

/// A row's number is a JSON NUMBER only (the shim writes numbers): a quoted number, a word, a bool,
/// `null` and an absent key all read `0.0`. Pinned on the edge inputs before the read moved onto
/// `vike_bridge_core::json`.
#[test]
fn a_number_field_reads_a_json_number_only() {
    let qty = |amount: &str| {
        let body = format!(
            r#"[{{"order_id":"O","instrument":"EUR/USD","buysell":"B",{amount}"type":"LE","status":"W"}}]"#
        );
        parse_orders(&body, SYMBOL).unwrap()[0].qty
    };
    assert_eq!(qty(r#""amount":10000,"#), 10000.0);
    assert_eq!(qty(r#""amount":0.5,"#), 0.5);
    for unread in
        [r#""amount":"10000","#, r#""amount":"x","#, r#""amount":true,"#, r#""amount":null,"#, ""]
    {
        assert_eq!(qty(unread), 0.0, "{unread}");
    }
    // A quoted number reads as none, so a row whose rate or amount is quoted states no price or no quantity and is
    // skipped (`vike_bridge_core::event_map::fill_report_is_bookable`), never reported at 0.
    let body = r#"[
        {"trade_id":"N1","order_id":"O1","instrument":"EUR/USD","buysell":"S","amount":-2000,"open_rate":"1.1"},
        {"trade_id":"N2","order_id":"O2","instrument":"EUR/USD","buysell":"B","amount":"3000","open_rate":1.2},
        {"trade_id":"N3","order_id":"O3","instrument":"EUR/USD","buysell":"S","amount":-2000,"open_rate":1.1}
    ]"#;
    let fills = parse_fills(body, SYMBOL).unwrap();
    let read: Vec<(f64, f64)> = fills.iter().map(|f| (f.last_qty, f.last_price)).collect();
    assert_eq!(read, [(2000.0, 1.1)]);
}
