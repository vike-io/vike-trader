//! LIVE PAPER smoke - the VARIANT TABLE of what IBKR accepts for a far US-stock LIMIT order flagged for extended
//! hours, and the bridge's own flagged order through it. Places and cancels EIGHT real orders (buy 1 AAPL @150,
//! coids `b9ibs1`..`b9ibs8`) on the PAPER account; none can fill.
//!
//! The defect behind it (v0.1.88, 2026-10-25): with `venue.ibkr.<tier>.extended_hours` on, every stock LIMIT order is
//! sent with `outsideRth` and `includeOvernight`, and the node's `submit` always sends GTC. IB answered each one,
//! 2-3 ms after the `placeOrder`, with error 10052 "Invalid time in force:GTC" and no order status. Not known then:
//! WHICH flag causes it, and whether DAY is accepted. This table is the answer, one order per row:
//!
//! | coid | flags | tif |
//! |---|---|---|
//! | `b9ibs1` | none | GTC |
//! | `b9ibs2` | outsideRth | GTC |
//! | `b9ibs3` | outsideRth | DAY |
//! | `b9ibs4` | includeOvernight | DAY |
//! | `b9ibs5` | outsideRth + includeOvernight | DAY |
//! | `b9ibs6` | includeOvernight | GTC |
//! | `b9ibs7` | outsideRth + includeOvernight | GTC (the defect, reproduced) |
//! | `b9ibs8` | the BRIDGE: `extended_hours` on, a GTC request, through `IbkrExecutionClient` | what it sends |
//!
//! Rows 1-7 go straight through `ibapi` on a client id of their own (taken from the clock, never one the daemon
//! dials), so each flag and TIF is exactly the row's; row 8 is the production path and proves the fix end to end
//! (`OrderAccepted`, or `OrderRejected` carrying IB's words, never a silence). Each accepted order is cancelled at
//! once, so it rests about a second: a reconcile pass of the node that lands in that second sees an order it does not
//! know. The table is printed at the end (`--nocapture`); its ONLY hard failure is a run in which nothing was answered.
//! A run that prints no `VARIANT TABLE` line did not test anything (it self-skipped), whatever its exit code says.
//!
//! ⚠ It PLACES AND CANCELS REAL PAPER ORDERS. Never run it casually; the lane, the credential store and the fence are
//! `docs/ops/live-smoke-lane.md`'s, and no workflow runs it. On a the CI box lane (the gateway is on the box):
//!
//!     just the CI box <branch> env VIKE_SETTINGS_DIR=/var/lib/vike/settings VIKE_NO_LIVE_KEYS=true \
//!       cargo test -p vike-ibkr --features ibkr --test ibkr_flagged_tif_smoke -- --ignored --nocapture
//!
//! Triple-gated like the other live tests: `#[ignore]`; self-skips unless a demo ibkr account holding an `ACCOUNT`, on
//! the `socket` backend, exists and the gateway answers; and a hard assert refuses anything but a paper `DU...` account.
//! What it cannot show: the overnight SESSION itself (IB's opens Sunday 20:00 New York = Monday 00:00 UTC); a row's
//! answer outside that session is validation only, which is where a 10052 is raised.
#![cfg(feature = "ibkr")]

mod common;
use common::ibkr_settings;

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ibapi::client::blocking::{Client, Subscription};
use ibapi::contracts::{Contract, Currency, Exchange, SecurityType, Symbol};
use ibapi::orders::{Action, Order, OrderUpdate, TimeInForce as IbTif};
use ibapi::subscriptions::SubscriptionItem;
use tokio::sync::mpsc::error::TryRecvError;
use vike_exec::{ExecutionClient, Ingest, event_channel};
use vike_ibkr::IbkrExecutionClient;
use vike_ibkr::config::load_ibkr_config_from;
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::events::Event;
use vike_model::{AccountTier, OrderRequest};
use vike_venues::IbkrBackend;

