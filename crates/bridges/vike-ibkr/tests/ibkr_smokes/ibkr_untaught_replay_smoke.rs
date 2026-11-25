//! LIVE PAPER smoke - what a FRESHLY BUILT bridge emits for orders its host never taught it. Places REAL orders on the
//! PAPER account: it BUYS 1 AAPL and SELLS it back (the account ends where it began), and rests one far limit order
//! that it cancels itself. Everything is observed through `connect_execution_client`, the production bridge, over the IB
//! Gateway socket: the events that reach the host's lane are printed one per line, with the session and the seconds.
//!
//! The defect behind it (measured 2026-10-26, v0.1.90): after every rebuild IB replays the whole day's executions of
//! our client id, and the bridge re-emitted `[Fill, OrderPartiallyFilled]` for each of them under IB's echo of the
//! instrument (`AAPL.IBEOS.USD` overnight), whether the order had FINISHED or was still OPEN. The fix: an order the
//! host never taught the symbol of emits NO fill (it is tracked, announced and cancellable; the reconcile reports what
//! the books never saw), and one the host DID teach is unchanged.
//!
//! One client id (`CID`, from the clock, never one the daemon dials) is used for every bridge session, so each later
//! session is a REBUILD of the same client: IB replays this run's executions to it. A second id (`CID + 1`) is the
//! observer: a quote, the positions, the open orders. Sessions, in order:
//!
//! | session | taught | what it shows |
//! |---|---|---|
//! | A | nothing placed by the host | places `vsmk<N>buy` (a marketable limit BUY 1, fills) and `vsmk<N>rst` (a limit 20 percent below, rests), then is torn down WITHOUT cancelling |
//! | B (S1+S2) | nothing | the rebuild: the FINISHED buy and the OPEN rest are replayed to a bridge nobody taught: NO fill/partial/terminal event for the buy, NO fill for the rest; it is announced accepted, and this session cancels its own order |
//! | C (S3) | the buy's symbol | the same replay with the symbol taught: exactly ONE `Fill` + one `OrderPartiallyFilled` for the buy, under the taught symbol |
//! | D (S4) | nothing | places `vsmk<N>sel` (a marketable limit SELL 1) and fills it: the account is flat of this run |
//! | E | nothing | the end state: no order of this run rests, AAPL position as before |
//!
//! The quote is the DELAYED ask (else last, else close); with no price at all the smoke stops before it places anything.
//! The client ids come from the UTC day, so a second test, `ibkr_untaught_replay_observe_only`, run later the same day
//! with another build of the bridge, replays the same executions to an untaught session and prints what that build
//! emits for them (the contrast run; it places nothing). Add `ibkr_untaught_replay_observe_only` to the command below.
//!
//! ⚠ It PLACES REAL PAPER ORDERS. Never run it casually; the lane, the credential store and the fence are
//! `docs/ops/live-smoke-lane.md`'s, and no workflow runs it. A reconcile pass of the node that lands while its order
//! rests, or after its fills, sees activity of a client id it does not know. On a the CI box lane (the gateway is on the box):
//!
//!     just the CI box <branch> env VIKE_SETTINGS_DIR=/var/lib/vike/settings VIKE_NO_LIVE_KEYS=true \
//!       cargo test -p vike-ibkr --features ibkr,test-support --test ibkr_smokes -- --ignored --nocapture ibkr_untaught_replay_smoke::
//!
//! Triple-gated like the other live tests: `#[ignore]`; self-skips unless a demo ibkr account holding an `ACCOUNT`, on
//! the `socket` backend, exists and the gateway answers; and a hard assert refuses anything but a paper `DU...` account.
#![cfg(feature = "ibkr")]

use crate::common::ibkr_settings;

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::common::{aapl, observe};
use ibapi::client::blocking::Client;
use ibapi::contracts::tick_types::TickType;
use ibapi::market_data::MarketDataType;
use ibapi::market_data::realtime::TickTypes;
use tokio::sync::mpsc::Receiver;
use tokio::sync::mpsc::error::TryRecvError;
use vike_bridge_core::exec_actor::ExecutionClientActor;
use vike_execution::{EventSender, ExecutionClient, Ingest, event_channel};
use vike_ibkr::config::{IbkrConfig, load_ibkr_config_from};
use vike_ibkr::connect_execution_client;
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::events::Event;
use vike_model::{AccountTier, OrderRequest};
use vike_venues::IbkrBackend;

