//! LIVE PAPER smoke - **which iceberg display sizes IB takes on a US stock**, asked of IB itself, then of the bridge.
//! Reads five stocks' contract details and places and cancels SIX real paper orders (buy 200 AAPL LMT @150 GTC, regular
//! session; none can fill).
//!
//! The question (session vike-trader-rust-41's paper smoke, 2026-11-09): the bridge refused an AAPL iceberg with "display
//! size 1 is not a multiple of AAPL's lot size 40". Its lot is the details' `suggestedSizeIncrement`
//! (`src/transport/socket.rs`'s `lot_of`), read as the Regulation NMS round lot (Rule 600(b)(93) since 2025-11-03: 100
//! shares up to $250, 40 up to $1,000, 10 up to $10,000, 1 above), after IB ended a display of 1 with `[201] Display size
//! should be a multiple of lot size.` Neither reading is a capture; this run is.
//!
//! ⚠ **FIRST RUN 2026-11-10 05:57 UTC (session 41, paper #12, `f88602d57d`; outside regular hours, the orders unflagged):**
//! raw display 1 and 100 refused with `[201] Display size should be a multiple of lot`, display 40 ACCEPTED then
//! cancelled; through the bridge 1 and 100 refused by `lot_refusal`, 40 `OrderAccepted` then `OrderCanceled`; nothing
//! rested. `suggestedSizeIncrement` read F 100, AAPL 40, MSFT 40, BRK A 1, each its tier, and BKNG 100 (split 25:1 in
//! April 2026, under $250 since; AZO is the $1,000-to-$10,000 example now). The ibkr row lists the iceberg behind it.
//!
//! | step | what | settles |
//! |---|---|---|
//! | `LOT DETAILS` | `minSize`, `sizeIncrement`, `suggestedSizeIncrement` of one stock per price tier (`TIER_STOCKS`) | whether the field is the price-tiered round lot (100, 40, 40, 10, 1 if each stock's price sits in the tier named) |
//! | `b9ice1r`, `b9ice40r`, `b9ice100r` | displays 1, 40, 100 straight through `ibapi` (the bridge's check is not on this path) | IB's own answer for each |
//! | `b9ice1b`, `b9ice40b`, `b9ice100b` | the same through `connect_execution_client` | the bridge's answer: it refuses 1 and 100 by name (`lot_refusal`) and sends 40 |
//!
//! **Reading the raw rows** (a row IB accepted and then ended reads REFUSED with IB's words; ACCEPTED means it still rested
//! when cancelled):
//! - 1 REFUSED, 40 ACCEPTED, 100 REFUSED: the lot is AAPL's round lot 40 and the bridge's check is right. The row may flip
//!   on that plus the bridge's `b9ice40b` ACCEPTED then CANCELED.
//! - 100 ACCEPTED as well: IB takes a display that is no multiple of 40 (a mixed lot), so the check is too strict; loosen
//!   it before any flip.
//! - 40 REFUSED and 100 ACCEPTED: IB's lot is still the old 100, so `suggestedSizeIncrement` is the wrong field.
//! - 1 ACCEPTED and not ended: IB judges no lot on this route; the check would refuse what IB takes.
//!
//! **Extended hours are OFF for this run, both halves.** The raw rows set neither `outsideRth` nor `includeOvernight`. The
//! bridge half turns the tier's `venue.ibkr.<tier>.extended_hours` switch off for ITS OWN connection (`cfg.extended_hours =
//! false` on the config it loaded, as `ibkr_attributes_smoke` does; the settings row is never written): with the switch on,
//! as on the CI box's paper tier, the bridge refuses an iceberg by name before IB sees it (`overnight_refusal`, IB refused one on
//! its overnight route on 2026-11-09), so the row would record the bridge's refusal instead of IB's answer. The table's
//! header says what the tier's switch is.
//!
//! An IB precaution about the far price is IB's words in the table; the fix is a price nearer the market (still
//! unfillable), never a bypassed precaution. Each accepted order is cancelled at once, and the run ends by listing what of
//! `b9ice*` still rests (`RESTING`), which must be nothing.
//!
//! ⚠ It PLACES AND CANCELS REAL PAPER ORDERS. Never run it casually; the lane, the credential store and the fence are
//! `docs/ops/live-smoke-lane.md`'s, and no workflow runs it. In US regular hours (14:30 to 21:00 UTC while New York is on
//! standard time), on a the CI box lane (the gateway is on the box):
//!
//!     just the CI box <branch> env VIKE_SETTINGS_DIR=/var/lib/vike/settings VIKE_NO_LIVE_KEYS=true \
//!       cargo test -p vike-ibkr --features ibkr --test ibkr_smokes -- --ignored --nocapture ibkr_iceberg_lot_smoke::
//!
//! Triple-gated like the other live tests: `#[ignore]`; self-skips unless a demo ibkr account holding an `ACCOUNT`, on
//! the `socket` backend, exists and the gateway answers; and a hard assert refuses anything but a paper `DU...` account.
//! A run that prints no `ICEBERG LOT TABLE` tested nothing (it self-skipped), whatever its exit code says.
#![cfg(feature = "ibkr")]

