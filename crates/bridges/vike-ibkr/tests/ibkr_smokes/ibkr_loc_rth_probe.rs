//! LIVE PAPER probe - **does IB answer an auction-at-close order placed DURING the regular session?** On paper #12 on
//! 2026-11-10 a limit-on-close (`LOC`, DAY, buy 1 AAPL at 150) placed before the open was answered at once (`[399]` "will
//! not be placed at the exchange until 09:30 US/Eastern", read back), while the same order placed at 14:43 and 15:06 UTC,
//! inside the session, drew NO frame at all for its order id until its cancel (then `openOrder`, two `orderStatus` and
//! `[202] Order Canceled`), and a GTD limit beside it was answered at once. This probe asks IB directly, through `ibapi`
//! (no bridge in between), one row per case, printing every frame naming the order with its UTC time and the seconds since
//! the `placeOrder`:
//!
//! | case | order | what it answers |
//! |---|---|---|
//! | `control` | `LMT` DAY at the far bid, SMART | the session answers at all |
//! | `loc-smart` | `LOC` DAY at the far bid, SMART, [`LONG_WINDOW`] | does IB ever answer if we wait, and does `reqAllOpenOrders` (an observer client) or `reqOpenOrders` (the placing client) list it meanwhile |
//! | `loc-smart-primary` | the same with `primaryExchange` NASDAQ | does naming the primary change it |
//! | `loc-island` | the same routed to `ISLAND` (NASDAQ direct) | does a direct route change it |
//! | `moc-smart` | `MOC` DAY, SMART (only before 20:30 UTC) | does a market-on-close get an answer |
//! | `loc-near` | `LOC` DAY 4 percent under the delayed last price, SMART | does a price inside the node's 5 percent band change it |
//!
//! ⚠ **MEASURED 2026-11-10, twice (15:21-15:25 and 15:30-15:35 UTC, paper #12):** `control` `PreSubmitted` in 0.3-0.5 s and
//! `Submitted` 0.04 s later. `loc-smart`, `loc-smart-primary`, `moc-smart` and `loc-near` (the second run's: 336.66, priced
//! off the previous close 350.69 because this probe then read only `Price` ticks; 3 percent under the delayed last 347.0)
//! drew NO frame on the placing client until their cancel (153 s and 33 s), while every `reqAllOpenOrders` read listed them
//! `PendingSubmit` (from 5.4 s on, with no `whyHeld` and no warning) and the placing client's `reqOpenOrders` listed them
//! too; each cancel was answered in 0.15 s (`PendingCancel`, `Cancelled`, `[202]`). `loc-island` was answered at once: the
//! `[10311]` direct-routing notice, `Cancelled` and `[201] Order rejected - reason:Order was discarded.`
//!
//! Every order is cancelled right after its window, a sweep cancels again every order this client placed, and an
//! observer then reads this run's open orders (order client ids `b4lr…`) and the AAPL position. Outside 14:30-20:30 UTC (the US
//! regular session in winter time, less its last half hour) it places nothing and says so.
//!
//! ⚠ It PLACES AND CANCELS REAL PAPER ORDERS (a MOC among them: it would buy 1 AAPL at the close if a cancel were lost).
//! Never run it casually; the lane, the credential store and the fence are `docs/ops/live-smoke-lane.md`'s:
//!
//!     just the CI box <branch> env VIKE_SETTINGS_DIR=/var/lib/vike/settings VIKE_NO_LIVE_KEYS=true \
//!       cargo test -p vike-ibkr --features ibkr --test ibkr_smokes -- --ignored --nocapture ibkr_loc_rth_probe::
//!
//! Triple-gated like the other live tests: `#[ignore]`; self-skips unless a demo ibkr account holding an `ACCOUNT`, on
//! the `socket` backend, exists and the gateway answers; and a hard assert refuses anything but a paper `DU...` account.
#![cfg(feature = "ibkr")]

use crate::common::{aapl, connect, ibkr_settings, observe, stock};

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ibapi::client::blocking::{Client, Subscription};
use ibapi::contracts::tick_types::TickType;
use ibapi::contracts::{Contract, Exchange};
use ibapi::market_data::MarketDataType;
use ibapi::market_data::realtime::TickTypes;
use ibapi::orders::{Action, Order, OrderUpdate, Orders, TimeInForce as IbTif};
use ibapi::subscriptions::SubscriptionItem;
use vike_ibkr::config::load_ibkr_config_from;
use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;
use vike_venues::IbkrBackend;

