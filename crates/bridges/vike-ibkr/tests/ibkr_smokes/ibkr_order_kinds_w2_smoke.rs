//! LIVE PAPER smoke - the wave-2 order kinds through the bridge, on the IB Gateway socket: one row per kind, each a
//! real paper order of AAPL that cannot fill (far prices), placed, amended where the kind has an amend, and cancelled.
//!
//! ⚠ **FIRST RUN 2026-11-09 (paper #12, extended hours ON):** MIT, LIT and STP LMT placed, trigger-amended and
//! cancelled, MTL traded and flattened; the iceberg was accepted and then ended by IB with `[201] Iceberg orders not
//! supported for this combination of exchange and security type` (the overnight route the switch sends a stock limit
//! on). Since then the bridge refuses an iceberg by name while the switch applies, and THIS run turns the switch off for
//! its own connection (the header says what the tier's switch is). The retest (switch off) was ended by IB with `[201]
//! Display size should be a multiple of lot size.`, so the bridge now refuses a display off the contract's lot by name
//! (`src/transport/socket.rs`'s `lot_refusal`): the iceberg row (display 1) reads REFUSED, naming the lot it read; which
//! displays IB itself takes is `ibkr_iceberg_lot_smoke`'s question (displays 1, 40 and 100, with and without the bridge). What it
//! checks offline (`src/execution/order.rs`'s `a_take_profit_is_mit_without_a_price_and_lit_with_one`,
//! `the_auction_lifetimes_are_opg_and_moc_or_loc`, `a_display_size_rides_a_plain_limit_order_as_a_whole_number`,
//! `src/transport/socket.rs`'s `the_wave_two_kinds_are_built_as_mapped`, `tests/ibkr_amend.rs`'s
//! `a_take_profit_amend_moves_the_limit_of_a_lit_and_only_the_size_of_a_mit` and
//! `a_trigger_amend_re_places_the_order_with_its_new_aux_price`).
//!
//! What only a gateway can answer, one line each in the printed `ORDER KINDS W2` table:
//!
//! 1. **TWS takes each spelling**: `MIT` (trigger as `auxPrice`), `LIT` (limit `lmtPrice`, trigger `auxPrice`), an
//!    iceberg `LMT` with `displaySize`, a limit-on-open (`LMT`, TIF `OPG`) and a limit-on-close (`LOC`, DAY):
//!    `OrderAccepted`, or an `OrderRejected` carrying IB's words (an unlisted rejection code is the core watchdog's, and
//!    this smoke reports it as NO ANSWER).
//! 2. **The TRIGGER amends** (`modify_trigger`, re-placed under the same id): a `MIT`'s trigger moves; a `LIT`'s limit
//!    and trigger move together; a `STP LMT`'s trigger moves with its limit kept. `OrderModified` carrying the venue's
//!    trigger (matched on the echo's `auxPrice`), or an `OrderModifyRejected` saying why.
//! 3. **The cancel ends each**: `OrderCanceled`.
//! 4. **A market-to-limit** (`MTL`) is a MARKET order and fills: it buys 1 AAPL and, filled, sells 1 back with another
//!    `MTL` so the paper account ends flat; unfilled (the market is shut), it is cancelled. Its row says which.
//!
//! The market-on-open and market-on-close orders are NOT placed by that test. The AUCTION test
//! (`ibkr_order_kinds_w2_auctions_place_cancel`, its own `ORDER KINDS W2 AUCTIONS` table, order client ids `b4w2a…`) places all four
//! auction orders of 1 AAPL and cancels each at once: a market-on-open (`MKT`, TIF `OPG`), a limit-on-open (`LMT` at the
//! far bid, `OPG`), a market-on-close (`MOC`, DAY) and a limit-on-close (`LOC` at the far bid, DAY), and beside them a
//! good-till-date limit at the far bid (`GTD`, `goodTillDate` a day ahead), reads each back off IB's open-order row on an
//! observer client three ids above (`READ BACK` when IB holds the order type and TIF that were sent) and sweeps whatever of
//! its own still rests. It KEEPS the tier's extended-hours switch: no auction order is ever flagged, and the GTD row then
//! measures the route a GTD stock limit takes on that tier (`outsideRth` alone with the switch on; the attributes smoke's
//! GTD row measures the regular route). The two MARKET rows are placed only OUTSIDE 13:00-21:00 UTC (at least
//! half an hour before the US open in either daylight regime, and not inside the session), so the cancel behind each
//! cannot come after its auction; in that window they print `SKIPPED`. Its orders carry no data-delay label, so the
//! bridge's delayed-data rule (`src/execution/delayed_order.rs`) does not judge them; through the node a market auction order on a
//! delayed stream is refused there as a MARKET order (`src/execution/delayed_order_tests.rs`). Run it alone with
//! `-- --ignored --nocapture ibkr_order_kinds_w2_smoke::ibkr_order_kinds_w2_auctions`.
//!
//! It dials the exec client under a client id of its own (from the clock, never one the daemon dials) and the order client ids are
//! `b4w2…`. IB may answer a precaution about the far prices; the table then carries IB's words, and the fix is prices
//! nearer the market (still unfillable), never a bypassed precaution. The only hard failures are a target that is not a
//! paper `DU...` account and a run in which no submit was answered at all. A run that prints no `ORDER KINDS W2` table
//! tested nothing (it self-skipped), whatever its exit code says.
//!
//! ⚠ It PLACES, AMENDS AND CANCELS REAL PAPER ORDERS, and its market-to-limit row TRADES one AAPL each way. Never run it
//! casually; the lane, the credential store and the fence are `docs/ops/live-smoke-lane.md`'s, and no workflow runs it.
//! On a the CI box lane (the gateway is on the box), on 2026-11-09 or later:
//!
//!     just the CI box <branch> env VIKE_SETTINGS_DIR=/var/lib/vike/settings VIKE_NO_LIVE_KEYS=true \
//!       cargo test -p vike-ibkr --features ibkr --test ibkr_smokes -- --ignored --nocapture ibkr_order_kinds_w2_smoke::
//!
//! Triple-gated like the other live tests: `#[ignore]`; self-skips unless a demo ibkr account holding an `ACCOUNT`, on
//! the `socket` backend, exists and the gateway answers; and a hard assert refuses anything but a paper `DU...` account.
#![cfg(feature = "ibkr")]

