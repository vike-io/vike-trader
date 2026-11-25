//! Deribit ICEBERG and MARKET-LIMIT LIVE TESTNET drill: the two kinds `VenueRestDeribit` builds
//! (`execution/rest.rs`'s `order_params_on`: `display_amount` from `display_qty`, `market_limit` from
//! `market_to_limit`), which its capability row declares since both tests here passed: this is the
//! evidence the rows rest on.
//!
//!     MSYS_NO_PATHCONV=1 LANE=<free lane> just the CI box <branch> env \
//!       VIKE_SETTINGS_DIR=/var/lib/vike/settings VIKE_NO_LIVE_KEYS=true \
//!       cargo test -p vike-deribit --test deribit_smokes -- --ignored --nocapture \
//!       deribit_iceberg_and_market_limit_testnet_smoke::
//!
//! Both PASSED on the testnet 2026-11-08 (store account `#6`): the iceberg on `BTC-PERPETUAL`, the
//! market-limit on the linear `ALT_USDC-PERPETUAL`. Run them by hand, never in a loop.
//!
//! ORDER-PLACING, TESTNET ONLY: every socket is `TESTNET_WEBSOCKET` and `only_testnet` refuses anything else.
//! Double-gated like every `*_smoke.rs`: network + the default DEMO account's keys through
//! `vike_secrets::smoke_directory`; absent credentials print `SKIP` and return. Every label starts with
//! [`PREFIX`] (never `cancel_all`); a `Drop` guard cancels our labels and flattens whatever position the
//! run added, so a failed assertion or a panic still ends flat. A run killed from outside (SIGKILL)
//! skips destructors: cancel `n0kd*` and close the position on testnet by hand. A reconcile pass of the
//! running node that lands while one of these rests may hold it as an `UnknownOrder`: read the node's
//! `recon ls` after the run.
//!
//! What it proves:
//! - `deribit_iceberg_on_the_testnet`: a FAR limit BUY of 2000 USD showing 1000 (the venue's floor is 100
//!   times the instrument's minimum of 10) on `BTC-PERPETUAL` is accepted, reads back by label with
//!   `display_amount` 1000, KEEPS it across a price edit through the client (the edit restates it), and
//!   the client's cancel ends it. What the public book shows of it is not read.
//! - `deribit_market_limit_on_the_testnet`: on the first BTC inverse future whose best ask is at most
//!   [`CAP`] US dollars, else the first LINEAR USDC perpetual ([`LINEAR_FIRST`] first) whose best ask is
//!   worth at most that and whose best bid could take the run's buys back (on 2026-11-08 the testnet
//!   parked about 1,000,000 USD at every BTC future's best ask), its private reads scoped by the
//!   instrument's settlement currency and its lot the instrument's minimum, (1) a `market_limit` BUY
//!   of one lot that the best ask covers FILLS whole and
//!   nothing rests; (2) one sized past the best ask (its amount plus a lot) fills that level and its
//!   REMAINDER rests `open` as a limit at the price the filled part executed at (the changelog's law,
//!   2021-05-14, and the plan's condition for opening the row), the node sees the partial fill and no
//!   `OrderFilled`, the restart reconcile lists the remainder under our order client id (its `order_type` is
//!   printed as `FINDING`: `market_limit` or `limit`, unmeasured), and the client's cancel ends it.

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
const PREFIX: &str = "n0kd";
/// The iceberg's instrument: INVERSE, the amount is US dollars, one lot (and the minimum) is 10.
const PERP: &str = "BTC-PERPETUAL";
const LOT: f64 = 10.0;
/// The largest best-ask level the market-limit drill will take, in US dollars (an inverse future's
/// amount): a deeper book would make the partial cost a position this large.
const CAP: f64 = 2_000.0;
const ATTEMPTS: usize = 5;
/// The linear perpetual the market-limit drill tries before the others (a liquid testnet book).
const LINEAR_FIRST: &str = "SOL_USDC-PERPETUAL";

