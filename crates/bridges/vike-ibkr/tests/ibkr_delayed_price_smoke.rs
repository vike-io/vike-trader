//! LIVE PAPER smoke - **a stock the node was never asked about is looked up, PRICED OFF DELAYED DATA and traded one
//! share each way** (decision 0140: an order MAY be priced off delayed data; `docs/ops/ibkr-socket-gateway.md`'s
//! "Delayed data prices a new stock"). It runs the production pieces of the any-symbol path against the paper
//! gateway and PRINTS what each one saw, one line per event:
//!
//! | step | production piece | what it shows |
//! |---|---|---|
//! | FACTS | `IbkrFactsSource::facts` (symbol search, contract details, market rules) | the stock is admitted: its conId, grid and minimums |
//! | PRICE | `LazyQuotes::for_config` (the gatewayhub's price stream), over a recording sink | the label IBKR puts on the stream and the two-sided quotes, with the seconds; the tap in front of the core (`vike-tradehub`'s `PriceTap::labelled_quotes`) prices exactly a labelled two-sided quote |
//! | ORDERS | `IbkrExecutionClient`, the production bridge, `extended_hours` as the settings database has it | a marketable 1-share limit BUY priced off the delayed ask, then the SELL back; the events that reach the host's lane |
//! | END | an observer client of its own | no order of this run rests, the stock's position is what it was |
//!
//! The stock is [`TICKER`] (a liquid US stock the node has not admitted, never the engine's mounted `AAPL`: the node's
//! reconcile reads that one and would see a drift). Client ids come from the UTC day in 30000-39999 (the daemon's are
//! small and configured; `ibkr_untaught_replay_smoke` uses 20000-27999), so a node that is running is never touched:
//! this run is one more API client of the paper gateway, three ids of its own.
//!
//! ⚠ It PLACES REAL PAPER ORDERS that FILL (buys 1 share, sells it back). Never run it casually; the lane, the
//! credential store and the fence are `docs/ops/live-smoke-lane.md`'s, and no workflow runs it. The gateway is on
//! the CI box (the settings' host is its loopback):
//!
//!     just the CI box <branch> env VIKE_SETTINGS_DIR=/var/lib/vike/settings VIKE_NO_LIVE_KEYS=true \
//!       cargo test -p vike-ibkr --features ibkr,test-support --test ibkr_delayed_price_smoke -- --ignored --nocapture
//!
//! `ibkr_delayed_price_observe_only` is the read-only half alone (the lookup and the price stream, about a minute):
//! name it instead of the whole file's tests, or run it first.
//!
//! Triple-gated like the other live tests: `#[ignore]`; self-skips unless a demo ibkr account holding an `ACCOUNT`, on
//! the `socket` backend, exists and the gateway answers; and a hard assert refuses anything but a paper `DU...`
//! account.
#![cfg(feature = "ibkr")]

mod common;
use common::ibkr_settings;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ibapi::client::blocking::Client;
use ibapi::orders::Orders;
use ibapi::subscriptions::SubscriptionItem;
use tokio::sync::mpsc::Receiver;
use tokio::sync::mpsc::error::TryRecvError;
use vike_data::{DataClient, LiveDataSink};
use vike_exec::{EventSender, ExecutionClient, Ingest, event_channel};
use vike_ibkr::IbkrExecutionClient;
use vike_ibkr::any_symbol_socket::IbkrFactsSource;
use vike_ibkr::config::{IbkrConfig, load_ibkr_config_from};
use vike_ibkr::quote_handle::LazyQuotes;
use vike_marketdata::{Bar, L2Book, QuoteTick, TradeTick};
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::events::Event;
use vike_model::{AccountTier, DataDelay, OrderRequest, StreamStatus};
use vike_venues::IbkrBackend;

/// The stock the smoke looks up, prices and trades one share of.
const TICKER: &str = "MSFT";
const SYMBOL: &str = "MSFT.SMART.USD";

/// What the price stream's sink saw, in order, with the seconds since the run began.
#[derive(Default)]
struct Seen {
    lines: Mutex<Vec<String>>,
    /// The last label, and the last two-sided quote with the label it arrived under.
    label: Mutex<Option<DataDelay>>,
    last: Mutex<Option<(f64, f64, Option<DataDelay>)>>,
    quotes: Mutex<usize>,
    t0: Option<Instant>,
}

