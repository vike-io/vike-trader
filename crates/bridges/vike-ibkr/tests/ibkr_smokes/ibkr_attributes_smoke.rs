//! LIVE PAPER smoke - the attributes wave through the bridge, on the IB Gateway socket (decision 0267): one row per
//! item, each a real paper order of AAPL that cannot trade (far prices, far trails, levels no price reaches), placed,
//! read back from IB's own open-order row, and cancelled.
//!
//! ⚠ **FIRST RUN 2026-11-09 (paper #12, extended hours ON):** hidden, TRAIL MIT, the percent trails and the four
//! adjustable stops READ BACK; all-or-none, sweep-to-fill, discretion and goodAfterTime were accepted, then ended by IB
//! with `[201] ... not supported for this combination of exchange and security type` (the overnight route the switch
//! sends a stock limit on), and `TRAIL LIT` was refused `[321] Please enter a stop price`. Since then the bridge refuses
//! `TRAIL LIT` and, with the switch on, those four terms by name, and THIS run turns the switch off for its own
//! connection (the header says what the tier's switch is), so the four rows measure the regular-session route. Behind
//! the first run the `ibkr` row lists `hidden`, `adjust`, `trailing_take_profit` and the percent `trailing` and
//! `trailing_take_profit`. The SECOND RUN 2026-11-09 (the switch off for its connection) accepted the four terms,
//! read each back and cancelled it: the row lists `all_or_none`, `sweep_to_fill`, `discretionary` and `release`
//! behind it. It is run
//! by session `vike-trader-rust-41` (the owner's split of 2026-11-09). What it checks offline (`src/execution/order.rs`'s
//! `the_attributes_ride_a_plain_limit_and_two_of_them_a_stock_only`, `a_display_of_zero_is_a_hidden_limit_with_no_display_size`,
//! `a_trailing_take_profit_is_trail_mit_and_a_percent_trail_is_trailing_percent`,
//! `an_adjustable_stop_carries_its_level_and_its_target`, `a_time_release_is_good_after_time_and_a_price_condition_is_never_sent`,
//! `the_auction_lifetimes_are_opg_and_moc_or_loc`, `src/transport/socket.rs`'s `the_attributes_wave_is_built_as_mapped`,
//! `tests/ibkr_socket_reconcile_parse.rs`'s `the_trailing_if_touched_codes_and_a_percent_trail_read_back_as_their_kinds`).
//!
//! What only a gateway can answer, one line each in the printed `ATTRIBUTES` table (each is a cell of the plan's
//! "Not verified" list, and each ibkr row flip waits on its line):
//!
//! 1. **TWS takes each form**: `hidden` on a `SMART`-routed `LMT` (ibapi: "only applies to orders routed to the NASDAQ
//!    exchange"), `allOrNone`, `sweepToFill`, `discretionaryAmt`, `TRAIL MIT`, `TRAIL LIT` (TWS's names, never sent by
//!    this bridge before), `trailingPercent` on `TRAIL` and `TRAIL MIT`, a STANDALONE adjustable stop (IB's samples
//!    attach one to a parent) to each of its targets, `goodAfterTime` in UTC (`yyyyMMdd-HH:mm:ss`), a `GTD` with its
//!    `goodTillDate`, and an opening `MTL` with TIF `AUC` and no `lmtPrice`: `OrderAccepted`, or an `OrderRejected`
//!    carrying IB's words (an unlisted rejection code is the core watchdog's, and this smoke reports it as NO ANSWER).
//! 2. **IB holds what was asked** (`READ BACK`): IB's open-order row, read on an observer client of its own, carries the
//!    term (`ON IB'S ROW` is the row's whole term list either way), and the socket reconcile's parser reads the row as
//!    the kind that was placed. `TERM NOT ON IB'S ROW` is the finding that keeps a row closed: IB took the order and
//!    dropped (or does not echo) the attribute.
//! 3. **The cancel ends each**: `OrderCanceled`. A final sweep lists every order of this run still resting and cancels
//!    it again; `LEFT RESTING` names one to cancel by hand.
//!
//! ⚠ **Two rows depend on the tree it runs on.** The `GTD` row is REFUSED by the bridge's own TIF check
//! (`deny_unsupported_time_in_force`, "is not supported on ibkr") on a tree whose `crates/vike-venues/src/venue_time_in_force.rs` ibkr row maps
//! `Gtd` to `Unsupported`; the row maps it behind this row's paper run of 2026-11-10. The AUCTION row
//! (`MTL` + `AUC`) is placed only OUTSIDE 12:30-21:00 UTC (at least an hour before the US open in either daylight
//! regime, and not inside the session), so it cannot reach its auction before the cancel that follows it at once; in
//! that window it prints `SKIPPED`.
//!
//! It dials the exec client under a client id of its own (from the clock, never one the daemon dials) and an observer
//! client three ids above it; the order client ids are `b1at…`. IB may answer a precaution about the far prices; the table then
//! carries IB's words, and the fix is prices nearer the market (still unfillable), never a bypassed precaution. The
//! only hard failures are a target that is not a paper `DU...` account and a run in which no submit was answered at
//! all. A run that prints no `ATTRIBUTES` table tested nothing (it self-skipped), whatever its exit code says.
//!
//! ⚠ It PLACES AND CANCELS REAL PAPER ORDERS. Never run it casually; the lane, the credential store and the fence are
//! `docs/ops/live-smoke-lane.md`'s, and no workflow runs it. On a the CI box lane (the gateway is on the box):
//!
//!     just the CI box <branch> env VIKE_SETTINGS_DIR=/var/lib/vike/settings VIKE_NO_LIVE_KEYS=true \
//!       cargo test -p vike-ibkr --features ibkr --test ibkr_smokes -- --ignored --nocapture ibkr_attributes_smoke::
//!
//! Triple-gated like the other live tests: `#[ignore]`; self-skips unless a demo ibkr account holding an `ACCOUNT`, on
//! the `socket` backend, exists and the gateway answers; and a hard assert refuses anything but a paper `DU...` account.
#![cfg(feature = "ibkr")]