/// The instrument a drill trades, as its `public/get_instruments` row states it.
#[derive(Clone)]
struct Instrument {
    name: String,
    /// The settlement currency, which scopes every private read (`BTC`, `USDC`).
    currency: String,
    /// The instrument's minimum amount, one lot.
    lot: f64,
    /// A linear contract's amount is the base coin; an inverse one's is US dollars.
    linear: bool,
}

impl Instrument {
    fn of(row: &Value) -> Option<Self> {
        Some(Self {
            name: row["instrument_name"].as_str()?.to_string(),
            currency: row["settlement_currency"].as_str()?.to_string(),
            lot: row["min_trade_amount"].as_f64()?,
            linear: row["instrument_type"] == "linear",
        })
    }

    /// What `amount` at `price` is worth in US dollars.
    fn usd(&self, price: f64, amount: f64) -> f64 {
        if self.linear { price * amount } else { amount }
    }
}

/// One authed testnet socket for the reads and the cleanup (the client under test owns its own).
struct Conn(TestnetConnection);

impl Conn {
    fn open(key: &str, secret: &str) -> Self {
        Self(TestnetConnection::open(key, secret).expect("testnet order-WS auth"))
    }

    fn array(&mut self, method: &str, params: &Value) -> Vec<Value> {
        let v = self.0.send(method, params).unwrap_or_else(|e| panic!("{method}: {}", e.msg));
        v.as_array().cloned().unwrap_or_default()
    }

    /// The net position in `instrument`, signed, in the order's unit: a linear row's `size` is the quote
    /// notional and its `size_currency` the base coin (`reconcile/mod.rs`'s `position_amount`).
    fn position(&mut self, instrument: &Instrument) -> f64 {
        let field = if instrument.linear { "size_currency" } else { "size" };
        self.array(
            "private/get_positions",
            &json!({"currency": instrument.currency, "kind": "future"}),
        )
        .iter()
        .find(|p| p["instrument_name"] == instrument.name.as_str())
        .and_then(|p| p[field].as_f64())
        .unwrap_or(0.0)
    }

    /// Every open order of the account whose label starts with [`PREFIX`].
    fn open_ours(&mut self) -> Vec<Value> {
        self.array("private/get_open_orders", &json!({}))
            .into_iter()
            .filter(|o| o["label"].as_str().is_some_and(|l| l.starts_with(PREFIX)))
            .collect()
    }

    /// The venue's row of the order under `label` (a label is unique to its run), printed.
    fn row(&mut self, currency: &str, label: &str) -> Value {
        let rows = self.array(
            "private/get_order_state_by_label",
            &json!({"currency": currency, "label": label}),
        );
        let row = rows.last().cloned().unwrap_or_else(|| panic!("no row under {label}"));
        println!("CAPTURED order row {label}: {row}");
        row
    }
}

/// Cancels our labels and returns `instrument`'s position to `start` with a plain market order.
struct Guard {
    key: String,
    secret: String,
    instrument: Instrument,
    start: f64,
}