impl Seen {
    fn say(&self, what: String) {
        let at = self.t0.map_or(0.0, |t| t.elapsed().as_secs_f64());
        eprintln!("[PRICE +{at:>6.1}s] {what}");
        self.lines.lock().unwrap().push(what);
    }
}

impl LiveDataSink for Seen {
    fn seed_bars(&self, _v: &str, _s: &str, _i: &str, _b: Vec<Bar>) {}
    fn close_bar(&self, _v: &str, _s: &str, _i: &str, _b: Bar) {}
    fn forming_bar(&self, _v: &str, _s: &str, _i: &str, _b: Bar) {}
    fn mark_tick(&self, _v: &str, _s: &str, _p: f64, _t: i64) {}
    fn quote(&self, _v: &str, _s: &str, q: QuoteTick) {
        let label = *self.label.lock().unwrap();
        let two_sided = q.bid.is_finite() && q.ask.is_finite() && q.bid > 0.0 && q.ask >= q.bid;
        self.say(format!(
            "quote bid={} ask={} bid_size={} ask_size={} label={} two_sided={two_sided}",
            q.bid,
            q.ask,
            q.bid_size,
            q.ask_size,
            label.map_or("none", DataDelay::word)
        ));
        if two_sided {
            *self.last.lock().unwrap() = Some((q.bid, q.ask, label));
        }
        *self.quotes.lock().unwrap() += 1;
    }
    fn trade(&self, _v: &str, _s: &str, _t: TradeTick) {}
    fn book(&self, _v: &str, _s: &str, _b: Arc<L2Book>) {}
    fn stream_status(&self, _v: &str, _s: &str, stream: &str, status: StreamStatus) {
        self.say(format!("stream_status {stream} {status:?}"));
    }
    fn data_delay(&self, _v: &str, _s: &str, stream: &str, delay: DataDelay) {
        *self.label.lock().unwrap() = Some(delay);
        self.say(format!("data_delay {stream} = {} ({delay:?})", delay.word()));
    }
}

/// One of the bridge session's events as a short line; credentials never appear in an event.
fn describe(ev: &Event) -> String {
    match ev {
        Event::OrderSubmitted(e) => format!("OrderSubmitted {}", e.client_order_id),
        Event::OrderAccepted(e) => format!("OrderAccepted {}", e.client_order_id),
        Event::OrderRejected(e) => format!("OrderRejected {} ({})", e.client_order_id, e.reason),
        Event::OrderCanceled(e) => format!("OrderCanceled {}", e.client_order_id),
        Event::OrderFilled(e) => format!(
            "OrderFilled {} {}@{} symbol={}",
            e.client_order_id, e.fill.last_qty, e.fill.last_px, e.fill.symbol
        ),
        Event::Fill(f) => {
            format!("Fill {} {}@{} symbol={}", f.client_order_id, f.last_qty, f.last_px, f.symbol)
        }
        other => {
            let mut s = format!("{other:?}");
            s.truncate(160);
            s
        }
    }
}

/// The production bridge and everything its lane carried.
struct Session {
    client: IbkrExecutionClient,
    rx: Receiver<Ingest>,
    t0: Instant,
    lines: Vec<String>,
}

impl Session {
    /// Connect as `cfg.client_id` (retrying: an id the Gateway was only just told to release answers an error for a
    /// moment). Nothing is taught: the order placed here is this connection's own.
    fn open(cfg: &IbkrConfig) -> Result<Session, String> {
        let mut last = String::new();
        for _ in 0..8 {
            let (events, rx): (EventSender, Receiver<Ingest>) = event_channel(1024);
            match IbkrExecutionClient::connect(cfg, events) {
                Ok(mut client) => {
                    client.adopt_restored(&[]);
                    eprintln!("[ORDERS] connected as client id {}", cfg.client_id);
                    return Ok(Session { client, rx, t0: Instant::now(), lines: Vec::new() });
                }
                Err(e) => last = e.to_string(),
            }
            std::thread::sleep(Duration::from_secs(3));
        }
        Err(last)
    }

    fn say(&mut self, line: String) {
        eprintln!("[ORDERS +{:>6.1}s] {line}", self.t0.elapsed().as_secs_f64());
        self.lines.push(line);
    }