/// Far below any AAPL price: the order rests, never fills.
const PRICE: f64 = 150.0;
/// How long IB gets to answer one order (it answers in milliseconds; a refusal needs no longer).
const ANSWER_WINDOW: Duration = Duration::from_secs(4);
/// After the first answer, how long to keep listening for the rest of it (an order status after its openOrder).
const AFTER_FIRST_ANSWER: Duration = Duration::from_millis(1500);

/// One row of the table that goes straight through `ibapi`.
struct Variant {
    coid: &'static str,
    name: &'static str,
    outside_rth: bool,
    include_overnight: bool,
    tif: IbTif,
}

fn variants() -> Vec<Variant> {
    let v = |coid, name, outside_rth, include_overnight, tif| Variant {
        coid,
        name,
        outside_rth,
        include_overnight,
        tif,
    };
    vec![
        v("b9ibs1", "no flags + GTC", false, false, IbTif::GoodTillCanceled),
        v("b9ibs2", "outsideRth + GTC", true, false, IbTif::GoodTillCanceled),
        v("b9ibs3", "outsideRth + DAY", true, false, IbTif::Day),
        v("b9ibs4", "includeOvernight + DAY", false, true, IbTif::Day),
        v("b9ibs5", "outsideRth + includeOvernight + DAY", true, true, IbTif::Day),
        v("b9ibs6", "includeOvernight + GTC", false, true, IbTif::GoodTillCanceled),
        v(
            "b9ibs7",
            "outsideRth + includeOvernight + GTC (the defect)",
            true,
            true,
            IbTif::GoodTillCanceled,
        ),
    ]
}

/// What IB said about one order, as the order-update stream carried it.
#[derive(Default)]
struct Reply {
    /// `openOrder:<status>` / `orderStatus:<status>` rows naming the order.
    statuses: Vec<String>,
    /// `(code, text)` of every notice naming the order.
    notices: Vec<(i32, String)>,
}

impl Reply {
    fn verdict(&self) -> &'static str {
        let has =
            |words: &[&str]| self.statuses.iter().any(|s| words.iter().any(|w| s.ends_with(w)));
        if has(&["Inactive", "Cancelled", "ApiCancelled"]) {
            "REFUSED"
        } else if has(&["PreSubmitted", "Submitted", "PendingSubmit"]) {
            "ACCEPTED"
        } else if !self.notices.is_empty() {
            "REFUSED"
        } else {
            "NO ANSWER"
        }
    }

    fn detail(&self) -> String {
        let notices: Vec<String> =
            self.notices.iter().map(|(code, text)| format!("[{code}] {text}")).collect();
        format!("statuses {:?}; notices {}", self.statuses, notices.join(" | "))
    }
}

/// Read the order-update stream for the order `order_id` until `window` is up, or a beat after its first answer.
fn collect(stream: &Subscription<OrderUpdate>, order_id: i32, window: Duration) -> Reply {
    let mut reply = Reply::default();
    let deadline = Instant::now() + window;
    let mut first_answer: Option<Instant> = None;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() || first_answer.is_some_and(|t| t.elapsed() > AFTER_FIRST_ANSWER) {
            break;
        }
        match stream.next_timeout(left.min(Duration::from_millis(250))) {
            Some(Ok(SubscriptionItem::Data(OrderUpdate::OpenOrder(o))))
                if o.order_id == order_id =>
            {
                reply.statuses.push(format!("openOrder:{}", o.order_state.status.as_str()));
            }
            Some(Ok(SubscriptionItem::Data(OrderUpdate::OrderStatus(s))))
                if s.order_id == order_id =>
            {
                reply.statuses.push(format!("orderStatus:{}", s.status.as_str()));
            }
            Some(Ok(SubscriptionItem::Notice(n))) if n.request_id == Some(order_id) => {
                reply.notices.push((n.code, n.message));
            }
            Some(Err(e)) => {
                reply.notices.push((0, format!("stream error: {e}")));
                break;
            }
            Some(Ok(_)) | None => continue,
        }
        first_answer.get_or_insert_with(Instant::now);
    }
    reply
}