impl Drop for Guard {
    fn drop(&mut self) {
        let Ok(c) = TestnetConnection::open(&self.key, &self.secret) else {
            println!("CLEANUP FAILED: no testnet socket; cancel `{PREFIX}*` and flatten by hand");
            return;
        };
        let mut c = Conn(c);
        for o in c.open_ours() {
            let label = o["label"].as_str().unwrap_or_default().to_string();
            let r = c.0.send("private/cancel_by_label", &json!({ "label": label }));
            println!("cleanup cancel_by_label {label}: {r:?}");
        }
        let delta = c.position(&self.instrument) - self.start;
        if delta.abs() > 1e-9 {
            let method = if delta > 0.0 { "private/sell" } else { "private/buy" };
            let amount = (delta.abs() * 1e9).round() / 1e9; // a coin amount's float noise off the wire
            let r = c.0.send(
                method,
                &json!({"instrument_name": self.instrument.name, "amount": amount, "type": "market",
                        "label": format!("{PREFIX}99")}),
            );
            println!("cleanup flatten {delta}: {:?}", r.map(|v| v["order"]["order_state"].clone()));
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

fn order(
    order_client_id: &str,
    instrument: &str,
    kind: OrderType,
    qty: f64,
    price: Option<f64>,
) -> OrderRequest {
    OrderRequest {
        order_client_id: order_client_id.to_string(),
        venue: "deribit".to_string(),
        symbol: instrument.to_string(),
        side: 1,
        qty,
        order_type: kind,
        price,
        timestamp: now_ms(),
        ..Default::default()
    }
}

/// The best level of `instrument`'s `side` (`asks` or `bids`) as `(price, amount)`, when the book has one.
fn best(instrument: &str, side: &str) -> Option<(f64, f64)> {
    let book = public(
        "/api/v2/public/get_order_book",
        &[("instrument_name", instrument.to_string()), ("depth", "1".to_string())],
    );
    let level = book[side].get(0)?;
    Some((level.get(0)?.as_f64()?, level.get(1)?.as_f64()?))
}

#[test]
#[ignore = "places TESTNET orders (a far resting iceberg); run by hand (see the module doc)"]
fn deribit_iceberg_on_the_testnet() {
    vike_log::test_init();
    only_testnet();
    let Some(creds) = load_demo_credentials() else { return };
    let (key, secret) = (creds.api_key.expose().to_string(), creds.api_secret.expose().to_string());
    let mut raw = Conn::open(&key, &secret);
    if !raw.open_ours().is_empty() {
        println!("SKIP: the testnet account holds `{PREFIX}*` orders");
        return;
    }
    let perp =
        Instrument { name: PERP.to_string(), currency: "BTC".to_string(), lot: LOT, linear: false };
    let start = raw.position(&perp);
    let _guard = Guard { key, secret, instrument: perp.clone(), start };

    let grid = fetch_deribit_properties(TESTNET_REST, PERP).expect("the instrument grid");
    let tick = grid.tick_size;
    let (tx, mut rx) = event_channel(256);
    let mut client =
        spawn_deribit_execution_client(testnet_urls(), creds, PERP.to_string(), tx, None);
    let mut next = move || rx.try_recv().ok();
    let mut seen = Vec::new();
    std::thread::sleep(Duration::from_secs(4)); // the pump's auth + subscribe settle

    let (ask, _) = best(PERP, "asks").expect("the perpetual has an ask");
    let far = ((ask * 0.5) / tick).round() * tick;
    let order_client_id = format!("{PREFIX}{:05}i", now_ms() % 100_000);
    let mut req = order(&order_client_id, PERP, OrderType::Limit, 200.0 * LOT, Some(far));
    req.display_quantity = Some(100.0 * LOT);
    client.submit(&req);
    assert!(
        wait_for(&mut next, &mut seen, Duration::from_secs(15), |e| matches!(
            e, Event::OrderAccepted(a) if a.order_client_id == order_client_id
        )),
        "{order_client_id} never accepted: {seen:?}"
    );
    let row = raw.row(&perp.currency, &order_client_id);
    assert_eq!(row["order_state"], "open");
    assert_eq!(row["order_type"], "limit");
    assert_eq!(row["amount"].as_f64(), Some(200.0 * LOT));
    assert_eq!(row["display_amount"].as_f64(), Some(100.0 * LOT), "the venue holds the slice");

    let moved = far - 10.0 * tick;
    client.modify(&req, None, Some(moved));
    assert!(
        wait_for(&mut next, &mut seen, Duration::from_secs(15), |e| matches!(
            e, Event::OrderModified(m) if m.order_client_id == order_client_id
        )),
        "{order_client_id} edit never answered: {seen:?}"
    );
    let row = raw.row(&perp.currency, &order_client_id);
    assert_eq!(row["price"].as_f64(), Some(moved));
    assert_eq!(row["display_amount"].as_f64(), Some(100.0 * LOT), "the edit kept the slice");

    client.cancel(&order_client_id);
    assert!(
        wait_for(&mut next, &mut seen, Duration::from_secs(15), |e| matches!(
            e, Event::OrderCanceled(c) if c.order_client_id == order_client_id
        )),
        "{order_client_id} cancel not evented: {seen:?}"
    );
    assert!(raw.open_ours().is_empty(), "nothing of ours rests");
    client.detach();
    println!("END: the iceberg rested with its slice, kept it across an edit, and was cancelled");
}

#[test]
#[ignore = "places TESTNET orders that FILL (opens and closes a small position); run by hand"]
fn deribit_market_limit_on_the_testnet() {
    vike_log::test_init();
    only_testnet();
    let Some(creds) = load_demo_credentials() else { return };
    let (key, secret) = (creds.api_key.expose().to_string(), creds.api_secret.expose().to_string());
    let mut raw = Conn::open(&key, &secret);
    if !raw.open_ours().is_empty() {
        println!("SKIP: the testnet account holds `{PREFIX}*` orders");
        return;
    }
    // The first instrument whose best ask is a level this drill can take whole (the partial below
    // needs to consume it) and whose best bid takes the run's buys back: a BTC inverse future (the
    // perpetual included), else a linear USDC perpetual, `LINEAR_FIRST` first.
    let rows = |currency: &str| -> Vec<Value> {
        let r = public(
            "/api/v2/public/get_instruments",
            &[("currency", currency.to_string()), ("kind", "future".to_string())],
        );
        r.as_array().cloned().unwrap_or_default()
    };
    let inverse = rows("BTC")
        .into_iter()
        .filter(|row| row["instrument_type"] == "reversed" && row["contract_size"] == LOT);
    let mut linear: Vec<Value> = rows("USDC")
        .into_iter()
        .filter(|row| row["instrument_type"] == "linear" && row["settlement_period"] == "perpetual")
        .collect();
    linear.sort_by_key(|row| row["instrument_name"] != LINEAR_FIRST);
    let fits = |i: &Instrument| {
        best(&i.name, "asks").is_some_and(|(price, amount)| {
            amount >= i.lot
                && i.usd(price, amount) <= CAP
                && best(&i.name, "bids").is_some_and(|(_, bid)| bid >= amount + 2.0 * i.lot)
        })
    };
    let Some(instrument) = inverse.chain(linear).filter_map(|row| Instrument::of(&row)).find(fits)
    else {
        println!(
            "SKIP: no BTC inverse future and no linear USDC perpetual has a best ask worth at most \
             {CAP} USD with a bid to sell it back: nothing measured"
        );
        return;
    };
    let model = if instrument.linear { "linear" } else { "inverse" };
    println!(
        "instrument: {} ({model}, settles in {}, lot {})",
        instrument.name, instrument.currency, instrument.lot
    );
    let start = raw.position(&instrument);
    let _guard = Guard { key, secret, instrument: instrument.clone(), start };

    fetch_deribit_properties(TESTNET_REST, &instrument.name).expect("the instrument grid");
    let recon = ReconcileClientDeribit::connect(
        &testnet_urls(),
        &load_demo_credentials().expect("creds"),
        &instrument.name,
    )
    .expect("the recon client's own authed socket");
    let (tx, mut rx) = event_channel(256);
    let mut client =
        spawn_deribit_execution_client(testnet_urls(), creds, instrument.name.clone(), tx, None);
    let mut next = move || rx.try_recv().ok();
    let mut seen = Vec::new();
    std::thread::sleep(Duration::from_secs(4)); // the pump's auth + subscribe settle
    let run = format!("{PREFIX}{:05}", now_ms() % 100_000);

    // 1. one lot the best ask covers: the whole order fills, as a market order would, and nothing rests
    println!("== 1. a market_limit the best ask covers");
    let whole = format!("{run}w");
    client.submit(&order(&whole, &instrument.name, OrderType::MarketToLimit, instrument.lot, None));
    assert!(
        wait_for(&mut next, &mut seen, Duration::from_secs(30), |e| matches!(
            e, Event::OrderFilled(f) if f.order_client_id == whole
        )),
        "{whole} never filled: {seen:?}"
    );
    std::thread::sleep(Duration::from_secs(1));
    assert!(raw.open_ours().is_empty(), "a whole fill leaves nothing resting");

    // 2. past the best ask: the level fills and the remainder rests as a limit at the fill's price
    println!("== 2. a market_limit past the best ask");
    let mut measured = false;
    for attempt in 1..=ATTEMPTS {
        let Some((ask, level)) =
            best(&instrument.name, "asks").filter(|&(p, a)| instrument.usd(p, a) <= CAP)
        else {
            println!(
                "  attempt {attempt}: the best ask is gone or worth over {CAP} USD; re-reading"
            );
            std::thread::sleep(Duration::from_secs(5));
            continue;
        };
        let order_client_id = format!("{run}p{attempt}");
        let qty = level + instrument.lot;
        println!("  attempt {attempt}: best ask {level} at {ask}; buying {qty}");
        client.submit(&order(
            &order_client_id,
            &instrument.name,
            OrderType::MarketToLimit,
            qty,
            None,
        ));
        let accepted = wait_for(
            &mut next,
            &mut seen,
            Duration::from_secs(15),
            |e| matches!(e, Event::OrderAccepted(a) if a.order_client_id == order_client_id),
        );
        assert!(accepted, "{order_client_id} never accepted: {seen:?}");
        // Drain the lane up to the order's first fill: a bare sleep left it unread in the channel.
        wait_for(
            &mut next,
            &mut seen,
            Duration::from_secs(10),
            |e| matches!(e, Event::Fill(f) if f.order_client_id == order_client_id),
        );
        let row = raw.row(&instrument.currency, &order_client_id);
        if row["order_state"] == "filled" {
            println!(
                "  {order_client_id}: the level grew before the order landed and it filled whole; again"
            );
            continue;
        }
        let filled = row["filled_amount"].as_f64().unwrap_or(0.0);
        let first_fill = seen.iter().find_map(|e| match e {
            Event::Fill(f) if f.order_client_id == order_client_id => Some(f.last_price),
            _ => None,
        });
        println!("  {order_client_id}: filled {filled} of {qty}, first fill at {first_fill:?}");
        assert_eq!(row["order_state"], "open", "the remainder RESTS: {row}");
        assert!(filled > 0.0 && filled < qty, "a partial fill: {row}");
        assert_eq!(
            row["price"].as_f64(),
            first_fill,
            "the remainder rests at the price its filled part executed at: {row}"
        );
        assert!(
            !seen.iter().any(
                |e| matches!(e, Event::OrderFilled(f) if f.order_client_id == order_client_id)
            ),
            "the node must not read the order as filled: {seen:?}"
        );
        let reports = recon.fetch_order_status_reports(0).expect("the recon read");
        let report = reports
            .iter()
            .find(|r| r.order_client_id.as_deref() == Some(order_client_id.as_str()))
            .unwrap_or_else(|| {
                panic!("the restart reconcile must list the remainder: {reports:?}")
            });
        println!(
            "FINDING: a resting market_limit remainder reads order_type {:?} (venue word {}), filled {} of {}",
            report.order_type, row["order_type"], report.filled_qty, report.qty
        );
        client.cancel(&order_client_id);
        assert!(
            wait_for(&mut next, &mut seen, Duration::from_secs(15), |e| matches!(
                e, Event::OrderCanceled(c) if c.order_client_id == order_client_id
            )),
            "{order_client_id} cancel not evented: {seen:?}"
        );
        measured = true;
        break;
    }
    assert!(
        measured,
        "no market_limit landed partially in {ATTEMPTS} attempts: step 2 not measured"
    );
    assert!(raw.open_ours().is_empty(), "nothing of ours rests");
    client.detach();
    println!(
        "END: the remainder rested at its first fill's price; the guard flattens the position"
    );
}