    /// Read the lane for `window`, or until a line satisfies `done`.
    fn pump(&mut self, window: Duration, done: impl Fn(&str) -> bool) -> bool {
        let deadline = Instant::now() + window;
        while Instant::now() < deadline {
            let line = match self.rx.try_recv() {
                Ok(Ingest::Event(ev)) => describe(&ev),
                Ok(Ingest::ExecLink(u)) => format!("ExecLink up={}", u.up),
                Ok(_) => continue,
                Err(TryRecvError::Empty) => {
                    std::thread::sleep(Duration::from_millis(40));
                    continue;
                }
                Err(TryRecvError::Disconnected) => return false,
            };
            let hit = done(&line);
            self.say(line);
            if hit {
                return true;
            }
        }
        false
    }

    fn submit(&mut self, symbol: &str, coid: &str, side: i32, price: f64) {
        self.say(format!(
            "submit {coid} {} 1 {symbol} limit @{price}",
            if side > 0 { "BUY" } else { "SELL" }
        ));
        self.client.submit(&OrderRequest {
            client_order_id: coid.to_string(),
            venue: "ibkr".into(),
            symbol: symbol.into(),
            side,
            qty: 1.0,
            order_type: vike_model::OrderType::Limit,
            price: Some(price),
            ..Default::default()
        });
    }

    fn filled(&mut self, coid: &str, window: Duration) -> bool {
        self.pump(window, |l| l.starts_with(&format!("OrderFilled {coid}")))
    }

    fn about(&self, coid: &str) -> Vec<&str> {
        self.lines.iter().filter(|l| l.contains(coid)).map(String::as_str).collect()
    }
}

/// What the observer client (its own id) sees: this run's open orders (by `orderRef` prefix) and the stock's position.
fn observe(
    addr: &str,
    id: i32,
    account: &str,
    ticker: &str,
    prefix: &str,
) -> Result<(Vec<String>, f64), String> {
    let client = Client::connect(addr, id).map_err(|e| format!("observer connect: {e}"))?;
    let mut open = Vec::new();
    let sub = client.all_open_orders().map_err(|e| format!("open orders: {e}"))?;
    let end = Instant::now() + Duration::from_secs(10);
    while Instant::now() < end {
        match sub.next_timeout(Duration::from_millis(500)) {
            Some(Ok(SubscriptionItem::Data(Orders::OrderData(o)))) => {
                if o.order.order_ref.starts_with(prefix) {
                    open.push(format!(
                        "{} id={} status={}",
                        o.order.order_ref,
                        o.order_id,
                        o.order_state.status.as_str()
                    ));
                }
            }
            Some(Ok(_)) => {}
            Some(Err(e)) => return Err(format!("open orders: {e}")),
            None => break,
        }
    }
    let mut position = 0.0;
    let sub = client.positions().map_err(|e| format!("positions: {e}"))?;
    let end = Instant::now() + Duration::from_secs(10);
    while Instant::now() < end {
        match sub.next_timeout(Duration::from_millis(500)) {
            Some(Ok(SubscriptionItem::Data(ibapi::accounts::PositionUpdate::Position(p)))) => {
                if p.account == account && p.contract.symbol == ticker {
                    position += p.position;
                }
            }
            Some(Ok(SubscriptionItem::Data(ibapi::accounts::PositionUpdate::PositionEnd))) => break,
            Some(Ok(_)) => {}
            Some(Err(e)) => return Err(format!("positions: {e}")),
            None => break,
        }
    }
    client.disconnect();
    Ok((open, position))
}

fn round_to(x: f64, tick: f64) -> f64 {
    let t = if tick.is_finite() && tick > 0.0 { tick } else { 0.01 };
    ((x / t).round() * t * 1e6).round() / 1e6
}

fn verdict(ok: bool) -> &'static str {
    if ok { "PASS" } else { "FAIL" }
}

/// Three client ids from the UTC day, 30000-39999: the bridge's, the data connection's (the lookup and the price
/// stream share it, as in production) and the observer's.
fn todays_ids() -> [i32; 3] {
    let days = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs()) / 86_400;
    let base = 30_000 + (days % 1_000) as i32 * 8;
    [base, base + 1, base + 2]
}

