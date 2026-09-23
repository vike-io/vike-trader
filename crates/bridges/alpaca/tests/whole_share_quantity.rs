//! **A whole-share Alpaca order must reach the wire as a whole number.**
//!
//! `crates/bridges/alpaca/src/event_mapper.rs`'s `build_order_body` renders the order quantity
//! through `format_to_step(qty, "0.000000001")` — a nine-decimal PRECISION CAP (Alpaca's
//! fractional-share limit), not a venue grid — and then trims the padding zeros. For a whole share
//! count the two are supposed to compose to a bare integer, which
//! `crates/bridges/alpaca/tests/mapper_orders.rs`'s `market_body_basic` pins at `10` shares.
//!
//! It stopped composing somewhere between nine and nine million. MEASURED in a the CI box lane on the
//! unfixed tree, `build_order_body` for a 9,000,000-share market order emitted
//! `"qty": "8999999.999999999"`, and Alpaca rejects a fractional quantity on most order types — so
//! a large equity order was refused at the venue for a quantity nobody asked for.
//!
//! The window is not a scattering of unlucky counts: **every whole share count in
//! `8_388_609..=9_007_199` came out fractional, 618_591 of 618_591, each exactly one nano-share
//! LOW**, while 8_388_608 and 9_007_200 either side of it came out clean.
//!
//! # Why the fix is not in this file
//!
//! The corruption is in the shared quantizer, not in this bridge, and it is stated at
//! `crates/vike-bridge-core/src/format.rs`'s `MAX_UNAMBIGUOUS_MULTIPLE`: above a multiple count of
//! `2^52` the decimal grid is finer than the f64 spacing, so several multiples of the step share
//! one float and the bit-exact witness returns whichever one its own roundings land on.
//! `crates/vike-bridge-core/tests/grid_witness_is_unambiguous.rs` is that defect's own test.
//!
//! Threading this bridge's real `SymbolProperties` grid into the call site — the other candidate —
//! would not have closed it. The grid is fetched at the mount for `RiskLimits` only
//! (`crates/bridges/alpaca/src/instruments.rs`'s `fetch_alpaca_properties`), is best-effort so a
//! failed fetch leaves no step to thread, and is VENUE-SUPPLIED: `parse_asset_properties` will
//! return whatever `min_trade_increment` says, including a nine-decimal one, at which point the
//! identical call reproduces the identical corruption. What this file pins is the property the
//! venue actually cares about, at the venue's own wire site.

use vike_alpaca::build_order_body;
use vike_model::{OrderRequest, TimeInForce};

fn req(qty: f64) -> OrderRequest {
    OrderRequest {
        account: None,
        client_order_id: "coid-1".into(),
        venue: "alpaca".into(),
        symbol: "AAPL".into(),
        side: 1,
        qty,
        order_type: "market".into(),
        price: None,
        trigger_price: None,
        reduce_only: false,
        time_in_force: TimeInForce::Day,
        gtd_expiry: None,
        ts: 111,
        parent_order_id: None,
        linked_order_ids: vec![],
        order_list_id: None,
        contingency_type: None,
        weight: 0.0,
        stop: None,
        trail: None,
        extreme: None,
        on_close: false,
        margin_mode: None,
        trigger_by: None,
        combo_legs: Vec::new(),
    }
}

fn qty_of(qty: f64) -> String {
    build_order_body(&req(qty))["qty"].as_str().expect("qty is a wire string").to_string()
}

/// THE DEFECT, as the one string that went out. RED before the quantizer bound landed.
#[test]
fn a_nine_million_share_order_is_not_sent_as_a_fraction() {
    assert_eq!(
        qty_of(9_000_000.0),
        "9000000",
        "before the fix this read \"8999999.999999999\" — Alpaca rejects a fractional quantity on \
         most order types, so the order never rested"
    );
}

/// The whole exposed window, end to end. `8_388_608` is `2^23`, the first magnitude whose f64
/// spacing exceeds the nine-decimal cap; `9_007_199` is the last share count below the old
/// `MAX_EXACT_INT` ceiling. Every whole share count across it — and a margin either side — must
/// reach the wire as a bare integer.
#[test]
fn every_whole_share_count_in_the_window_reaches_the_wire_whole() {
    let mut broken = 0usize;
    let mut sample: Vec<String> = Vec::new();
    for n in 8_388_600i64..=9_007_210 {
        let wire = qty_of(n as f64);
        if wire != n.to_string() {
            broken += 1;
            if sample.len() < 8 {
                sample.push(format!("{n} -> {wire}"));
            }
        }
    }
    assert_eq!(
        broken,
        0,
        "{broken} whole share counts reached the wire fractional; first: {}",
        sample.join(", ")
    );
}

/// The edges, named. The row above the band is the CONTROL: the old bound already declined there,
/// so it was correct before the change and must be unchanged by it.
#[test]
fn the_windows_edges() {
    assert_eq!(qty_of(8_388_607.0), "8388607", "below the band");
    assert_eq!(qty_of(8_388_608.0), "8388608", "the low edge, 2^23");
    assert_eq!(qty_of(9_007_199.0), "9007199", "the high edge");
    assert_eq!(qty_of(9_007_200.0), "9007200", "above the old ceiling — the control row");
}

/// The cap still CAPS: fractional-share and crypto quantities keep their digits, and nothing is
/// rounded up to a whole share to make the test above pass.
#[test]
fn fractional_quantities_keep_their_precision() {
    assert_eq!(qty_of(10.0), "10");
    assert_eq!(qty_of(0.01), "0.01");
    assert_eq!(qty_of(0.000000001), "0.000000001");
    assert_eq!(qty_of(1.5), "1.5");
    assert_eq!(qty_of(0.123456789), "0.123456789");
    // ...including inside the exposed window, so the cure is not a whole-number special case.
    assert_eq!(qty_of(9_000_000.5), "9000000.5");
    assert_eq!(qty_of(8_500_000.25), "8500000.25");
}