use crate::common::ibkr_settings;
use crate::ibkr_flagged_tif_smoke::collect;
use crate::ibkr_order_kinds_w2_smoke::{cancelled, placed};

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::common::{aapl, connect, observe, stock};
use ibapi::orders::{Action, Order, TimeInForce as IbTif};
use vike_execution::{ExecutionClient, event_channel};
use vike_ibkr::config::load_ibkr_config_from;
use vike_ibkr::connect_execution_client;
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::{AccountTier, OrderRequest, OrderType};
use vike_venues::IbkrBackend;

/// Far below any AAPL price: a buy limit there rests unfilled.
const FAR_BID: f64 = 150.0;
/// The iceberg's whole size: a multiple of 1, 40 and 100.
const QTY: f64 = 200.0;
/// The displays asked: below a lot, AAPL's round lot as IB stated it, and the old round lot (no multiple of 40).
const DISPLAYS: [i32; 3] = [1, 40, 100];
/// One stock per round-lot tier, with the tier its price is believed to sit in (only the printed details say).
const TIER_STOCKS: [(&str, &str); 5] = [
    ("F", "100 (under $250)"),
    ("AAPL", "40 ($250 to $1,000)"),
    ("MSFT", "40 ($250 to $1,000)"),
    // Not BKNG: its 25:1 split of April 2026 put it under $250 (it read 100 on 2026-11-10).
    ("AZO", "10 ($1,000 to $10,000)"),
    ("BRK A", "1 (above $10,000)"),
];
/// How long IB gets to answer one raw order (it answers in milliseconds).
const ANSWER_WINDOW: Duration = Duration::from_secs(4);

/// Three client ids this gateway has not seen from us (raw, bridge, observer): from the clock, beside
/// `ibkr_flagged_time_in_force_smoke`'s pair in the same slot.
fn fresh_ids() -> [i32; 3] {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let base = crate::common::SMOKE_CLIENT_ID_BASE + (secs % 8_000) as i32 * 4;
    [base + 2, base + 3, crate::common::SMOKE_CLIENT_ID_BASE + 33_000 + (secs % 1_000) as i32]
}

fn iceberg(display: i32, order_client_id: &str, account: &str) -> Order {
    Order {
        action: Action::Buy,
        total_quantity: QTY,
        order_type: "LMT".to_string(),
        limit_price: Some(FAR_BID),
        tif: IbTif::GoodTillCanceled,
        display_size: Some(display),
        order_ref: order_client_id.to_string(),
        account: account.to_string(),
        ..Order::default()
    }
}