use crate::common::{connect, ibkr_settings};

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ibapi::client::blocking::Client;
use ibapi::orders::{OrderData, Orders};
use ibapi::subscriptions::SubscriptionItem;
use tokio::sync::mpsc::Receiver;
use tokio::sync::mpsc::error::TryRecvError;
use vike_execution::{ExecutionClient, Ingest, event_channel};
use vike_ibkr::config::load_ibkr_config_from;
use vike_ibkr::connect_execution_client;
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::events::Event;
use vike_model::{AccountTier, OrderRequest, OrderType, TimeInForce};
use vike_venues::IbkrBackend;

/// Far above any AAPL price: a sell if-touched there rests untriggered.
const TRIGGER: f64 = 5_000.0;
/// Far below any AAPL price: a buy limit there rests unfilled.
const FAR_BID: f64 = 150.0;

/// A client id this gateway has not seen from us: from the clock (the daemon's ids are small and configured).
fn fresh_id() -> i32 {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    crate::common::SMOKE_CLIENT_ID_BASE + 8_000 + (secs % 8_000) as i32
}

pub(crate) type Verdict = (&'static str, String);

/// The first event about `order_client_id` that `pick` answers, within `window`: `(verdict, detail)`, or NO ANSWER.
fn wait_for(
    rx: &mut Receiver<Ingest>,
    window: Duration,
    pick: impl Fn(&Event) -> Option<Verdict>,
) -> Verdict {
    let deadline = Instant::now() + window;
    while Instant::now() < deadline {
        match rx.try_recv() {
            Ok(Ingest::Event(e)) => {
                if let Some(answer) = pick(&e) {
                    return answer;
                }
            }
            Ok(_) => {}
            Err(TryRecvError::Empty) => std::thread::sleep(Duration::from_millis(50)),
            Err(TryRecvError::Disconnected) => break,
        }
    }
    ("NO ANSWER", format!("nothing within {} s", window.as_secs()))
}

pub(crate) fn placed(rx: &mut Receiver<Ingest>, order_client_id: &str) -> Verdict {
    wait_for(rx, Duration::from_secs(20), |e| match e {
        Event::OrderAccepted(a) if a.order_client_id == order_client_id => {
            Some(("ACCEPTED", "OrderAccepted".into()))
        }
        Event::OrderFilled(f) if f.order_client_id == order_client_id => {
            Some(("FILLED", "OrderFilled".into()))
        }
        Event::OrderRejected(r) if r.order_client_id == order_client_id => {
            Some(("REFUSED", format!("OrderRejected: {}", r.reason)))
        }
        _ => None,
    })
}

fn amended(rx: &mut Receiver<Ingest>, order_client_id: &str) -> Verdict {
    wait_for(rx, Duration::from_secs(15), |e| match e {
        Event::OrderModified(m) if m.order_client_id == order_client_id => Some((
            "MODIFIED",
            format!(
                "OrderModified: qty {:?}, price {:?}, trigger {:?}",
                m.new_qty, m.new_price, m.new_trigger_price
            ),
        )),
        Event::OrderModifyRejected(r) if r.order_client_id == order_client_id => {
            Some(("REFUSED", format!("OrderModifyRejected: {}", r.reason)))
        }
        _ => None,
    })
}

pub(crate) fn cancelled(rx: &mut Receiver<Ingest>, order_client_id: &str) -> Verdict {
    wait_for(rx, Duration::from_secs(10), |e| match e {
        // An end IB made itself carries IB's words (a refusal right behind the accept: the bridge's fresh-end hold).
        Event::OrderCanceled(c) if c.order_client_id == order_client_id && !c.reason.is_empty() => {
            Some(("ENDED BY IB", format!("OrderCanceled: {}", c.reason)))
        }
        Event::OrderCanceled(c) if c.order_client_id == order_client_id => {
            Some(("CANCELED", "OrderCanceled".into()))
        }
        Event::OrderCancelRejected(r) if r.order_client_id == order_client_id => {
            Some(("REFUSED", format!("OrderCancelRejected: {}", r.reason)))
        }
        _ => None,
    })
}

#[test]
#[ignore = "live paper, Monday 2026-11-09 or later: places, trigger-amends and cancels real paper orders (MIT, LIT, STP LMT, iceberg, LOO, LOC) and trades 1 AAPL each way with MTL; needs a running Gateway logged into a DU... account"]
fn ibkr_order_kinds_w2_place_amend_cancel() {
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
        eprintln!(
            "skip: venue.ibkr.demo.backend is cpapi; these kinds go through the IB Gateway socket only"
        );
        return;
    }
    // HARD SAFETY: paper only - this assert stays a hard failure, never a skip.
    assert!(
        config.account.starts_with("DU"),
        "smoke must target a paper (DU\u{2026}) account, got {}",
        config.account
    );
    config.client_id = fresh_id();
    // The iceberg is measured on the regular-session route: with the tier's extended-hours switch on the bridge refuses
    // it on a GTC/DAY stock limit (IB refused it on its overnight venue, paper 2026-11-09).
    let tier_extended_hours = config.extended_hours;
    config.extended_hours = false;
    let (events, mut rx) = event_channel(256);
    let mut client = match connect_execution_client(&config, events) {
        Ok((c, _)) => c,
        Err(e) => {
            eprintln!("skip: the bridge could not connect (client id {}): {e}", config.client_id);
            return;
        }
    };

    let base = OrderRequest {
        venue: "ibkr".into(),
        symbol: "AAPL.SMART.USD".into(),
        side: 1,
        qty: 1.0,
        ..Default::default()
    };
    // (row, order, the trigger amend `(new_price, new_trigger)` it takes when accepted)
    let rows = [
        (
            "MIT sell, trigger",
            OrderRequest {
                order_client_id: "b4w2mit".into(),
                side: -1,
                order_type: OrderType::TakeProfit,
                trigger_price: Some(TRIGGER),
                ..base.clone()
            },
            Some((None, TRIGGER + 5.0)),
        ),
        (
            "LIT sell, both",
            OrderRequest {
                order_client_id: "b4w2lit".into(),
                side: -1,
                order_type: OrderType::TakeProfit,
                price: Some(TRIGGER - 1.0),
                trigger_price: Some(TRIGGER),
                ..base.clone()
            },
            Some((Some(TRIGGER + 4.0), TRIGGER + 5.0)),
        ),
        (
            "STP LMT buy, trigger",
            OrderRequest {
                order_client_id: "b4w2stl".into(),
                order_type: OrderType::StopLimit,
                price: Some(TRIGGER + 1.0),
                trigger_price: Some(TRIGGER),
                ..base.clone()
            },
            Some((None, TRIGGER + 5.0)),
        ),
        (
            "iceberg LMT 2/1",
            OrderRequest {
                order_client_id: "b4w2ice".into(),
                qty: 2.0,
                order_type: OrderType::Limit,
                price: Some(FAR_BID),
                display_quantity: Some(1.0),
                ..base.clone()
            },
            None,
        ),
        (
            "LOO (LMT OPG)",
            OrderRequest {
                order_client_id: "b4w2loo".into(),
                order_type: OrderType::Limit,
                price: Some(FAR_BID),
                time_in_force: TimeInForce::AtOpen,
                ..base.clone()
            },
            None,
        ),
        (
            "LOC",
            OrderRequest {
                order_client_id: "b4w2loc".into(),
                order_type: OrderType::Limit,
                price: Some(FAR_BID),
                time_in_force: TimeInForce::AtClose,
                ..base.clone()
            },
            None,
        ),
    ];

    let mut table: Vec<(String, Verdict)> = Vec::new();
    for (row, order, amend) in &rows {
        let order_client_id = order.order_client_id.as_str();
        client.submit(order);
        let verdict = placed(&mut rx, order_client_id);
        let accepted = verdict.0 == "ACCEPTED";
        table.push((format!("{row}: submit"), verdict));
        if let (true, Some((price, trigger))) = (accepted, amend) {
            let verdict = match client.modify_trigger(order, None, *price, *trigger) {
                Ok(()) => amended(&mut rx, order_client_id),
                Err(why) => ("REFUSED", format!("modify_trigger: {why}")),
            };
            table.push((format!("{row}: amend trigger {trigger}"), verdict));
        }
        // Cancel whatever the answer was: an order IB holds must not be left resting.
        client.cancel(order_client_id);
        table.push((format!("{row}: cancel"), cancelled(&mut rx, order_client_id)));
    }

    // The market-to-limit buys 1 and, filled, sells it back, so the paper account ends where it began.
    let mtl = OrderRequest {
        order_client_id: "b4w2mtl".into(),
        order_type: OrderType::MarketToLimit,
        ..base.clone()
    };
    client.submit(&mtl);
    // Its acceptance comes before its fill, so it waits for the fill (or a refusal), not for the first answer.
    let verdict = wait_for(&mut rx, Duration::from_secs(20), |e| match e {
        Event::OrderFilled(f) if f.order_client_id == "b4w2mtl" => {
            Some(("FILLED", "OrderFilled".into()))
        }
        Event::OrderRejected(r) if r.order_client_id == "b4w2mtl" => {
            Some(("REFUSED", format!("OrderRejected: {}", r.reason)))
        }
        _ => None,
    });
    let filled = verdict.0 == "FILLED";
    table.push(("MTL buy 1: submit".into(), verdict));
    if filled {
        let back =
            OrderRequest { order_client_id: "b4w2mtlx".into(), side: -1, reduce_only: true, ..mtl };
        client.submit(&back);
        let verdict = wait_for(&mut rx, Duration::from_secs(20), |e| match e {
            Event::OrderFilled(f) if f.order_client_id == "b4w2mtlx" => {
                Some(("FLAT", "OrderFilled".into()))
            }
            Event::OrderRejected(r) if r.order_client_id == "b4w2mtlx" => {
                Some(("NOT FLAT", format!("OrderRejected: {}; sell 1 AAPL back by hand", r.reason)))
            }
            _ => None,
        });
        table.push(("MTL sell 1 back".into(), verdict));
    } else {
        client.cancel("b4w2mtl");
        table.push(("MTL buy 1: cancel".into(), cancelled(&mut rx, "b4w2mtl")));
    }
    client.detach();

    eprintln!(
        "ORDER KINDS W2 (AAPL.SMART.USD, paper; trigger {TRIGGER}, far bid {FAR_BID}; extended hours OFF for \
         this run, the tier's switch is {})",
        if tier_extended_hours { "ON" } else { "off" }
    );
    for (step, (verdict, detail)) in &table {
        eprintln!("  {step:<26}  {verdict:<11}  {detail}");
    }
    assert!(
        table
            .iter()
            .any(|(step, (verdict, _))| step.ends_with("submit") && *verdict != "NO ANSWER"),
        "no submit was answered: nothing was measured"
    );
}