/// The user name (decision 0290); the box's kept ibkr row (`AAPL.SMART.USD`) is what the bridge sends.
const SYMBOL: &str = "AAPL";

/// One of the session's events, as a short line a human can read; credentials never appear in an event.
fn describe(event: &Event) -> String {
    match event {
        Event::OrderAccepted(e) => format!("OrderAccepted {}", e.order_client_id),
        Event::OrderRejected(e) => format!("OrderRejected {} ({})", e.order_client_id, e.reason),
        Event::OrderCanceled(e) => format!("OrderCanceled {}", e.order_client_id),
        Event::OrderCancelRejected(e) => format!("OrderCancelRejected {}", e.order_client_id),
        Event::OrderPartiallyFilled(e) => format!(
            "OrderPartiallyFilled {} {}@{} symbol={} trade_id={}",
            e.order_client_id,
            e.fill.last_qty,
            e.fill.last_price,
            e.fill.symbol,
            e.fill.trade_id.as_str()
        ),
        Event::OrderFilled(e) => format!(
            "OrderFilled {} {}@{} symbol={} trade_id={}",
            e.order_client_id,
            e.fill.last_qty,
            e.fill.last_price,
            e.fill.symbol,
            e.fill.trade_id.as_str()
        ),
        Event::Fill(f) => format!(
            "Fill {} {}@{} symbol={} trade_id={}",
            f.order_client_id,
            f.last_qty,
            f.last_price,
            f.symbol,
            f.trade_id.as_str()
        ),
        other => {
            let mut s = format!("{other:?}");
            s.truncate(160);
            s
        }
    }
}

/// One bridge session: the production exec client (`connect_execution_client`) and everything its lane carried.
struct Session {
    name: &'static str,
    client: ExecutionClientActor,
    rx: Receiver<Ingest>,
    t0: Instant,
    /// Every line this session printed, for the verdicts.
    lines: Vec<String>,
}

impl Session {
    /// Connect as `config.client_id` (retrying: an id the Gateway was only just told to release answers an error for a
    /// moment) and make the host's handover with `taught` (the order-client-id-symbol pairs the host knows; empty = nothing).
    fn open(
        name: &'static str,
        config: &IbkrConfig,
        taught: &[vike_execution::RestoredOrder],
    ) -> Result<Session, String> {
        let mut last = String::new();
        for _ in 0..8 {
            let (events, rx): (EventSender, Receiver<Ingest>) = event_channel(1024);
            match connect_execution_client(config, events) {
                Ok((mut client, _)) => {
                    client.adopt_restored(taught);
                    eprintln!(
                        "[{name}] connected as client id {} (taught: {:?})",
                        config.client_id, taught
                    );
                    return Ok(Session { name, client, rx, t0: Instant::now(), lines: Vec::new() });
                }
                Err(e) => last = e.to_string(),
            }
            std::thread::sleep(Duration::from_secs(3));
        }
        Err(last)
    }

    fn say(&mut self, line: String) {
        eprintln!("[{} +{:>5.1}s] {line}", self.name, self.t0.elapsed().as_secs_f64());
        self.lines.push(line);
    }

    /// Read the lane for `window`, or until `done` says a line is the one waited for (checked per event).
    fn pump(&mut self, window: Duration, done: impl Fn(&str) -> bool) -> bool {
        let deadline = Instant::now() + window;
        while Instant::now() < deadline {
            match self.rx.try_recv() {
                Ok(Ingest::Event(event)) => {
                    let line = describe(&event);
                    let hit = done(&line);
                    self.say(line);
                    if hit {
                        return true;
                    }
                }
                Ok(Ingest::TradeLink(u)) => {
                    let line = format!("ExecLink up={}", u.up);
                    let hit = done(&line);
                    self.say(line);
                    if hit {
                        return true;
                    }
                }
                Ok(_) => {}
                Err(TryRecvError::Empty) => std::thread::sleep(Duration::from_millis(40)),
                Err(TryRecvError::Disconnected) => return false,
            }
        }
        false
    }