use crate::common::{connect, ibkr_settings};

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ibapi::client::blocking::Client;
use ibapi::orders::{Order, OrderData, Orders};
use ibapi::subscriptions::SubscriptionItem;
use tokio::sync::mpsc::Receiver;
use tokio::sync::mpsc::error::TryRecvError;
use vike_execution::{ExecutionClient, Ingest, event_channel};
use vike_ibkr::config::load_ibkr_config_from;
use vike_ibkr::connect_execution_client;
use vike_ibkr::reconcile::socket::parse_socket_order_reports;
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::events::Event;
use vike_model::{
    AccountTier, OrderAttributes, OrderRelease, OrderRequest, OrderStopAdjust, OrderStopAdjustTo,
    OrderType, TimeInForce,
};
use vike_venues::IbkrBackend;

const SYMBOL: &str = "AAPL.SMART.USD";
/// Far below any AAPL price: a buy limit there rests unfilled.
const FAR_BID: f64 = 150.0;
/// Far beyond any distance AAPL moves in a run: a buy trailing stop starts this far above the market, and a buy
/// trailing if-touched order's trigger this far below its high.
const TRAIL: f64 = 200.0;
/// Half the price: as far as a percent trail goes.
const TRAIL_PERCENT: f64 = 50.0;
/// Far below any AAPL price: a sell stop there never fires.
const FAR_STOP: f64 = 10.0;
/// Far above any AAPL price: a sell stop's adjustment at this level is never made.
const ADJUST_AT: f64 = 5_000.0;
/// The adjusted stop, below `ADJUST_AT` (the near side, so the adjustment would not fire it at once).
const ADJUST_TO: f64 = 4_000.0;
/// The order client id prefix of this run's orders.
const PREFIX: &str = "b1at";

/// A client id this gateway has not seen from us: from the clock (the daemon's ids are small and configured).
fn fresh_id() -> i32 {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    crate::common::SMOKE_CLIENT_ID_BASE + 8_000 + (secs % 8_000) as i32
}