/// The order client id prefix of the auction test's orders.
const AUCTION_PREFIX: &str = "b4w2a";

/// Whether a MARKET auction order may be placed now: outside 13:00-21:00 UTC (module doc).
fn market_auction_window() -> bool {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    !(13 * 60..21 * 60).contains(&((secs % 86_400) / 60))
}

/// Every open order row of the auction test the observer is told of (`reqAllOpenOrders`, every client of the login).
fn auction_rows(observer: &Client) -> Result<Vec<OrderData>, String> {
    let sub = observer.all_open_orders().map_err(|e| format!("open orders: {e}"))?;
    let end = Instant::now() + Duration::from_secs(10);
    let mut rows = Vec::new();
    while Instant::now() < end {
        match sub.next_timeout(Duration::from_secs(3)) {
            Some(Ok(SubscriptionItem::Data(Orders::OrderData(o)))) => {
                if o.order.order_ref.starts_with(AUCTION_PREFIX) {
                    rows.push(o);
                }
            }
            Some(Ok(_)) => {}
            Some(Err(e)) => return Err(format!("open orders: {e}")),
            None => break,
        }
    }
    Ok(rows)
}

#[test]
#[ignore = "live paper: places and at once cancels a market-on-open, a limit-on-open, a market-on-close, a limit-on-close (the market ones only outside 13:00-21:00 UTC) and a far GTD limit of 1 AAPL, reading each back off IB's open-order row; needs a running Gateway logged into a DU... account"]
fn ibkr_order_kinds_w2_auctions_place_cancel() {
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
        eprintln!(
            "skip: venue.ibkr.demo.backend is cpapi; the auction orders go through the IB Gateway socket only"
        );
        return;
    }
    // HARD SAFETY: paper only - this assert stays a hard failure, never a skip.
    assert!(
        config.account.starts_with("DU"),
        "smoke must target a paper (DU\u{2026}) account, got {}",
        config.account
    );
    config.client_id = fresh_id();
    // The TIER's extended-hours switch is kept: it never flags an auction order (`MKT`, `OPG`, `MOC`, `LOC`), and the
    // GTD row then measures the route a GTD stock limit takes on this tier (`outsideRth` with the switch on).
    let tier_extended_hours = config.extended_hours;
    let (events, mut rx) = event_channel(256);
    let mut client = match connect_execution_client(&config, events) {
        Ok((c, _)) => c,
        Err(e) => {
            eprintln!("skip: the bridge could not connect (client id {}): {e}", config.client_id);
            return;
        }
    };
    std::thread::sleep(Duration::from_secs(2)); // the gateway's connect gap
    let observer = match connect(&format!("{}:{}", config.host, config.port), config.client_id + 3)
    {
        Ok(o) => o,
        Err(e) => {
            client.detach();
            eprintln!("skip: the observer could not connect: {e}");
            return;
        }
    };

    let base = OrderRequest {
        venue: "ibkr".into(),
        symbol: "AAPL.SMART.USD".into(),
        side: 1,
        qty: 1.0,
        ..Default::default()
    };
    let auction = |order_client_id: &str, price: Option<f64>, tif: TimeInForce| OrderRequest {
        order_client_id: order_client_id.into(),
        order_type: if price.is_some() { OrderType::Limit } else { OrderType::Market },
        price,
        time_in_force: tif,
        ..base.clone()
    };
    // (row, order, IB's order type and TIF it must hold)
    let rows = [
        ("MOO (MKT OPG)", auction("b4w2amoo", None, TimeInForce::AtOpen), ("MKT", "OPG")),
        ("LOO (LMT OPG)", auction("b4w2aloo", Some(FAR_BID), TimeInForce::AtOpen), ("LMT", "OPG")),
        ("MOC (DAY)", auction("b4w2amoc", None, TimeInForce::AtClose), ("MOC", "DAY")),
        ("LOC (DAY)", auction("b4w2aloc", Some(FAR_BID), TimeInForce::AtClose), ("LOC", "DAY")),
        (
            "GTD LMT +1d, tier's route",
            OrderRequest {
                gtd_expiry: Some(vike_model::now_ms() + 86_400_000),
                ..auction("b4w2agtd", Some(FAR_BID), TimeInForce::Gtd)
            },
            ("LMT", "GTD"),
        ),
    ];
    let market_ok = market_auction_window();

    let mut table: Vec<(String, Verdict)> = Vec::new();
    for (row, order, (want_type, want_time_in_force)) in &rows {
        let order_client_id = order.order_client_id.as_str();
        if order.order_type == OrderType::Market && !market_ok {
            table.push((
                format!("{row}: submit"),
                ("SKIPPED", "inside 13:00-21:00 UTC: a cancel could come after its auction".into()),
            ));
            continue;
        }
        client.submit(order);
        let verdict = placed(&mut rx, order_client_id);
        let accepted = verdict.0 == "ACCEPTED";
        table.push((format!("{row}: submit"), verdict));
        if accepted {
            std::thread::sleep(Duration::from_secs(1)); // the gateway's open-order table catches up
            let held = match auction_rows(&observer) {
                Err(e) => ("NO READ", e),
                Ok(held) => match held.iter().find(|r| r.order.order_ref == order_client_id) {
                    None => ("NOT LISTED", format!("no open order under {order_client_id}")),
                    Some(r) => {
                        let o = &r.order;
                        let terms = format!(
                            "id {} {} tif={} lmt={:?} gtd={:?} outsideRth={} overnight={}",
                            r.order_id,
                            o.order_type,
                            o.tif.as_str(),
                            o.limit_price,
                            o.good_till_date,
                            o.outside_rth,
                            o.include_overnight
                        );
                        if o.order_type == *want_type && o.tif.as_str() == *want_time_in_force {
                            ("READ BACK", terms)
                        } else {
                            ("NOT AS ASKED", terms)
                        }
                    }
                },
            };
            table.push((format!("{row}: IB's row"), held));
        }
        // Cancel whatever the answer was: an order IB holds must not be left resting.
        client.cancel(order_client_id);
        table.push((format!("{row}: cancel"), cancelled(&mut rx, order_client_id)));
    }

    // Nothing of this run may stay resting: cancel again whatever IB still lists, and name what survives that.
    std::thread::sleep(Duration::from_secs(2));
    if let Ok(left) = auction_rows(&observer) {
        for row in &left {
            client.cancel(&row.order.order_ref);
            table.push((
                format!("sweep {}", row.order.order_ref),
                cancelled(&mut rx, &row.order.order_ref),
            ));
        }
        std::thread::sleep(Duration::from_secs(2));
        for row in auction_rows(&observer).unwrap_or_default() {
            table.push((
                format!("LEFT RESTING {}", row.order.order_ref),
                ("CANCEL BY HAND", format!("IB order id {}", row.order_id)),
            ));
        }
    }
    client.detach();
    observer.disconnect();

    eprintln!(
        "ORDER KINDS W2 AUCTIONS (AAPL.SMART.USD, paper; far bid {FAR_BID}; the tier's extended-hours switch, kept, \
         is {})",
        if tier_extended_hours { "ON" } else { "off" }
    );
    for (step, (verdict, detail)) in &table {
        eprintln!("  {step:<26}  {verdict:<14}  {detail}");
    }
    assert!(
        table.iter().any(|(step, (verdict, _))| step.ends_with("submit")
            && !matches!(*verdict, "NO ANSWER" | "SKIPPED")),
        "no submit was answered: nothing was measured"
    );
}