    /// The lane up to the link announcement (the learning, the replay and the rebind all precede it), then `settle`
    /// more of it. False when the link never came up.
    fn learn(&mut self, settle: Duration) -> bool {
        let up = self.pump(Duration::from_secs(60), |l| l == "ExecLink up=true");
        self.pump(settle, |_| false);
        up
    }

    fn submit(&mut self, order_client_id: &str, side: i32, price: f64) {
        self.say(format!(
            "submit {order_client_id} {} 1 {SYMBOL} limit @{price}",
            if side > 0 { "BUY" } else { "SELL" }
        ));
        self.client.submit(&OrderRequest {
            order_client_id: order_client_id.to_string(),
            venue: "ibkr".into(),
            symbol: SYMBOL.into(),
            side,
            qty: 1.0,
            order_type: vike_model::OrderType::Limit,
            price: Some(price),
            ..Default::default()
        });
    }

    /// Lines of this session naming `order_client_id`.
    fn about(&self, order_client_id: &str) -> Vec<&str> {
        self.lines.iter().filter(|l| l.contains(order_client_id)).map(String::as_str).collect()
    }

    fn close(mut self) {
        self.client.detach();
        // Leave IB time to release the client id before the next session asks for it.
        std::thread::sleep(Duration::from_secs(4));
    }
}

/// A reference price for AAPL: the delayed ask, else last, else close.
pub(crate) fn reference_price(addr: &str, id: i32) -> Result<(f64, String), String> {
    let client = Client::connect(addr, id).map_err(|e| format!("observer connect: {e}"))?;
    client
        .switch_market_data_type(MarketDataType::Delayed)
        .map_err(|e| format!("market data type: {e}"))?;
    let ticks = client
        .market_data(&aapl())
        .snapshot_once(Duration::from_secs(10))
        .map_err(|e| format!("snapshot: {e}"))?;
    let (mut ask, mut last, mut close) = (None, None, None);
    for t in &ticks {
        let (kind, price) = match t {
            TickTypes::Price(p) => (&p.tick_type, p.price),
            TickTypes::PriceSize(p) => (&p.price_tick_type, p.price),
            _ => continue,
        };
        if !(price.is_finite() && price > 0.0) {
            continue;
        }
        match kind {
            TickType::Ask | TickType::DelayedAsk => ask = Some(price),
            TickType::Last | TickType::DelayedLast => last = Some(price),
            TickType::Close | TickType::DelayedClose => close = Some(price),
            _ => {}
        }
    }
    client.disconnect();
    ask.map(|p| (p, "delayed ask".to_string()))
        .or(last.map(|p| (p, "delayed last".to_string())))
        .or(close.map(|p| (p, "delayed close".to_string())))
        .ok_or_else(|| format!("no AAPL price in {} ticks", ticks.len()))
}

pub(crate) fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

/// Two client ids derived from the UTC day (the daemon's ids are small and configured, these start at 20000): the bridge's
/// and the observer's. A run later the same day reuses them, so the observation-only test sees the day's executions of
/// the full smoke; a run on another day gets other ids, and IB replays only the current day's executions anyway.
pub(crate) fn todays_ids() -> [i32; 2] {
    let days = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs()) / 86_400;
    let base = crate::common::SMOKE_CLIENT_ID_BASE + 19_000 + (days % 2_000) as i32 * 4;
    [base, base + 1]
}

fn verdict(ok: bool) -> &'static str {
    if ok { "PASS" } else { "FAIL" }
}

/// The demo ibkr account's config for a live run and the gateway address, or `None` (after saying why it skips).
pub(crate) fn live_config() -> Option<(IbkrConfig, String)> {
    let settings_dir = std::env::var("VIKE_SETTINGS_DIR").ok();
    let dir = vike_secrets::smoke_directory();
    let creds = dir.credentials_for("ibkr", &AccountLabel::Default, AccountTier::Demo);
    let Some(mut config) =
        load_ibkr_config_from(AccountTier::Demo, &creds, &ibkr_settings(settings_dir.as_deref()))
    else {
        eprintln!("skip: no demo ibkr account holding an `ACCOUNT` in the settings database");
        return None;
    };
    if config.backend == IbkrBackend::Cpapi {
        eprintln!(
            "skip: venue.ibkr.demo.backend is cpapi; this smoke speaks the IB Gateway socket"
        );
        return None;
    }
    // HARD SAFETY: paper only - this assert stays a hard failure, never a skip.
    assert!(
        config.account.starts_with("DU"),
        "smoke must target a paper (DU\u{2026}) account, got {}",
        config.account
    );
    // The production path: `extended_hours` on, as the node's paper account runs it.
    config.extended_hours = true;
    let addr = format!("{}:{}", config.host, config.port);
    Some((config, addr))
}