type Verdict = (&'static str, String);

/// A row: its name, the order, and the term IB's own row must carry.
type Row = (&'static str, OrderRequest, fn(&Order) -> bool);

/// The first event that `pick` answers, within `window`: `(verdict, detail)`, or NO ANSWER.
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

fn placed(rx: &mut Receiver<Ingest>, order_client_id: &str) -> Verdict {
    wait_for(rx, Duration::from_secs(20), |e| match e {
        Event::OrderAccepted(a) if a.order_client_id == order_client_id => {
            Some(("ACCEPTED", "OrderAccepted".into()))
        }
        Event::OrderFilled(f) if f.order_client_id == order_client_id => {
            Some(("FILLED", "OrderFilled: IT TRADED, close 1 AAPL by hand".into()))
        }
        Event::OrderRejected(r) if r.order_client_id == order_client_id => {
            Some(("REFUSED", format!("OrderRejected: {}", r.reason)))
        }
        _ => None,
    })
}

fn cancelled(rx: &mut Receiver<Ingest>, order_client_id: &str) -> Verdict {
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

/// Every open order row of this run the observer is told of (`reqAllOpenOrders`, every client of the login).
fn open_rows(observer: &Client) -> Result<Vec<OrderData>, String> {
    let sub = observer.all_open_orders().map_err(|e| format!("open orders: {e}"))?;
    let end = Instant::now() + Duration::from_secs(10);
    let mut rows = Vec::new();
    while Instant::now() < end {
        match sub.next_timeout(Duration::from_secs(3)) {
            Some(Ok(SubscriptionItem::Data(Orders::OrderData(o)))) => {
                if o.order.order_ref.starts_with(PREFIX) {
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

/// IB's row's whole term list, the attributes wave's fields included, so a run shows what IB holds either way.
fn terms(o: &Order) -> String {
    format!(
        "{} tif={} lmt={:?} aux={:?} offset={:?} pct={:?} hidden={} aon={} sweep={} disc={} gat={:?} gtd={:?} \
         adj={:?} trig={:?} adjStop={:?} adjLmt={:?} adjTrail={:?} unit={}",
        o.order_type,
        o.tif.as_str(),
        o.limit_price,
        o.aux_price,
        o.limit_price_offset,
        o.trailing_percent,
        o.hidden,
        o.all_or_none,
        o.sweep_to_fill,
        o.discretionary_amt,
        o.good_after_time,
        o.good_till_date,
        o.adjusted_order_type,
        o.trigger_price,
        o.adjusted_stop_price,
        o.adjusted_stop_limit_price,
        o.adjusted_trailing_amount,
        o.adjustable_trailing_unit
    )
}

/// What IB's row of `order_client_id` says: `READ BACK` when the reconcile's parser reads it as `want` and `held` finds the term.
fn read_back(
    observer: &Client,
    account: &str,
    order_client_id: &str,
    want: &OrderType,
    held: fn(&Order) -> bool,
) -> Verdict {
    let rows = match open_rows(observer) {
        Ok(rows) => rows,
        Err(e) => return ("NO READ", e),
    };
    let Some(row) = rows.into_iter().find(|r| r.order.order_ref == order_client_id) else {
        return ("NOT LISTED", format!("no open order under {order_client_id}"));
    };
    let on_row = terms(&row.order);
    let conid = row.contract.contract_id;
    let kind =
        parse_socket_order_reports(&[Orders::OrderData(row.clone())], account, conid, SYMBOL)
            .first()
            .map(|r| r.order_type.clone());
    match kind {
        Some(k) if k != *want => {
            ("WRONG KIND", format!("read as {k:?}, wanted {want:?}: {on_row}"))
        }
        None => ("NOT PARSED", format!("the reconcile parser reports no row: {on_row}")),
        Some(_) if !held(&row.order) => ("TERM NOT ON IB'S ROW", on_row),
        Some(_) => ("READ BACK", on_row),
    }
}

/// Whether now is outside 12:30-21:00 UTC: at least an hour before the US open (13:30 or 14:30 UTC) and after the close.
fn far_from_the_open() -> bool {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let minute = (secs % 86_400) / 60;
    !(12 * 60 + 30..21 * 60).contains(&minute)
}

#[test]
#[ignore = "live paper: places, reads back and cancels real paper AAPL orders of the attributes wave (hidden, all-or-none, sweep, discretion, TRAIL MIT/LIT, percent trails, adjustable stops, goodAfterTime, GTD, MTL+AUC) that cannot trade; needs a running Gateway logged into a DU... account"]
fn ibkr_attributes_place_read_back_cancel() {
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
            "skip: venue.ibkr.demo.backend is cpapi; the attributes go through the IB Gateway socket only"
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
    // The attributes are measured on the regular-session route: with the tier's extended-hours switch on, the bridge
    // refuses an iceberg, all-or-none, sweep-to-fill, discretion and goodAfterTime on a GTC/DAY stock limit (IB refused
    // each on its overnight venue, paper 2026-11-09), so this run turns the switch off for its own connection.
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
    std::thread::sleep(Duration::from_secs(2)); // the gateway's connect gap
    let addr = format!("{}:{}", config.host, config.port);
    let observer = match connect(&addr, config.client_id + 3) {
        Ok(o) => o,
        Err(e) => {
            client.detach();
            eprintln!("skip: the observer could not connect: {e}");
            return;
        }
    };

    let now_ms = vike_model::now_ms();
    let limit = OrderRequest {
        venue: "ibkr".into(),
        symbol: SYMBOL.into(),
        side: 1,
        qty: 1.0,
        order_type: OrderType::Limit,
        price: Some(FAR_BID),
        ..Default::default()
    };
    let attributed = |order_client_id: &str, attributes: OrderAttributes| OrderRequest {
        order_client_id: order_client_id.into(),
        attributes: Some(attributes),
        ..limit.clone()
    };
    let trailing = |order_client_id: &str, order_type: OrderType| OrderRequest {
        order_client_id: order_client_id.into(),
        order_type,
        price: None,
        trail: Some(TRAIL),
        ..limit.clone()
    };
    let percent =
        |r: OrderRequest| OrderRequest { trail: None, trail_percent: Some(TRAIL_PERCENT), ..r };
    let adjustable = |order_client_id: &str, to: OrderStopAdjustTo| OrderRequest {
        order_client_id: order_client_id.into(),
        side: -1,
        order_type: OrderType::Stop,
        price: None,
        trigger_price: Some(FAR_STOP),
        adjust: Some(OrderStopAdjust { at: ADJUST_AT, to }),
        ..limit.clone()
    };
    // (row, order, the term IB's row must carry)
    let fixed: [Row; 14] = [
        (
            "LMT hidden",
            OrderRequest {
                order_client_id: "b1athid".into(),
                display_quantity: Some(0.0),
                ..limit.clone()
            },
            |o| o.hidden,
        ),
        (
            "LMT all-or-none",
            attributed("b1ataon", OrderAttributes { all_or_none: true, ..Default::default() }),
            |o| o.all_or_none,
        ),
        (
            "LMT sweep-to-fill",
            attributed("b1atswp", OrderAttributes { sweep_to_fill: true, ..Default::default() }),
            |o| o.sweep_to_fill,
        ),
        (
            "LMT discretion 1.0",
            attributed(
                "b1atdsc",
                OrderAttributes { discretionary_amount: Some(1.0), ..Default::default() },
            ),
            |o| o.discretionary_amt == 1.0,
        ),
        ("TRAIL MIT buy", trailing("b1attmt", OrderType::TrailingTakeProfit), |o| {
            o.order_type == "TRAIL MIT"
        }),
        (
            "TRAIL LIT buy",
            OrderRequest {
                limit_offset: Some(1.0),
                ..trailing("b1attlt", OrderType::TrailingTakeProfit)
            },
            |o| o.order_type == "TRAIL LIT" && o.limit_price_offset == Some(1.0),
        ),
        ("TRAIL buy 50%", percent(trailing("b1attpc", OrderType::Trailing)), |o| {
            o.trailing_percent == Some(TRAIL_PERCENT)
        }),
        ("TRAIL MIT buy 50%", percent(trailing("b1atmpc", OrderType::TrailingTakeProfit)), |o| {
            o.trailing_percent == Some(TRAIL_PERCENT)
        }),
        (
            "STP sell, adjust to STP",
            adjustable("b1atas", OrderStopAdjustTo::Stop { stop: ADJUST_TO }),
            |o| o.adjusted_order_type == "STP" && o.trigger_price == Some(ADJUST_AT),
        ),
        (
            "STP sell, adjust to STP LMT",
            adjustable(
                "b1atasl",
                OrderStopAdjustTo::StopLimit { stop: ADJUST_TO, limit: ADJUST_TO - 10.0 },
            ),
            |o| o.adjusted_order_type == "STP LMT" && o.adjusted_stop_limit_price.is_some(),
        ),
        (
            "STP sell, adjust to TRAIL",
            adjustable(
                "b1atat",
                OrderStopAdjustTo::Trailing {
                    stop: ADJUST_TO,
                    trail: Some(10.0),
                    trail_percent: None,
                },
            ),
            |o| o.adjusted_order_type == "TRAIL" && o.adjustable_trailing_unit == 0,
        ),
        (
            "STP sell, adjust to TRAIL 1%",
            adjustable(
                "b1atatp",
                OrderStopAdjustTo::Trailing {
                    stop: ADJUST_TO,
                    trail: None,
                    trail_percent: Some(1.0),
                },
            ),
            |o| o.adjusted_order_type == "TRAIL" && o.adjustable_trailing_unit == 100,
        ),
        (
            "LMT goodAfterTime +1h",
            OrderRequest {
                order_client_id: "b1atgat".into(),
                release: Some(OrderRelease { at: now_ms + 3_600_000, price: None }),
                ..limit.clone()
            },
            |o| !o.good_after_time.is_empty(),
        ),
        (
            "LMT GTD +1d",
            OrderRequest {
                order_client_id: "b1atgtd".into(),
                time_in_force: TimeInForce::Gtd,
                gtd_expiry: Some(now_ms + 86_400_000),
                ..limit.clone()
            },
            |o| !o.good_till_date.is_empty(),
        ),
    ];
    let mut rows = Vec::from(fixed);
    let auction_open = far_from_the_open();
    if auction_open {
        rows.push((
            "MTL AUC (opening auction)",
            OrderRequest {
                order_client_id: "b1atauc".into(),
                order_type: OrderType::MarketToLimit,
                price: None,
                time_in_force: TimeInForce::AtOpen,
                ..limit.clone()
            },
            |o| o.order_type == "MTL" && o.tif.as_str() == "AUC",
        ));
    }

    let mut table: Vec<(String, Verdict)> = Vec::new();
    for (row, order, held) in &rows {
        let order_client_id = order.order_client_id.as_str();
        client.submit(order);
        let verdict = placed(&mut rx, order_client_id);
        let accepted = verdict.0 == "ACCEPTED";
        table.push((format!("{row}: submit"), verdict));
        if accepted {
            std::thread::sleep(Duration::from_secs(1)); // the gateway's open-order table catches up
            table.push((
                format!("{row}: IB's row"),
                read_back(&observer, &config.account, order_client_id, &order.order_type, *held),
            ));
        }
        // Cancel whatever the answer was: an order IB holds must not be left resting.
        client.cancel(order_client_id);
        table.push((format!("{row}: cancel"), cancelled(&mut rx, order_client_id)));
    }
    if !auction_open {
        table.push((
            "MTL AUC (opening auction)".into(),
            ("SKIPPED", "inside 12:30-21:00 UTC: too near the open or inside the session".into()),
        ));
    }

    // Nothing of this run may stay resting: cancel again whatever IB still lists, and name what survives that.
    std::thread::sleep(Duration::from_secs(2));
    if let Ok(left) = open_rows(&observer) {
        for row in &left {
            client.cancel(&row.order.order_ref);
            let verdict = cancelled(&mut rx, &row.order.order_ref);
            table.push((format!("sweep {}", row.order.order_ref), verdict));
        }
        std::thread::sleep(Duration::from_secs(2));
        for row in open_rows(&observer).unwrap_or_default() {
            table.push((
                format!("LEFT RESTING {}", row.order.order_ref),
                ("CANCEL BY HAND", format!("IB order id {}", row.order_id)),
            ));
        }
    }
    client.detach();
    observer.disconnect();

    eprintln!(
        "ATTRIBUTES ({SYMBOL}, paper; buy limits at {FAR_BID}, trail {TRAIL} or {TRAIL_PERCENT}%, sell stops at \
         {FAR_STOP} adjusting at {ADJUST_AT}; extended hours OFF for this run, the tier's switch is {})",
        if tier_extended_hours { "ON" } else { "off" }
    );
    for (step, (verdict, detail)) in &table {
        eprintln!("  {step:<40}  {verdict:<20}  {detail}");
    }
    assert!(
        table
            .iter()
            .any(|(step, (verdict, _))| step.ends_with("submit") && *verdict != "NO ANSWER"),
        "no submit was answered: nothing was measured"
    );
}