#[test]
#[ignore = "live paper, US regular hours: places and cancels six real paper AAPL icebergs; needs a running Gateway logged into a DU... account"]
fn ibkr_iceberg_display_sizes() {
    vike_log::test_init();

    let settings_dir = std::env::var("VIKE_SETTINGS_DIR").ok();
    let dir = vike_secrets::smoke_directory();
    let creds = dir.credentials_for("ibkr", &AccountLabel::Default, AccountTier::Demo);
    let Some(mut config) =
        load_ibkr_config_from(AccountTier::Demo, &creds, &ibkr_settings(settings_dir.as_deref()))
    else {
        eprintln!("skip: no demo ibkr account holding an `ACCOUNT` in the settings database");
        return;
    };
    if config.backend == IbkrBackend::Cpapi {
        eprintln!("skip: venue.ibkr.demo.backend is cpapi; the iceberg is the IB Gateway socket's");
        return;
    }
    // HARD SAFETY: paper only - this assert stays a hard failure, never a skip.
    assert!(
        config.account.starts_with("DU"),
        "smoke must target a paper (DU\u{2026}) account, got {}",
        config.account
    );
    let addr = format!("{}:{}", config.host, config.port);
    let [raw_id, bridge_id, observer_id] = fresh_ids();
    eprintln!(
        "gateway {addr}, paper account DU\u{2026}, client ids raw={raw_id} bridge={bridge_id}"
    );

    let mut table: Vec<(String, &'static str, String)> = Vec::new();
    match connect(&addr, raw_id) {
        Err(e) => {
            eprintln!("skip raw rows: could not connect to {addr} as client id {raw_id}: {e}")
        }
        Ok(client) => {
            for (symbol, tier) in TIER_STOCKS {
                match client.contract_details(&stock(symbol, "SMART", "USD")) {
                    Ok(rows) => {
                        for d in &rows {
                            eprintln!(
                                "LOT DETAILS {symbol:<6} conid {} min_size {:?} size_increment {:?} \
                                 suggested_size_increment {:?}  (round lot by tier: {tier})",
                                d.contract.contract_id,
                                d.min_size,
                                d.size_increment,
                                d.suggested_size_increment
                            );
                        }
                    }
                    Err(e) => eprintln!("LOT DETAILS {symbol:<6} failed: {e}"),
                }
            }
            let stream = client.order_update_stream().expect("the order-update stream");
            let first_id = client.next_valid_order_id().expect("IB's next valid order id");
            let mut sent: Vec<i32> = Vec::new();
            for (n, display) in DISPLAYS.into_iter().enumerate() {
                let (order_id, order_client_id) = (first_id + n as i32, format!("b9ice{display}r"));
                if let Err(e) = client.submit_order(
                    order_id,
                    &aapl(),
                    &iceberg(display, &order_client_id, &config.account),
                ) {
                    table.push((order_client_id, "NO ANSWER", format!("submit failed: {e}")));
                    continue;
                }
                sent.push(order_id);
                let reply = collect(&stream, order_id, ANSWER_WINDOW);
                let (verdict, mut detail) = (reply.verdict(), reply.detail());
                if verdict == "ACCEPTED" {
                    let _ = client.cancel_order(order_id, "");
                    let after = collect(&stream, order_id, Duration::from_secs(3));
                    detail = format!("{detail}; after cancel: {}", after.detail());
                }
                table.push((order_client_id, verdict, detail));
            }
            // Sweep by id whatever this client placed (an order already gone answers a harmless notice); never
            // `reqGlobalCancel`, which cancels every client's orders, the daemon's included.
            for order_id in sent {
                let _ = client.cancel_order(order_id, "");
            }
            std::thread::sleep(Duration::from_millis(500));
            client.disconnect();
        }
    }

    // The production path on the regular-session route: with the tier's extended-hours switch on (the CI box's paper tier) the
    // bridge refuses an iceberg by name (`overnight_refusal`) before IB sees it, so THIS connection's config turns it off,
    // as `ibkr_attributes_smoke` does; the settings row is never written.
    let tier_extended_hours = config.extended_hours;
    config.client_id = bridge_id;
    config.extended_hours = false;
    let (events, mut rx) = event_channel(256);
    match connect_execution_client(&config, events) {
        Err(e) => {
            eprintln!("skip bridge rows: the bridge could not connect (client id {bridge_id}): {e}")
        }
        Ok((mut client, _)) => {
            for display in DISPLAYS {
                let order_client_id = format!("b9ice{display}b");
                client.submit(&OrderRequest {
                    order_client_id: order_client_id.clone(),
                    venue: "ibkr".into(),
                    symbol: "AAPL.SMART.USD".into(),
                    side: 1,
                    qty: QTY,
                    order_type: OrderType::Limit,
                    price: Some(FAR_BID),
                    display_quantity: Some(f64::from(display)),
                    ..Default::default()
                });
                let (verdict, detail) = placed(&mut rx, &order_client_id);
                // Cancel whatever the answer was: an order IB holds must not be left resting.
                client.cancel(&order_client_id);
                let (end, why) = cancelled(&mut rx, &order_client_id);
                table.push((order_client_id, verdict, format!("{detail}; cancel: {end} {why}")));
            }
            client.detach();
        }
    }

    eprintln!(
        "ICEBERG LOT TABLE (AAPL.SMART.USD, buy {QTY} LMT @{FAR_BID} GTC, regular session, IB paper gateway {addr}; \
         `r` = straight through ibapi, `b` = through the bridge; extended hours OFF for this run, the tier's switch is \
         {})",
        if tier_extended_hours { "ON" } else { "off" }
    );
    for (order_client_id, verdict, detail) in &table {
        eprintln!("  {order_client_id:<10}  {verdict:<9}  {detail}");
    }
    match observe(&addr, observer_id, &config.account, "AAPL", "b9ice") {
        Ok((open, _)) if open.is_empty() => eprintln!("RESTING b9ice*: none"),
        Ok((open, _)) => eprintln!("RESTING b9ice*: {open:?} - CANCEL THESE BY HAND"),
        Err(e) => eprintln!("RESTING b9ice*: not read ({e}) - look in TWS"),
    }
    assert!(
        table.iter().any(|(_, verdict, _)| *verdict != "NO ANSWER"),
        "not one order was answered: nothing was measured"
    );
}
