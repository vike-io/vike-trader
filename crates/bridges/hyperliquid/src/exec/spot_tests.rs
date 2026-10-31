//! The spot rules of `super`: no reduce-only flag on a spot wire, and a base-token fee netted out of the fill.

use serde_json::json;

use super::*;
use crate::base::symbology::Symbology;
use crate::exec::event_mapper::map_user_fills;

fn symbology() -> Symbology {
    let meta = json!({"universe":[{"name":"BTC","szDecimals":5,"maxLeverage":40}]});
    let spot = json!({
        "tokens":[
            {"name":"USDC","szDecimals":8,"index":0},
            {"name":"HYPE","szDecimals":2,"index":150}
        ],
        "universe":[{"name":"@107","tokens":[150,0],"index":107}]
    });
    Symbology::from_meta(&meta, &spot)
}

fn hype() -> InstrumentRef {
    symbology().by_symbol("HYPE/USDC").expect("HYPE/USDC").clone()
}

fn btc() -> InstrumentRef {
    symbology().by_symbol("BTC").expect("BTC").clone()
}

#[test]
fn a_spot_order_carries_no_reduce_only_flag_and_a_perp_keeps_its_own() {
    assert!(!reduce_only_on_wire(&hype(), true), "spot takes none");
    assert!(!reduce_only_on_wire(&hype(), false));
    assert!(reduce_only_on_wire(&btc(), true), "a perpetual's flag is untouched");
    assert!(!reduce_only_on_wire(&btc(), false));
}

/// A BUY pays its fee in the base token: the account gets `sz - fee`, the fee is `fee x px` dollars.
#[test]
fn a_buy_with_a_fee_in_the_base_token_is_reported_net() {
    let (qty, commission, asset) =
        net_base_fee(&hype(), 1, 10.0, 37.0, 0.0045, "HYPE").expect("netted");
    assert!((qty - 9.9955).abs() < 1e-12, "{qty}");
    assert!((commission - 0.1665).abs() < 1e-12, "{commission}");
    assert_eq!(asset, "USDC", "charged in the quote token");
    // A maker rebate in the base is the same rule with the sign of the fee: more is received.
    let (qty, commission, _) =
        net_base_fee(&hype(), 1, 10.0, 37.0, -0.0045, "HYPE").expect("netted");
    assert!((qty - 10.0045).abs() < 1e-12 && commission < 0.0, "{qty} {commission}");
    // A sale that were charged in the base gives the fee on top of the size.
    let (qty, _, _) = net_base_fee(&hype(), -1, 10.0, 37.0, 0.0045, "HYPE").expect("netted");
    assert!((qty - 10.0045).abs() < 1e-12, "{qty}");
}

#[test]
fn a_fill_that_needs_no_netting_is_left_alone() {
    // A sale pays its fee in the quote: nothing to net.
    assert_eq!(net_base_fee(&hype(), -1, 10.0, 37.0, 0.5, "USDC"), None);
    // No fee, or a fee in some third token.
    assert_eq!(net_base_fee(&hype(), 1, 10.0, 37.0, 0.0, "HYPE"), None);
    assert_eq!(net_base_fee(&hype(), 1, 10.0, 37.0, 0.01, "PURR"), None);
    // A perpetual pays in the quote, whatever the row says.
    assert_eq!(net_base_fee(&btc(), 1, 1.0, 60_000.0, 0.01, "BTC"), None);
    // A row that would net to nothing, or to a non-number, is not rewritten into one.
    assert_eq!(net_base_fee(&hype(), 1, 0.004, 37.0, 0.0045, "HYPE"), None);
    assert_eq!(net_base_fee(&hype(), 1, 10.0, 0.0, 0.0045, "HYPE"), None);
    assert_eq!(net_base_fee(&hype(), 1, 10.0, 37.0, f64::NAN, "HYPE"), None);
}

/// **The point of netting: the engine's equity is the venue's at every mark.** The venue's account went
/// `-sz x px` dollars and `+(sz - fee)` tokens; the engine's margin books only the position at its entry price and
/// the commission, so its equity change is `-commission + net x (mark - px)`.
#[test]
fn net_base_fee_keeps_the_equity_identical_at_every_mark() {
    let (sz, px, fee) = (10.0_f64, 37.0_f64, 0.0045_f64);
    let (net, commission, _) = net_base_fee(&hype(), 1, sz, px, fee, "HYPE").expect("netted");
    for mark in [30.0, 37.0, 41.5, 50.0] {
        let venue = -sz * px + (sz - fee) * mark;
        let engine = -commission + net * (mark - px);
        assert!((venue - engine).abs() < 1e-9, "mark {mark}: venue {venue} engine {engine}");
    }
    // What the engine does WITHOUT it, at the mark the fill printed at: the fee is booked as 0.0045 dollars
    // where 0.1665 were paid, and the position is 0.0045 tokens larger than the account's.
    let naive = -fee + sz * (px - px);
    assert!((naive - (-px * fee)).abs() > 0.16, "{naive}");
}

fn spot_fill_frame(side: &str, fee: &str, fee_token: &str) -> serde_json::Value {
    json!({"channel":"userFills","data":{"isSnapshot":false,"user":"0xm","fills":[{
        "coin":"@107","px":"37.0","sz":"10.0","side": side,"time":1,"startPosition":"0.0",
        "dir":"Buy","closedPnl":"0.0","hash":"0x","oid":7,"crossed":true,"fee": fee,
        "tid":42,"feeToken": fee_token
    }]}})
}

#[test]
fn the_live_fill_and_the_reconcile_report_are_netted_to_the_same_digits() {
    let inst = hype();
    // The live row.
    let mut live = map_user_fills(&spot_fill_frame("B", "0.0045", "HYPE"), "hyperliquid").fills;
    let fill = live.get_mut(0).expect("one fill");
    fill.symbol = inst.symbol.as_str().into();
    net_fill_event(fill, &inst);
    assert_eq!(fill.symbol.as_str(), "HYPE/USDC");
    assert!((fill.last_qty - 9.9955).abs() < 1e-12);
    assert!((fill.commission - 0.1665).abs() < 1e-12);
    assert_eq!(fill.commission_asset.as_str(), "USDC");

    // The reconcile read of the same row.
    let body = serde_json::to_string(&json!([{
        "coin":"@107","px":"37.0","sz":"10.0","side":"B","time":1,"oid":7,"crossed":true,
        "fee":"0.0045","tid":42,"feeToken":"HYPE"
    }]))
    .unwrap();
    let mut reports = crate::exec::recon_client::parse_fills(&body).expect("parses");
    let report = reports.get_mut(0).expect("one report");
    net_fill_report(report, &inst);
    assert_eq!(report.last_qty, fill.last_qty);
    assert_eq!(report.commission, fill.commission);
    assert_eq!(report.commission_asset, fill.commission_asset.as_str());

    // A sale pays its fee in USDC and is left exactly as the venue said it.
    let mut sale = map_user_fills(&spot_fill_frame("A", "0.1665", "USDC"), "hyperliquid").fills;
    let sold = sale.get_mut(0).expect("one fill");
    net_fill_event(sold, &inst);
    assert_eq!(
        (sold.last_qty, sold.commission, sold.commission_asset.as_str()),
        (10.0, 0.1665, "USDC")
    );
}