/// Far below any AAPL price: a buy limit there rests unfilled, at the close too.
const FAR_BID: f64 = 150.0;
/// How long the first limit-on-close is watched.
const LONG_WINDOW: Duration = Duration::from_secs(150);
/// How long every other row is watched.
const SHORT_WINDOW: Duration = Duration::from_secs(30);
/// This run's order client id prefix.
const PREFIX: &str = "b4lr";

fn utc_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn utc_now() -> String {
    let ms = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis());
    let s = (ms / 1000) % 86_400;
    format!("{:02}:{:02}:{:02}.{:03}Z", s / 3600, (s / 60) % 60, s % 60, ms % 1000)
}

/// Three client ids this gateway has not seen from us (exec, observer, data), from the clock, in 57000-59999 above the
/// base: past the other smokes' clock ranges and below the fixed ids.
fn fresh_ids() -> [i32; 3] {
    let base = crate::common::SMOKE_CLIENT_ID_BASE + 56_000 + (utc_secs() % 1_000) as i32 * 3;
    [base, base + 1, base + 2]
}

/// The delayed (or live) last price of AAPL within 20 s (every tick read is printed). The last comes as a price-and-size
/// tick (2026-11-10: `DelayedBid`, `DelayedAsk` and `DelayedLast` as `PriceSize`, the previous close as a `Price`).
fn aapl_price(addr: &str, id: i32) -> Option<f64> {
    let client = connect(addr, id).ok()?;
    client.switch_market_data_type(MarketDataType::Delayed).ok()?;
    let sub = client.market_data(&aapl()).streaming().subscribe().ok()?;
    let end = Instant::now() + Duration::from_secs(20);
    let mut found = None;
    while Instant::now() < end && found.is_none() {
        match sub.next_timeout(Duration::from_millis(500)) {
            Some(Ok(SubscriptionItem::Data(TickTypes::PriceSize(p)))) => {
                eprintln!("AAPL tick {:?} {}", p.price_tick_type, p.price);
                if p.price > 0.0
                    && matches!(p.price_tick_type, TickType::DelayedLast | TickType::Last)
                {
                    found = Some(p.price);
                }
            }
            Some(Ok(other)) => eprintln!("AAPL data frame {other:?}"),
            Some(Err(e)) => eprintln!("AAPL data error {e}"),
            None => {}
        }
    }
    drop(sub);
    client.disconnect();
    found
}

/// Print every frame of the order-update stream naming `order_id` for `window`; how many there were.
fn drain(
    stream: &Subscription<OrderUpdate>,
    order_id: i32,
    window: Duration,
    case: &str,
    t0: Instant,
) -> usize {
    let end = Instant::now() + window;
    let mut seen = 0;
    let at = || format!("{} +{:>7.3}s", utc_now(), t0.elapsed().as_secs_f64());
    while Instant::now() < end {
        let line = match stream.next_timeout(Duration::from_millis(200)) {
            Some(Ok(SubscriptionItem::Data(OrderUpdate::OpenOrder(o))))
                if o.order_id == order_id =>
            {
                format!(
                    "openOrder status={} type={} tif={} lmt={:?} warning={:?} reject={:?}",
                    o.order_state.status.as_str(),
                    o.order.order_type,
                    o.order.tif.as_str(),
                    o.order.limit_price,
                    o.order_state.warning_text,
                    o.order_state.reject_reason
                )
            }
            Some(Ok(SubscriptionItem::Data(OrderUpdate::OrderStatus(s))))
                if s.order_id == order_id =>
            {
                format!(
                    "orderStatus status={} filled={} remaining={} whyHeld={:?} permId={}",
                    s.status.as_str(),
                    s.filled,
                    s.remaining,
                    s.why_held,
                    s.perm_id
                )
            }
            Some(Ok(SubscriptionItem::Notice(n))) if n.request_id == Some(order_id) => {
                format!("notice [{}] {}", n.code, n.message)
            }
            Some(Ok(SubscriptionItem::Data(other))) => format!("(another frame) {other:?}"),
            Some(Ok(SubscriptionItem::Notice(n))) => {
                format!("(a notice for {:?}) [{}] {}", n.request_id, n.code, n.message)
            }
            Some(Err(e)) => format!("stream error: {e}"),
            None => continue,
        };
        seen += 1;
        eprintln!("[{case}] {}  {line}", at());
    }
    seen
}