fn aapl() -> Contract {
    Contract {
        symbol: Symbol::from("AAPL"),
        security_type: SecurityType::Stock,
        exchange: Exchange::from("SMART"),
        currency: Currency::from("USD"),
        ..Contract::default()
    }
}

fn order_of(v: &Variant, account: &str) -> Order {
    Order {
        action: Action::Buy,
        total_quantity: 1.0,
        order_type: "LMT".to_string(),
        limit_price: Some(PRICE),
        tif: v.tif.clone(),
        order_ref: v.coid.to_string(),
        account: account.to_string(),
        outside_rth: v.outside_rth,
        include_overnight: v.include_overnight,
        ..Order::default()
    }
}

/// Two client ids this gateway has not seen from us: from the clock, so a rerun an hour later gets others (the
/// daemon's ids are small and configured; these are `1000 + 4k`).
fn fresh_ids() -> [i32; 2] {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let base = 1_000 + (secs % 8_000) as i32 * 4;
    [base, base + 1]
}

/// Connect, retrying a few times: an id the Gateway has only just been told to release answers `[326]` for a moment.
fn connect(addr: &str, id: i32) -> Result<Client, String> {
    let mut last = String::new();
    for _ in 0..5 {
        match Client::connect(addr, id) {
            Ok(client) => return Ok(client),
            Err(e) => last = e.to_string(),
        }
        std::thread::sleep(Duration::from_secs(3));
    }
    Err(last)
}

/// The row-8 order through the PRODUCTION path: `extended_hours` on, the GTC the node always sends, no TIF chosen here.
/// Returns the verdict and what the bridge said.
fn through_the_bridge(
    mut cfg: vike_ibkr::config::IbkrConfig,
    client_id: i32,
) -> (&'static str, String) {
    cfg.client_id = client_id;
    cfg.extended_hours = true;
    let (events, mut rx) = event_channel(256);
    let mut client = match IbkrExecutionClient::connect(&cfg, events) {
        Ok(c) => c,
        Err(e) => return ("NO ANSWER", format!("the bridge could not connect: {e}")),
    };
    let coid = "b9ibs8".to_string();
    client.submit(&OrderRequest {
        client_order_id: coid.clone(),
        venue: "ibkr".into(),
        symbol: "AAPL.SMART.USD".into(),
        side: 1,
        qty: 1.0,
        order_type: vike_model::OrderType::Limit,
        price: Some(PRICE),
        ..Default::default()
    });
    let mut out = ("NO ANSWER", "no OrderAccepted / OrderRejected within 20 s".to_string());
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        match rx.try_recv() {
            Ok(Ingest::Event(Event::OrderAccepted(e))) if e.client_order_id == coid => {
                out = ("ACCEPTED", "OrderAccepted".to_string());
                break;
            }
            Ok(Ingest::Event(Event::OrderRejected(e))) if e.client_order_id == coid => {
                out = ("REFUSED", format!("OrderRejected: {}", e.reason));
                break;
            }
            Ok(_) => {}
            Err(TryRecvError::Empty) => std::thread::sleep(Duration::from_millis(50)),
            Err(TryRecvError::Disconnected) => break,
        }
    }
    if out.0 == "ACCEPTED" {
        client.cancel(&coid);
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut canceled = false;
        while Instant::now() < deadline && !canceled {
            match rx.try_recv() {
                Ok(Ingest::Event(Event::OrderCanceled(e))) if e.client_order_id == coid => {
                    canceled = true;
                }
                Ok(_) => {}
                Err(TryRecvError::Empty) => std::thread::sleep(Duration::from_millis(50)),
                Err(TryRecvError::Disconnected) => break,
            }
        }
        out.1 = format!(
            "{}; cancel {}",
            out.1,
            if canceled { "confirmed" } else { "NOT seen in 10 s" }
        );
    }
    client.detach();
    out
}