/// The demo ibkr account's config for a live run and the gateway address, or `None` (after saying why it skips).
fn live_cfg() -> Option<(IbkrConfig, String)> {
    let settings_dir = std::env::var("VIKE_SETTINGS_DIR").ok();
    let dir = vike_secrets::smoke_directory();
    let creds = dir.credentials_for("ibkr", &AccountLabel::Default, AccountTier::Demo);
    let Some(mut cfg) =
        load_ibkr_config_from(AccountTier::Demo, &creds, &ibkr_settings(settings_dir.as_deref()))
    else {
        eprintln!("skip: no demo ibkr account holding an `ACCOUNT` in the settings database");
        return None;
    };
    if cfg.backend == IbkrBackend::Cpapi {
        eprintln!(
            "skip: venue.ibkr.demo.backend is cpapi; this smoke speaks the IB Gateway socket"
        );
        return None;
    }
    // HARD SAFETY: paper only - this assert stays a hard failure, never a skip.
    assert!(
        cfg.account.starts_with("DU"),
        "smoke must target a paper (DU\u{2026}) account, got {}",
        cfg.account
    );
    let [cid, data, _] = todays_ids();
    (cfg.client_id, cfg.data_client_id) = (cid, data);
    let addr = format!("{}:{}", cfg.host, cfg.port);
    Some((cfg, addr))
}

/// What the read-only half found: the stock's tick and the last two-sided quote with the label it arrived under.
struct Priced {
    tick: f64,
    bid: f64,
    ask: f64,
}

/// FACTS and PRICE: the production lookup of a stock nobody asked about, then the gatewayhub's price stream over a sink
/// that records what the tap would be handed. Places nothing. `None` when no two-sided quote arrived.
fn look_and_price(
    cfg: &IbkrConfig,
    symbol: &str,
    results: &mut Vec<(String, bool, String)>,
) -> Option<Priced> {
    let run = Instant::now();

    // ---- FACTS: the production lookup of a stock nobody asked about ----
    let facts = IbkrFactsSource::new(cfg.clone());
    let asked = Instant::now();
    let found = facts.facts(symbol, "USD");
    eprintln!("[FACTS +{:>5.1}s] {found:?}", asked.elapsed().as_secs_f64());
    let tick = found.as_ref().map_or(0.01, |f| f.properties.tick_size);
    results.push((
        "FACTS the stock is admitted by name".into(),
        found.is_ok(),
        format!("{found:?}"),
    ));

    // ---- PRICE: the gatewayhub's price stream, over a sink that records what the tap would be handed ----
    let seen = Arc::new(Seen { t0: Some(run), ..Seen::default() });
    let mut stream =
        LazyQuotes::for_config(cfg.clone(), Arc::clone(&seen) as Arc<dyn LiveDataSink>);
    let id = stream.subscribe_quotes(symbol);
    eprintln!("[PRICE] subscribe_quotes({symbol}) -> {id:?}");
    let deadline = Instant::now() + Duration::from_secs(75);
    while seen.last.lock().unwrap().is_none() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(200));
    }
    // Keep listening so the cadence of the delayed feed shows (a quiet stock's price ages out of the tap at 60 s).
    let watch = Instant::now() + Duration::from_secs(60);
    while Instant::now() < watch {
        std::thread::sleep(Duration::from_millis(500));
    }
    let (n, label, last) =
        (*seen.quotes.lock().unwrap(), *seen.label.lock().unwrap(), *seen.last.lock().unwrap());
    let first_label_before_quote = {
        let lines = seen.lines.lock().unwrap();
        let l = lines.iter().position(|l| l.starts_with("data_delay"));
        let q = lines.iter().position(|l| l.starts_with("quote"));
        matches!((l, q), (Some(l), Some(q)) if l < q)
    };
    if let Ok(id) = id {
        stream.unsubscribe(id);
    }
    stream.shutdown();
    results.push((
        "PRICE the stream states a label BEFORE its first quote, and a two-sided quote arrives"
            .into(),
        label.is_some() && last.is_some() && first_label_before_quote,
        format!("{n} quotes in the run; label {label:?}; last two-sided {last:?}"),
    ));
    let Some((bid, ask, quote_label)) = last else {
        eprintln!("NOT PRODUCED: no two-sided quote of {symbol}; nothing is placed");
        return None;
    };
    eprintln!(
        "TAP would price {symbol} at mid {} under label {} (a labelled two-sided quote); real-time is preferred when the venue serves it",
        (bid + ask) / 2.0,
        quote_label.map_or("none", DataDelay::word)
    );
    Some(Priced { tick, bid, ask })
}