/// What an open-orders read lists for `order_client_id`, as one line.
fn listed(sub: Result<Subscription<Orders>, ibapi::Error>, order_client_id: &str) -> String {
    let sub = match sub {
        Ok(s) => s,
        Err(e) => return format!("read failed: {e}"),
    };
    let end = Instant::now() + Duration::from_secs(8);
    let mut rows = Vec::new();
    let mut ids = Vec::new();
    while Instant::now() < end {
        match sub.next_timeout(Duration::from_secs(2)) {
            Some(Ok(SubscriptionItem::Data(Orders::OrderData(o))))
                if o.order.order_ref == order_client_id =>
            {
                ids.push(o.order_id);
                rows.push(format!(
                    "openOrder id {} status={} {} tif={} lmt={:?} warning={:?}",
                    o.order_id,
                    o.order_state.status.as_str(),
                    o.order.order_type,
                    o.order.tif.as_str(),
                    o.order.limit_price,
                    o.order_state.warning_text
                ));
            }
            Some(Ok(SubscriptionItem::Data(Orders::OrderStatus(s))))
                if ids.contains(&s.order_id) =>
            {
                rows.push(format!(
                    "orderStatus status={} whyHeld={:?}",
                    s.status.as_str(),
                    s.why_held
                ));
            }
            Some(Ok(_)) => {}
            Some(Err(e)) => {
                rows.push(format!("read error: {e}"));
                break;
            }
            None => break,
        }
    }
    if rows.is_empty() { "NOT LISTED".into() } else { format!("LISTED: {}", rows.join(" | ")) }
}

struct Case {
    name: &'static str,
    contract: Contract,
    order: Order,
    window: Duration,
    /// Seconds after the `placeOrder` at which the observer reads `reqAllOpenOrders`.
    polls: &'static [u64],
}

fn run_case(
    client: &Client,
    stream: &Subscription<OrderUpdate>,
    observer: &Client,
    order_id: i32,
    case: &Case,
) {
    let name = case.name;
    let o = &case.order;
    eprintln!(
        "[{name}] {} placeOrder id {order_id}: {} {} {} tif={} lmt={:?} on {} (primary {:?}) order client id {}",
        utc_now(),
        o.action.as_str(),
        o.total_quantity,
        o.order_type,
        o.tif.as_str(),
        o.limit_price,
        case.contract.exchange.as_str(),
        case.contract.primary_exchange.as_str(),
        o.order_ref
    );
    let t0 = Instant::now();
    if let Err(e) = client.submit_order(order_id, &case.contract, o) {
        eprintln!("[{name}] submit failed: {e}");
        return;
    }
    let mut first_frame: Option<f64> = None;
    let mut polls = case.polls.iter().peekable();
    while t0.elapsed() < case.window {
        if let Some(&&p) = polls.peek()
            && t0.elapsed() >= Duration::from_secs(p)
        {
            polls.next();
            eprintln!(
                "[{name}] {} +{:>7.3}s reqAllOpenOrders (observer client): {}",
                utc_now(),
                t0.elapsed().as_secs_f64(),
                listed(observer.all_open_orders(), &o.order_ref)
            );
        }
        if drain(stream, order_id, Duration::from_millis(500), name, t0) > 0 {
            first_frame.get_or_insert(t0.elapsed().as_secs_f64());
        }
        // Answered: a few seconds more for the rest of the answer, then on.
        if first_frame.is_some_and(|f| t0.elapsed().as_secs_f64() > f + 4.0)
            && polls.peek().is_none()
        {
            break;
        }
    }
    eprintln!(
        "[{name}] {} +{:>7.3}s reqOpenOrders (the placing client): {}",
        utc_now(),
        t0.elapsed().as_secs_f64(),
        listed(client.open_orders(), &o.order_ref)
    );
    drain(stream, order_id, Duration::from_secs(3), name, t0);
    eprintln!(
        "[{name}] {} +{:>7.3}s cancelOrder id {order_id} (first frame before it: {})",
        utc_now(),
        t0.elapsed().as_secs_f64(),
        first_frame.map_or("NONE".into(), |f| format!("+{f:.3}s"))
    );
    let _ = client.cancel_order(order_id, "");
    drain(stream, order_id, Duration::from_secs(5), name, t0);
}

