//! **Does Deribit's `private/edit` move a resting STOP's trigger in place?** The testnet half of the
//! unified amend (decision record on the unified amend, `docs/decisions/`): `DeribitRest::edit_order` edits a
//! stop's `trigger_price` natively, and the offline fake (`tests/offline/native_stops.rs`) can only say what
//! we believe. This asks the venue.
//!
//! ```text
//! MSYS_NO_PATHCONV=1 LANE=<free lane> just the CI box <branch> env \
//!   VIKE_SETTINGS_DIR=/var/lib/vike/settings VIKE_NO_LIVE_KEYS=true \
//!   cargo test -p vike-deribit --test deribit_stop_amend_testnet_smoke -- --ignored --nocapture
//! ```
//!
//! ORDER-PLACING, TESTNET ONLY (`only_testnet` refuses any other host), `#[ignore]`d, self-skipping without the
//! default demo account's keys. It rests ONE far, non-reduce-only SELL stop_market on `BTC-PERPETUAL` (the
//! trigger 20 percent under the last price, so nothing can fire and no position is needed), edits it twice, and
//! cancels it: the account is flat and nothing rests when it ends, on every exit path (a `Drop` guard cancels
//! the label). A run killed from outside skips destructors: cancel `n0amd*` on the testnet by hand.
//!
//! What it proves, in order:
//!   1. the stop rests `untriggered` / `stop_market` at the trigger placed;
//!   2. `modify_order` with a new trigger and a new amount answers `OrderModified`, and the VENUE's own row,
//!      read back by label, holds the new trigger (floored to the tick), the new amount, the SAME order id and
//!      the same label: an edit in place, not a cancel and a new order;
//!   3. a quantity-only edit keeps the trigger (the venue wants the trigger on every edit of a trigger order,
//!      and the client re-sends the resting one);
//!   4. a take-profit is refused by the client in words before a frame is written;
//!   5. the cancel ends it and nothing rests under the label.
//!
//! The last line, `stop-amend smoke green`, is the proof; a `SKIP:` line naming `deribit` is not.

use std::time::{Duration, Instant};

use serde_json::{Value, json};
use vike_bridge_core::rest::VenueRest;
use vike_bridge_core::transport::{RestTransport, UreqTransport};
use vike_deribit::client::DeribitRest;
use vike_deribit::fetch_deribit_properties;
use vike_deribit::rpc::parse_response;
use vike_deribit::transport::{DeribitOrderTransport, TESTNET_REST, TESTNET_WS};
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::events::Event;
use vike_model::{AccountTier, OrderRequest, OrderType, now_ms};
use vike_secrets::credentials::{Credentials, api_credentials};

const INST: &str = "BTC-PERPETUAL";
const TICK: f64 = 0.5;
/// `BTC-PERPETUAL`'s lot, and its minimum amount, in US dollars.
const LOT: f64 = 10.0;

/// The crate's smoke double-gate: the store's default DEMO account; `None` = self-skip, said out loud.
fn load_demo_creds() -> Option<Credentials> {
    let directory = vike_secrets::smoke_directory();
    let account = directory.credentials_for("deribit", &AccountLabel::Default, AccountTier::Demo);
    let creds = api_credentials(&account, "deribit");
    if creds.is_none() {
        println!("SKIP: DERIBIT_DEMO creds absent — nothing was measured");
        tracing::warn!(target: "vike_deribit", "SKIP: DERIBIT_DEMO creds absent");
    }
    creds
}

/// Refuse any host but the testnet's: this drill places orders.
fn only_testnet() {
    assert!(TESTNET_WS.contains("test.deribit.com"), "not the testnet: {TESTNET_WS}");
    assert!(TESTNET_REST.contains("test.deribit.com"), "not the testnet: {TESTNET_REST}");
}

fn snap(x: f64) -> f64 {
    (x / TICK).round() * TICK
}

fn last_price() -> f64 {
    only_testnet();
    let r = UreqTransport::new("deribit")
        .public(TESTNET_REST, "/api/v2/public/ticker", &[("instrument_name", INST.to_string())])
        .expect("testnet ticker");
    r["result"]["last_price"].as_f64().expect("a last price")
}

/// The venue's own row for `label`: `private/get_order_state_by_label` (one row, or none).
fn venue_row(rest: &DeribitRest, label: &str) -> Option<Value> {
    let resp = rest
        .transport
        .lock()
        .unwrap()
        .call("private/get_order_state_by_label", &json!({"currency": "BTC", "label": label}))
        .expect("order state by label");
    let (_id, result, error) = parse_response(&resp);
    assert!(error.is_none_or(|e| e.is_null()), "the venue answered an error: {resp}");
    result.and_then(|r| r.as_array().and_then(|rows| rows.first().cloned()))
}