/// The read-only half alone: the lookup and the price stream of a stock, placing nothing.
#[test]
#[ignore = "live paper: looks up a stock and streams its quotes for about a minute; places nothing"]
fn ibkr_delayed_price_observe_only() {
    vike_log::test_init();
    let Some((cfg, addr)) = live_cfg() else { return };
    eprintln!("gateway {addr}, paper account DU\u{2026}, stock {SYMBOL}; nothing is placed");
    let mut results = Vec::new();
    let priced = look_and_price(&cfg, SYMBOL, &mut results);
    print_results(&results);
    assert!(priced.is_some() && results.iter().all(|(_, ok, _)| *ok), "see RESULTS above");
}

#[test]
#[ignore = "live paper: looks up a stock, streams its quotes and buys then sells back 1 share on a DU... account"]
fn ibkr_delayed_price_smoke() {
    vike_log::test_init();
    let Some((cfg, addr)) = live_cfg() else { return };
    let ticker = TICKER;
    let [cid, data_id, obs_id] = todays_ids();
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs()) % 100_000;
    let (buy, sell) = (format!("vdlp{stamp}buy"), format!("vdlp{stamp}sel"));
    let prefix = format!("vdlp{stamp}");
    eprintln!(
        "gateway {addr}, paper account DU\u{2026}, extended_hours={}, mktdata_type={:?}, client ids bridge {cid} data {data_id} observer {obs_id}; \
         stock {SYMBOL}; coids {buy} {sell}",
        cfg.extended_hours, cfg.mktdata_type
    );
    let mut results: Vec<(String, bool, String)> = Vec::new();
    let Some(Priced { tick, bid, ask }) = look_and_price(&cfg, SYMBOL, &mut results) else {
        print_results(&results);
        return;
    };
    let symbol = SYMBOL;

    // ---- ORDERS: the production bridge, a marketable limit each way priced off that delayed quote ----
    let (buy_px, sell_px) = (round_to(ask * 1.01, tick), round_to(bid * 0.99, tick));
    let (before_open, position_before) = observe(&addr, obs_id, &cfg.account, ticker, &prefix)
        .expect("the observer reads the account");
    eprintln!(
        "BEFORE: {ticker} position {position_before}; open orders of this run: {before_open:?}"
    );
    let mut s = Session::open(&cfg).expect("the bridge connects");
    let up = s.pump(Duration::from_secs(60), |l| l == "ExecLink up=true");
    s.pump(Duration::from_secs(2), |_| false);
    results.push(("ORDERS the bridge learned the venue".into(), up, String::new()));
    s.submit(symbol, &buy, 1, buy_px);
    let bought = s.filled(&buy, Duration::from_secs(90));
    results.push((
        "ORDERS the marketable buy, priced off the delayed ask, fills".into(),
        bought,
        format!("{:?}", s.about(&buy)),
    ));
    if bought {
        s.submit(symbol, &sell, -1, sell_px);
        let sold = s.filled(&sell, Duration::from_secs(90));
        results.push(("ORDERS the sell-back fills".into(), sold, format!("{:?}", s.about(&sell))));
        if !sold {
            s.client.cancel(&sell);
            s.pump(Duration::from_secs(10), |_| false);
            eprintln!(
                "LEFT BEHIND: {ticker} may be +1 on the paper account (the sell {sell} did not fill and was cancelled): sell it by hand"
            );
        }
    } else {
        s.client.cancel(&buy);
        s.pump(Duration::from_secs(10), |_| false);
    }
    s.client.detach();
    std::thread::sleep(Duration::from_secs(3));

    // ---- END ----
    let (open_after, position_after) = observe(&addr, obs_id, &cfg.account, ticker, &prefix)
        .expect("the observer reads the account");
    eprintln!(
        "AFTER: {ticker} position {position_after} (before {position_before}); open orders of this run: {open_after:?}"
    );
    results.push((
        "END the position is what it was and no order of this run rests".into(),
        (position_after - position_before).abs() < 1e-9 && open_after.is_empty(),
        format!("position {position_before} -> {position_after}; open {open_after:?}"),
    ));
    print_results(&results);
    assert!(results.iter().all(|(_, ok, _)| *ok), "a step did not hold: see RESULTS above");
}

fn print_results(results: &[(String, bool, String)]) {
    eprintln!("RESULTS");
    for (what, ok, detail) in results {
        eprintln!("  {}  {what}\n        {detail}", verdict(*ok));
    }
}
