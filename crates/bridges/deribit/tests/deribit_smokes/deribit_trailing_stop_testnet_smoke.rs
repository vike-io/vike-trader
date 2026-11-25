//! Deribit TRAILING STOP LIVE TESTNET drill: the `trailing` kind `VenueRestDeribit` builds (`execution/rest.rs`'s
//! `order_params_on`: `trailing_stop` with the trail as `trigger_offset`) and its capability row does not
//! declare yet. The row opens only after this has passed, so this is the evidence it waits on.
//!
//!     MSYS_NO_PATHCONV=1 LANE=<free lane> just the CI box <branch> env \
//!       VIKE_SETTINGS_DIR=/var/lib/vike/settings VIKE_NO_LIVE_KEYS=true \
//!       cargo test -p vike-deribit --test deribit_smokes -- --ignored --nocapture \
//!       deribit_trailing_stop_testnet_smoke::
//!
//! PASSED on the testnet 2026-11-08 (main `d1624c326e`, store account `#6`); its `CAPTURED` row is
//! `tests/offline/native_stops.rs`'s `CAPTURED_TRAILING_ROW`. Run it by hand, never in a loop.
//!
//! ORDER-PLACING, TESTNET ONLY: every socket is `TESTNET_WEBSOCKET` and `only_testnet` refuses anything else.
//! Double-gated like every `*_smoke.rs`: network + the default DEMO account's keys through
//! `vike_secrets::smoke_directory`; absent credentials print `SKIP` and return. Every label starts with
//! [`PREFIX`] (never `cancel_all`); a `Drop` guard cancels our labels, so a failed assertion or a panic
//! still leaves nothing resting. NOTHING TRADES: the one order is a REDUCE-ONLY sell trailing stop on
//! `BTC-PERPETUAL` half the mark price behind the market, so it cannot fire, and if it ever did with no
//! position the venue refuses its order (a reduce-only stop is accepted with no position, measured
//! 2026-10-26). A reconcile pass of the running node that lands while it rests may hold it as an
//! `UnknownOrder`: read the node's `recon ls` after the run.
//!
//! What it proves: the far trailing stop is ACCEPTED through the production client, reads back by label
//! as `untriggered` / `trailing_stop` with `trigger_offset` the trail (the row is printed as `CAPTURED`:
//! the offline rows in `tests/offline/native_stops.rs` are the documented shape and this replaces them),
//! the restart reconcile (`ReconcileClientDeribit`) lists it as a resting `Trailing` under our order client id, and the
//! client's cancel ends it with `OrderCanceled`.

use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::demo_credentials::load_demo_credentials;
use vike_deribit::test_support::{
    TESTNET_REST, TestnetConnection, only_testnet, public, testnet_urls,
};
use vike_deribit::{
    ReconcileClientDeribit, fetch_deribit_properties, spawn_deribit_execution_client,
};
use vike_execution::recon::ReconcileClient;
use vike_execution::{ExecutionClient, Ingest, event_channel};
use vike_model::events::Event;
use vike_model::{OrderRequest, OrderType, now_ms};

/// Every label of this drill starts with it, and nothing else on the account does.
const PREFIX: &str = "n0tr";
/// INVERSE: the amount is US dollars, one lot (and the minimum) is 10.
const PERP: &str = "BTC-PERPETUAL";
const LOT: f64 = 10.0;

/// The rows of a private read, or a panic naming the method.
fn array(c: &mut TestnetConnection, method: &str, params: &Value) -> Vec<Value> {
    let v = c.send(method, params).unwrap_or_else(|e| panic!("{method}: {}", e.msg));
    v.as_array().cloned().unwrap_or_default()
}

/// Every open order of the account whose label starts with [`PREFIX`].
fn open_ours(c: &mut TestnetConnection) -> Vec<Value> {
    array(c, "private/get_open_orders", &json!({}))
        .into_iter()
        .filter(|o| o["label"].as_str().is_some_and(|l| l.starts_with(PREFIX)))
        .collect()
}

/// Cancels our labels when the drill ends, however it ends.
struct Guard {
    key: String,
    secret: String,
}

impl Drop for Guard {
    fn drop(&mut self) {
        let Ok(mut c) = TestnetConnection::open(&self.key, &self.secret) else {
            println!("CLEANUP FAILED: no testnet socket; cancel `{PREFIX}*` by hand");
            return;
        };
        for o in open_ours(&mut c) {
            let label = o["label"].as_str().unwrap_or_default().to_string();
            let r = c.send("private/cancel_by_label", &json!({ "label": label }));
            println!("cleanup cancel_by_label {label}: {r:?}");
        }
    }
}