/// Cancels `coid` through the PLACING client when dropped, so no failure path leaves it resting.
struct CancelOnDrop<'a> {
    rest: &'a DeribitRest,
    coid: &'a str,
}

impl Drop for CancelOnDrop<'_> {
    fn drop(&mut self) {
        if let Err(e) = self.rest.cancel_order(self.coid) {
            // an order already cancelled by the test's own end answers "gone": the quiet case
            tracing::info!(target: "vike_deribit", coid = self.coid, error = %e.msg, "cleanup: cancel answered");
        }
    }
}

fn stop(coid: &str, trigger: f64, qty: f64) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.to_string(),
        venue: "deribit".to_string(),
        symbol: INST.to_string(),
        side: -1,
        qty,
        order_type: OrderType::Stop,
        trigger_price: Some(trigger),
        reduce_only: false,
        ts: now_ms(),
        ..Default::default()
    }
}

#[test]
#[ignore = "network + testnet creds — rests, edits and cancels a real far testnet stop — run manually (see module doc)"]
fn a_stops_trigger_moves_in_place_on_the_deribit_testnet() {
    vike_log::test_init();
    let Some(creds) = load_demo_creds() else { return };
    only_testnet();
    let Some(grid) = fetch_deribit_properties(&creds, INST) else {
        println!("SKIP: deribit testnet lists no readable row for {INST} — nothing was measured");
        return;
    };
    let mut transport = DeribitOrderTransport::new(
        TESTNET_WS,
        creds.api_key.expose(),
        creds.api_secret.expose(),
        None,
    );
    transport.connect().expect("order-WS auth");
    let rest = DeribitRest::new(transport, INST, grid, "BTC");

    let last = last_price();
    let (t0, t1) = (snap(last * 0.80), snap(last * 0.75));
    let coid = format!("n0amd{}", now_ms() % 100_000_000);
    println!("last={last}; placing a far SELL stop {coid} at trigger {t0}, to edit to {t1}");

    // 1. it rests
    let events = VenueRest::submit_order(&rest, &stop(&coid, t0, LOT));
    let _cleanup = CancelOnDrop { rest: &rest, coid: &coid };
    assert!(matches!(events.last(), Some(Event::OrderAccepted(_))), "not accepted: {events:?}");
    let before = venue_row(&rest, &coid).expect("the stop rests under its label");
    println!("before: {before}");
    assert_eq!(before["order_state"], "untriggered");
    assert_eq!(before["order_type"], "stop_market");
    assert_eq!(before["trigger_price"].as_f64(), Some(t0));
    let order_id = before["order_id"].clone();

    // 2. a new trigger and a new amount: an edit in place
    let resting = stop(&coid, t0, LOT);
    let events = rest.modify_order(&resting, Some(2.0 * LOT), Some(t1));
    println!("modify events: {events:?}");
    assert!(matches!(events.as_slice(), [Event::OrderModified(_)]), "not modified: {events:?}");
    let after = venue_row(&rest, &coid).expect("the stop still rests under its label");
    println!("after:  {after}");
    assert_eq!(after["trigger_price"].as_f64(), Some(t1), "the venue holds the new trigger");
    assert_eq!(after["amount"].as_f64(), Some(2.0 * LOT), "and the new amount");
    assert_eq!(after["order_id"], order_id, "the SAME venue order: an edit, not a replacement");
    assert_eq!(after["order_state"], "untriggered");

    // 3. a quantity-only edit keeps the trigger
    let moved = stop(&coid, t1, 2.0 * LOT);
    let events = rest.modify_order(&moved, Some(LOT), None);
    assert!(matches!(events.as_slice(), [Event::OrderModified(_)]), "not modified: {events:?}");
    let kept = venue_row(&rest, &coid).expect("the stop still rests");
    assert_eq!(kept["trigger_price"].as_f64(), Some(t1), "the trigger did not move");
    assert_eq!(kept["amount"].as_f64(), Some(LOT));

    // 4. a take-profit is refused in words, nothing written
    let mut take = stop(&coid, t1, LOT);
    take.order_type = OrderType::TakeProfit;
    let events = rest.modify_order(&take, Some(LOT), None);
    assert!(matches!(events.as_slice(), [Event::OrderModifyRejected(_)]), "{events:?}");

    // 5. the cancel ends it and nothing rests under the label
    rest.cancel_order(&coid).expect("cancel the stop");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let gone = venue_row(&rest, &coid).is_none_or(|r| r["order_state"] != "untriggered");
        if gone {
            break;
        }
        assert!(Instant::now() < deadline, "{coid} still rests after the cancel");
        std::thread::sleep(Duration::from_millis(500));
    }
    println!("stop-amend smoke green");
}