/// The contrast run: ONLY an untaught observation session, as the client id the full smoke used today. Nothing is
/// placed. Run it after the full smoke, with another build of the bridge, to see what that build replays.
#[test]
#[ignore = "live paper: connects to the IB Gateway as a client id of its own and prints what a rebuilt bridge emits; places nothing"]
fn ibkr_untaught_replay_observe_only() {
    vike_log::test_init();
    let Some((mut config, addr)) = live_config() else { return };
    config.client_id = todays_ids()[0];
    eprintln!("OBSERVE ONLY: gateway {addr}, client id {}, nothing is placed", config.client_id);
    let mut s = Session::open("OBS", &config, &[]).expect("the observation session connects");
    let up = s.learn(Duration::from_secs(6));
    eprintln!("OBSERVE ONLY done: link up = {up}, {} lines", s.lines.len());
    s.close();
}

#[test]
#[ignore = "live paper: buys and sells back 1 AAPL and rests-and-cancels one far limit order on a DU... account"]
fn ibkr_untaught_replay_smoke() {
    vike_log::test_init();
    let Some((mut config, addr)) = live_config() else { return };

    let [cid, obs_id] = todays_ids();
    config.client_id = cid;
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs()) % 100_000;
    let (buy, rest, sell) =
        (format!("vsmk{stamp}buy"), format!("vsmk{stamp}rst"), format!("vsmk{stamp}sel"));
    let prefix = format!("vsmk{stamp}");
    eprintln!(
        "gateway {addr}, paper account DU\u{2026}, bridge client id {cid}, observer client id {obs_id}; order client ids {buy} {rest} {sell}"
    );

    let (reference, source) = match reference_price(&addr, obs_id) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("skip: no price to place a marketable order against: {e}");
            return;
        }
    };
    let (buy_price, rest_price, sell_price) =
        (round2(reference * 1.02), round2(reference * 0.80), round2(reference * 0.98));
    eprintln!(
        "reference price {reference} ({source}): buy limit {buy_price}, resting limit {rest_price}, sell limit {sell_price}"
    );
    let (before_open, position_before) = observe(&addr, obs_id, &config.account, "AAPL", &prefix)
        .expect("the observer reads the account");
    eprintln!("BEFORE: AAPL position {position_before}; open orders of this run: {before_open:?}");
    let mut results: Vec<(String, bool, String)> = Vec::new();

    // ---- session A: place the two orders, tear down WITHOUT cancelling ----
    let mut a = Session::open("A", &config, &[]).expect("session A connects");
    assert!(a.learn(Duration::from_secs(2)), "session A learned the venue");
    a.submit(&rest, 1, rest_price);
    a.submit(&buy, 1, buy_price);
    let filled = a.pump(Duration::from_secs(90), |l| l.starts_with(&format!("OrderFilled {buy}")));
    a.pump(Duration::from_secs(3), |_| false);
    let rest_accepted = a.about(&rest).iter().any(|l| l.starts_with("OrderAccepted"));
    eprintln!("A: buy filled = {filled}, resting order accepted = {rest_accepted}");
    if !filled {
        // Leave nothing behind: cancel both, say what happened, stop.
        a.client.cancel(&buy);
        a.client.cancel(&rest);
        a.pump(Duration::from_secs(10), |_| false);
        a.close();
        let (open, pos) =
            observe(&addr, obs_id, &config.account, "AAPL", &prefix).unwrap_or_default();
        eprintln!(
            "NOT PRODUCED: the marketable buy did not fill within 90 s (the market, the hour or the account refused it); \
             cancelled both; open orders of this run now {open:?}, AAPL position {pos}"
        );
        return;
    }
    a.close(); // the resting order is left at the venue on purpose

    // ---- session B: a rebuilt bridge nobody taught (S1 finished + S2 open) ----
    let left = format!(
        "LEFT BEHIND: order {rest} may still rest and AAPL may be +1 on the paper account: cancel it and sell by hand"
    );
    let mut b = Session::open("B", &config, &[])
        .unwrap_or_else(|e| panic!("session B could not connect ({e}); {left}"));
    let up = b.learn(Duration::from_secs(6));
    let s1_silent = !b.about(&buy).iter().any(|l| {
        l.starts_with("Fill")
            || l.starts_with("OrderPartiallyFilled")
            || l.starts_with("OrderFilled")
    }) && b.about(&buy).is_empty();
    let s2_rebound = b.about(&rest).iter().any(|l| l.starts_with("OrderAccepted"));
    let s2_silent = !b.about(&rest).iter().any(|l| {
        l.starts_with("Fill")
            || l.starts_with("OrderPartiallyFilled")
            || l.starts_with("OrderFilled")
    });
    results.push((
        "S1 finished order, nothing taught: no event about it".into(),
        up && s1_silent,
        format!("lines about {buy}: {:?}", b.about(&buy)),
    ));
    results.push((
        "S2 open order, nothing taught: rebound and announced, no fill".into(),
        s2_rebound && s2_silent,
        format!("lines about {rest}: {:?}", b.about(&rest)),
    ));
    // Cleanup of OUR OWN test order only.
    b.client.cancel(&rest);
    let canceled =
        b.pump(Duration::from_secs(20), |l| l.starts_with(&format!("OrderCanceled {rest}")));
    results.push(("S2 the smoke cancels its own resting order".into(), canceled, String::new()));
    b.close();

    // ---- session C: the same replay, the buy's symbol taught (S3) ----
    let mut c =
        Session::open("C", &config, &[vike_execution::RestoredOrder::new(buy.clone(), SYMBOL)])
            .unwrap_or_else(|e| panic!("session C could not connect ({e}); {left}"));
    let up = c.learn(Duration::from_secs(6));
    let fills: Vec<String> =
        c.about(&buy).iter().filter(|l| l.starts_with("Fill")).map(|l| l.to_string()).collect();
    let partials = c.about(&buy).iter().filter(|l| l.starts_with("OrderPartiallyFilled")).count();
    let under_taught = fills.iter().all(|l| l.contains(&format!("symbol={SYMBOL}")));
    results.push((
        "S3 taught twin: exactly one Fill and one OrderPartiallyFilled under the taught symbol"
            .into(),
        up && fills.len() == 1 && partials == 1 && under_taught,
        format!("lines about {buy}: {:?}", c.about(&buy)),
    ));
    c.close();

    // ---- session D: sell it back (S4) ----
    let mut d = Session::open("D", &config, &[])
        .unwrap_or_else(|e| panic!("session D could not connect ({e}); {left}"));
    d.learn(Duration::from_secs(4));
    d.submit(&sell, -1, sell_price);
    let sold = d.pump(Duration::from_secs(90), |l| l.starts_with(&format!("OrderFilled {sell}")));
    d.pump(Duration::from_secs(2), |_| false);
    results.push((
        "S4 the sell-back filled".into(),
        sold,
        format!("lines about {sell}: {:?}", d.about(&sell)),
    ));
    if !sold {
        d.client.cancel(&sell);
        d.pump(Duration::from_secs(10), |_| false);
    }
    d.close();

    // ---- end state ----
    let (open_after, position_after) = observe(&addr, obs_id, &config.account, "AAPL", &prefix)
        .expect("the observer reads the account");
    eprintln!(
        "AFTER: AAPL position {position_after} (before {position_before}); open orders of this run: {open_after:?}"
    );
    results.push((
        "END: the AAPL position is what it was and no order of this run rests".into(),
        (position_after - position_before).abs() < 1e-9 && open_after.is_empty(),
        format!("position {position_before} -> {position_after}; open {open_after:?}"),
    ));

    eprintln!("RESULTS (order client ids {buy} {rest} {sell}; bridge client id {cid})");
    for (what, ok, detail) in &results {
        eprintln!("  {}  {what}\n        {detail}", verdict(*ok));
    }
    assert!(results.iter().all(|(_, ok, _)| *ok), "a scenario did not hold: see RESULTS above");
}