#[test]
#[ignore = "live paper: places and cancels eight real paper orders; needs a running Gateway logged into a DU... account"]
fn ibkr_flagged_tif_variant_table() {
    vike_log::test_init();

    let settings_dir = std::env::var("VIKE_SETTINGS_DIR").ok();
    let dir = vike_secrets::smoke_directory();
    let creds = dir.credentials_for("ibkr", &AccountLabel::Default, AccountTier::Demo);
    let Some(cfg) =
        load_ibkr_config_from(AccountTier::Demo, &creds, &ibkr_settings(settings_dir.as_deref()))
    else {
        eprintln!("skip: no demo ibkr account holding an `ACCOUNT` in the settings database");
        return;
    };
    if cfg.backend == IbkrBackend::Cpapi {
        eprintln!(
            "skip: venue.ibkr.demo.backend is cpapi; this smoke speaks the IB Gateway socket"
        );
        return;
    }
    // HARD SAFETY: paper only - this assert stays a hard failure, never a skip.
    assert!(
        cfg.account.starts_with("DU"),
        "smoke must target a paper (DU\u{2026}) account, got {}",
        cfg.account
    );
    let addr = format!("{}:{}", cfg.host, cfg.port);
    let [raw_id, bridge_id] = fresh_ids();
    eprintln!(
        "gateway {addr}, paper account DU\u{2026}, client ids raw={raw_id} bridge={bridge_id}"
    );

    let mut table: Vec<(String, String, &'static str, String)> = Vec::new();

    match connect(&addr, raw_id) {
        Err(e) => eprintln!("skip: could not connect to {addr} as client id {raw_id}: {e}"),
        Ok(client) => {
            let stream = client.order_update_stream().expect("the order-update stream");
            let first_id = client.next_valid_order_id().expect("IB's next valid order id");
            let mut placed: Vec<i32> = Vec::new();
            for (n, v) in variants().iter().enumerate() {
                let order_id = first_id + n as i32;
                eprintln!("placing {} (order id {order_id}): {}", v.coid, v.name);
                if let Err(e) = client.submit_order(order_id, &aapl(), &order_of(v, &cfg.account)) {
                    table.push((
                        v.coid.into(),
                        v.name.into(),
                        "NO ANSWER",
                        format!("submit failed: {e}"),
                    ));
                    continue;
                }
                placed.push(order_id);
                let reply = collect(&stream, order_id, ANSWER_WINDOW);
                let verdict = reply.verdict();
                let mut detail = reply.detail();
                if verdict == "ACCEPTED" {
                    let _ = client.cancel_order(order_id, "");
                    let after = collect(&stream, order_id, Duration::from_secs(3));
                    detail = format!("{detail}; after cancel: {}", after.detail());
                }
                table.push((v.coid.into(), v.name.into(), verdict, detail));
            }
            // Sweep: cancel by id whatever this client placed (an already-gone order answers a harmless notice).
            // Never `reqGlobalCancel`: it would cancel every client's orders, the daemon's included.
            for order_id in placed {
                let _ = client.cancel_order(order_id, "");
            }
            std::thread::sleep(Duration::from_millis(500));
            client.disconnect();
        }
    }

    let (verdict, detail) = through_the_bridge(cfg.clone(), bridge_id);
    table.push((
        "b9ibs8".into(),
        "the bridge, extended_hours on, a GTC request".into(),
        verdict,
        detail,
    ));

    eprintln!("VARIANT TABLE (AAPL.SMART.USD, buy 1 LMT @{PRICE}, IB paper gateway {addr})");
    for (coid, name, verdict, detail) in &table {
        eprintln!("  {coid}  {verdict:<9}  {name}\n      {detail}");
    }
    let answered = table.iter().filter(|(_, _, v, _)| *v != "NO ANSWER").count();
    assert!(answered > 0, "not one order was answered: nothing was measured");
}