#[test]
#[ignore = "live paper, US regular session only: places and cancels LOC, MOC and a control LMT of 1 AAPL, far from the market; needs a running Gateway logged into a DU... account"]
fn ibkr_loc_rth_probe() {
    vike_log::test_init();

    let settings_dir = std::env::var("VIKE_SETTINGS_DIR").ok();
    let dir = vike_secrets::smoke_directory();
    let creds = dir.credentials_for("ibkr", &AccountLabel::Default, AccountTier::Demo);
    let Some(config) =
        load_ibkr_config_from(AccountTier::Demo, &creds, &ibkr_settings(settings_dir.as_deref()))
    else {
        eprintln!("skip: no demo ibkr account holding an `ACCOUNT` in the settings database");
        return;
    };
    if config.backend == IbkrBackend::Cpapi {
        eprintln!(
            "skip: venue.ibkr.demo.backend is cpapi; this probe speaks the IB Gateway socket"
        );
        return;
    }
    // HARD SAFETY: paper only - this assert stays a hard failure, never a skip.
    assert!(
        config.account.starts_with("DU"),
        "the probe must target a paper (DU\u{2026}) account, got {}",
        config.account
    );
    let minute = (utc_secs() % 86_400) / 60;
    if !(14 * 60 + 30..20 * 60 + 30).contains(&minute) {
        eprintln!("skip: outside 14:30-20:30 UTC, the probe places nothing");
        return;
    }
    let addr = format!("{}:{}", config.host, config.port);
    let [exec_id, observer_id, data_id] = fresh_ids();
    eprintln!(
        "gateway {addr}, paper account DU\u{2026}, client ids exec={exec_id} observer={observer_id} data={data_id}"
    );

    let price = aapl_price(&addr, data_id);
    eprintln!("AAPL delayed last: {price:?}");

    let client = match connect(&addr, exec_id) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("skip: could not connect as {exec_id}: {e}");
            return;
        }
    };
    let observer = match connect(&addr, observer_id) {
        Ok(c) => c,
        Err(e) => {
            client.disconnect();
            eprintln!("skip: the observer could not connect as {observer_id}: {e}");
            return;
        }
    };
    let stream = client.order_update_stream().expect("the order-update stream");
    let first_id = client.next_valid_order_id().expect("IB's next valid order id");

    let order = |order_client_id: &str, order_type: &str, limit: Option<f64>| Order {
        action: Action::Buy,
        total_quantity: 1.0,
        order_type: order_type.into(),
        limit_price: limit,
        tif: IbTif::Day,
        order_ref: format!("{PREFIX}{order_client_id}"),
        account: config.account.clone(),
        ..Order::default()
    };
    let primary = Contract { primary_exchange: Exchange::from("NASDAQ"), ..aapl() };
    let mut cases = vec![
        Case {
            name: "control",
            contract: aapl(),
            order: order("lmt", "LMT", Some(FAR_BID)),
            window: Duration::from_secs(5),
            polls: &[],
        },
        Case {
            name: "loc-smart",
            contract: aapl(),
            order: order("loc", "LOC", Some(FAR_BID)),
            window: LONG_WINDOW,
            polls: &[5, 30, 90, 140],
        },
        Case {
            name: "loc-smart-primary",
            contract: primary,
            order: order("locp", "LOC", Some(FAR_BID)),
            window: SHORT_WINDOW,
            polls: &[10],
        },
        Case {
            name: "loc-island",
            contract: stock("AAPL", "ISLAND", "USD"),
            order: order("loci", "LOC", Some(FAR_BID)),
            window: SHORT_WINDOW,
            polls: &[10],
        },
    ];
    // A market-on-close only well before the close's last half hour (the cancel must come long before its auction).
    if minute < 20 * 60 {
        cases.push(Case {
            name: "moc-smart",
            contract: aapl(),
            order: order("moc", "MOC", None),
            window: SHORT_WINDOW,
            polls: &[10],
        });
    } else {
        eprintln!("[moc-smart] SKIPPED: past 20:00 UTC");
    }
    match price {
        Some(p) => {
            let near = ((p * 0.96) * 100.0).floor() / 100.0;
            cases.push(Case {
                name: "loc-near",
                contract: aapl(),
                order: order("locn", "LOC", Some(near)),
                window: SHORT_WINDOW,
                polls: &[10],
            });
        }
        None => eprintln!("[loc-near] SKIPPED: no AAPL price was read"),
    }

    let mut placed = Vec::new();
    for (n, case) in cases.iter().enumerate() {
        let order_id = first_id + n as i32;
        placed.push(order_id);
        run_case(&client, &stream, &observer, order_id, case);
    }

    // Sweep: cancel by id every order this client placed (an already-gone one answers a harmless notice). Never
    // `reqGlobalCancel`: it would cancel every client's orders, the daemon's included.
    for order_id in &placed {
        let _ = client.cancel_order(*order_id, "");
    }
    std::thread::sleep(Duration::from_secs(3));
    drop(stream);
    client.disconnect();
    observer.disconnect();
    match observe(&addr, observer_id + 3, &config.account, "AAPL", PREFIX) {
        Ok((open, position)) => {
            eprintln!("END: this run's open orders {open:?}; AAPL position {position}");
            assert!(open.is_empty(), "LEFT RESTING, cancel by hand: {open:?}");
        }
        Err(e) => eprintln!(
            "END: the observer could not read: {e} - check by hand that nothing of {PREFIX}* rests"
        ),
    }
}