/// Drains the client's ingest lane for up to `wait` until `pred` matches; every event is printed.
fn wait_for(
    next: &mut dyn FnMut() -> Option<Ingest>,
    seen: &mut Vec<Event>,
    wait: Duration,
    pred: impl Fn(&Event) -> bool,
) -> bool {
    let deadline = Instant::now() + wait;
    while Instant::now() < deadline {
        match next() {
            Some(Ingest::Event(e)) => {
                println!("  << {e:?}");
                let hit = pred(&e);
                seen.push(e);
                if hit {
                    return true;
                }
            }
            Some(_) => {}
            None => std::thread::sleep(Duration::from_millis(100)),
        }
    }
    false
}

#[test]
#[ignore = "places a TESTNET order (a far reduce-only trailing stop that cannot fire); run by hand (see the module doc)"]
fn deribit_trailing_stop_on_the_testnet() {
    vike_log::test_init();
    only_testnet();
    let Some(creds) = load_demo_credentials() else { return };
    let (key, secret) = (creds.api_key.expose().to_string(), creds.api_secret.expose().to_string());
    let mut raw = TestnetConnection::open(&key, &secret).expect("testnet order-WS auth");
    if !open_ours(&mut raw).is_empty() {
        println!("SKIP: the testnet account holds `{PREFIX}*` orders");
        return;
    }
    let _guard = Guard { key, secret };

    let grid = fetch_deribit_properties(TESTNET_REST, PERP).expect("the instrument grid");
    let tick = grid.tick_size;
    let recon = ReconcileClientDeribit::connect(
        &testnet_urls(),
        &load_demo_credentials().expect("creds"),
        PERP,
    )
    .expect("the recon client's own authed socket");
    let (tx, mut rx) = event_channel(256);
    let mut client =
        spawn_deribit_execution_client(testnet_urls(), creds, PERP.to_string(), tx, None);
    let mut next = move || rx.try_recv().ok();
    let mut seen = Vec::new();
    std::thread::sleep(Duration::from_secs(4)); // the pump's auth + subscribe settle

    let mark =
        public("/api/v2/public/ticker", &[("instrument_name", PERP.to_string())])["mark_price"]
            .as_f64()
            .expect("the perpetual has a mark price");
    let trail = ((mark * 0.5) / tick).round() * tick;
    let order_client_id = format!("{PREFIX}{:05}t", now_ms() % 100_000);
    let req = OrderRequest {
        order_client_id: order_client_id.clone(),
        venue: "deribit".to_string(),
        symbol: PERP.to_string(),
        side: -1,
        qty: LOT,
        order_type: OrderType::Trailing,
        trail: Some(trail),
        reduce_only: true,
        timestamp: now_ms(),
        ..Default::default()
    };
    println!("== a reduce-only sell trailing stop {trail} behind the mark {mark}");
    client.submit(&req);
    assert!(
        wait_for(&mut next, &mut seen, Duration::from_secs(15), |e| matches!(
            e, Event::OrderAccepted(a) if a.order_client_id == order_client_id
        )),
        "{order_client_id} never accepted: {seen:?}"
    );

    let rows = array(
        &mut raw,
        "private/get_order_state_by_label",
        &json!({"currency": "BTC", "label": order_client_id}),
    );
    let row = rows.last().cloned().unwrap_or_else(|| panic!("no row under {order_client_id}"));
    println!("CAPTURED trailing stop row {order_client_id}: {row}");
    assert_eq!(row["order_type"], "trailing_stop", "{row}");
    assert_eq!(row["order_state"], "untriggered", "{row}");
    assert_eq!(
        row["trigger_offset"].as_f64(),
        Some(trail),
        "the trail rides trigger_offset: {row}"
    );
    assert_eq!(row["reduce_only"], true, "{row}");

    let reports = recon.fetch_order_status_reports(0).expect("the recon read");
    let report = reports
        .iter()
        .find(|r| r.order_client_id.as_deref() == Some(order_client_id.as_str()))
        .unwrap_or_else(|| {
            panic!("the restart reconcile must list the trailing stop: {reports:?}")
        });
    assert_eq!(report.order_type, OrderType::Trailing, "{report:?}");

    client.cancel(&order_client_id);
    assert!(
        wait_for(&mut next, &mut seen, Duration::from_secs(15), |e| matches!(
            e, Event::OrderCanceled(c) if c.order_client_id == order_client_id
        )),
        "{order_client_id} cancel not evented: {seen:?}"
    );
    assert!(open_ours(&mut raw).is_empty(), "nothing of ours rests");
    client.detach();
    println!(
        "END: the trailing stop rested, read back as a Trailing, and was cancelled; nothing traded"
    );
}
