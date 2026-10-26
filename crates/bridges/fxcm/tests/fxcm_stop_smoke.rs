//! LIVE FXCM demo smoke of the TRADE-BOUND STOP-LOSS and TAKE-PROFIT (2026-10-26): on the HEDGING demo account a
//! reduce-only `stop` and a reduce-only priced `limit` are bound to the trade they protect (`crates/bridges/fxcm/src/exit.rs`),
//! rest at the venue, and are removed by the venue with their trade.
//!
//! ```text
//! export FCSDK_DIR=<repo>/vendor/fcsdk/linux LD_LIBRARY_PATH=$FCSDK_DIR/lib VIKE_SETTINGS_DIR=<project>/settings
//! cargo test -p vike-fxcm --features fxcm --test fxcm_stop_smoke -- --ignored --nocapture
//! ```
//!
//! **PLACES REAL ORDERS on the demo account**: a handful of EUR/USD market orders of 1000 units, one trade open at a
//! time, and it ends FLAT with no resting order (it asserts so, and closes what is left if a step fails). It needs the
//! market OPEN and LIQUID (Monday to Thursday, 12:30-15:30 UTC), a demo account that holds NO EUR/USD trade and no
//! resting order (it refuses to start otherwise), and the daemon's own FXCM account stopped
//! (`vike-cli secrets account deactivate --id <N>`; reactivate it after): it opens TWO sessions of the demo login.
//!
//! What it proves, each step against the VENUE's own tables read back through the reconcile client:
//!
//! 1. a reduce-only `stop` and a reduce-only priced `limit` on a long are ACCEPTED and rest as a Stop and a Limit order;
//!    the refusals that never reach the venue are named (a part of a trade, a stop that is not reduce-only, a mark trigger,
//!    a second stop);
//! 2. an amend moves the stop in place; a cancel removes the take-profit and leaves the stop;
//! 3. the trade closed by a reduce-only market order takes its exit with it: the venue deletes it and the node hears
//!    ONE `OrderCanceled`;
//! 4. a stop that REALLY FIRES (re-set toward the market until it is hit): it is a fill under the STOP's client order id,
//!    at the stop's rate, under a closing trade id of its own, the venue holds NO trade afterwards (no hedge), and the
//!    take-profit beside it is cancelled by the venue;
//! 5. a SHORT: a buy stop above the market is placed, and cancelled.
//!
//! Triple-gated like the other `*_smoke.rs`: `#[ignore]`, self-skips with no fxcm demo account in the store, and
//! self-skips on a box whose shim did not load (`no ForexConnect shim` is NOT a pass: look for `LOGIN OK`).

use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::time::{Duration, Instant};

use vike_exec::recon::ReconClient;
use vike_exec::{ExecutionClient, Ingest, event_channel};
use vike_fxcm::{FxcmConfig, FxcmExecutionClient, load_fxcm_config_from, recon_client_for};
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::events::Event;
use vike_model::{AccountTier, OrderRequest, OrderType, TriggerBy};

const SYMBOL: &str = "EURUSD";
const INSTRUMENT: &str = "EUR/USD";
/// ONE lot of EUR/USD on the demo is its base unit size, 1000 (measured).
const LOT: f64 = 1000.0;
/// The trade of the first steps: two lots, so that a stop for ONE of them is a part of the trade.
const TRADE: f64 = 2.0 * LOT;
/// One pip of EUR/USD.
const PIP: f64 = 0.0001;

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64
}

/// A price on EUR/USD's five-decimal grid.
fn grid(px: f64) -> f64 {
    (px * 1e5).round() / 1e5
}

/// The demo fxcm account's login: the active default row when there is one, else the one fxcm demo row there is (this
/// smoke runs while the daemon's account is deactivated).
fn config() -> Option<FxcmConfig> {
    let settings_dir = std::env::var("VIKE_SETTINGS_DIR").ok();
    let dir = vike_secrets::smoke_directory();
    let settings = vike_paths::state_path::project_settings_dir_from(
        settings_dir.as_deref(),
        &std::env::current_dir().expect("a working directory"),
    )
    .map(|dir| {
        vike_secrets::venue_setting::load_venue_settings(&dir)
            .unwrap_or_else(|e| panic!("the settings database could not be read: {e}"))
    })
    .and_then(|mut all| all.remove("fxcm"))
    .unwrap_or_default();
    let active = dir.credentials_for("fxcm", &AccountLabel::Default, AccountTier::Demo);
    if let Some(cfg) = load_fxcm_config_from(AccountTier::Demo, &active, &settings) {
        return Some(cfg);
    }
    let id = {
        let rows = dir.rows()?.ok()?.known()?;
        let mut demo = rows
            .iter()
            .filter(|r| r.venue == "fxcm" && r.tier == AccountTier::Demo && r.label.is_none());
        let first = demo.next()?.id;
        demo.next().is_none().then_some(first)?
    };
    let book = dir.credentials()?.ok()?;
    load_fxcm_config_from(AccountTier::Demo, &book.view("fxcm", Some(id)), &settings)
}

fn request(
    coid: &str,
    side: i32,
    qty: f64,
    order_type: OrderType,
    reduce_only: bool,
) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.into(),
        venue: "fxcm".into(),
        symbol: SYMBOL.into(),
        side,
        qty,
        order_type,
        reduce_only,
        ts: now_ms(),
        ..Default::default()
    }
}

fn stop(coid: &str, side: i32, qty: f64, trigger: f64) -> OrderRequest {
    OrderRequest { trigger_price: Some(trigger), ..request(coid, side, qty, OrderType::Stop, true) }
}

fn limit(coid: &str, side: i32, qty: f64, price: f64) -> OrderRequest {
    OrderRequest { price: Some(price), ..request(coid, side, qty, OrderType::Limit, true) }
}

/// The client order id an event is about.
fn coid_of(e: &Event) -> Option<&str> {
    Some(match e {
        Event::OrderSubmitted(x) => &x.client_order_id,
        Event::OrderAccepted(x) => &x.client_order_id,
        Event::OrderRejected(x) => &x.client_order_id,
        Event::OrderCanceled(x) => &x.client_order_id,
        Event::OrderFilled(x) => &x.client_order_id,
        Event::OrderPartiallyFilled(x) => &x.client_order_id,
        Event::OrderModified(x) => &x.client_order_id,
        Event::OrderModifyRejected(x) => &x.client_order_id,
        Event::Fill(x) => &x.client_order_id,
        _ => return None,
    })
}

fn names(evs: &[Event]) -> Vec<&'static str> {
    evs.iter()
        .map(|e| match e {
            Event::OrderSubmitted(_) => "OrderSubmitted",
            Event::OrderAccepted(_) => "OrderAccepted",
            Event::Fill(_) => "Fill",
            Event::OrderPartiallyFilled(_) => "OrderPartiallyFilled",
            Event::OrderFilled(_) => "OrderFilled",
            Event::OrderCanceled(_) => "OrderCanceled",
            Event::OrderRejected(_) => "OrderRejected",
            Event::OrderModified(_) => "OrderModified",
            Event::OrderModifyRejected(_) => "OrderModifyRejected",
            _ => "other",
        })
        .collect()
}

/// Has `coid` ended (filled, cancelled or rejected) in `seen`?
fn ended(seen: &[Event], coid: &str) -> bool {
    seen.iter().any(|e| {
        coid_of(e) == Some(coid)
            && matches!(
                e,
                Event::OrderFilled(_) | Event::OrderCanceled(_) | Event::OrderRejected(_)
            )
    })
}

/// Has `coid` been answered (accepted, or ended)?
fn answered(seen: &[Event], coid: &str) -> bool {
    ended(seen, coid)
        || seen.iter().any(|e| coid_of(e) == Some(coid) && matches!(e, Event::OrderAccepted(_)))
}

/// How many amend answers (`OrderModified` + `OrderModifyRejected`) `coid` has so far, and how many were `OrderModified`.
fn amends(seen: &[Event], coid: &str) -> (usize, usize) {
    let all = seen
        .iter()
        .filter(|e| {
            coid_of(e) == Some(coid)
                && matches!(e, Event::OrderModified(_) | Event::OrderModifyRejected(_))
        })
        .count();
    let ok = seen
        .iter()
        .filter(|e| coid_of(e) == Some(coid) && matches!(e, Event::OrderModified(_)))
        .count();
    (all, ok)
}

type Books = (Vec<(String, i32, f64)>, Vec<(String, i32, f64)>);

/// The exec client, the ingest lane it answers on, a reconcile session to read the venue's tables back with, and the
/// events seen so far (every order's, in arrival order).
struct Venue {
    client: FxcmExecutionClient,
    rx: tokio::sync::mpsc::Receiver<Ingest>,
    recon: Box<dyn ReconClient>,
    seen: Vec<Event>,
    seq: u32,
}

impl Venue {
    fn coid(&mut self, tag: &str) -> String {
        self.seq += 1;
        format!("fxst{tag}{}x{}", now_ms() % 100_000_000, self.seq)
    }

    /// Read events into `seen` until `done(&seen)` or `secs` pass.
    fn wait(&mut self, secs: u64, what: &str, done: impl Fn(&[Event]) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(secs);
        while !done(&self.seen) {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}; saw {:?}",
                names(&self.seen)
            );
            self.pump(Duration::from_millis(500));
        }
    }

    /// Read whatever events arrive in `dur`.
    fn pump(&mut self, dur: Duration) {
        let rt = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
        let deadline = Instant::now() + dur;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return;
            }
            match rt.block_on(async { tokio::time::timeout(left, self.rx.recv()).await }) {
                Ok(Some(Ingest::Event(ev))) => self.seen.push(ev),
                Ok(Some(other)) => panic!("expected Ingest::Event, got {other:?}"),
                Ok(None) => panic!("ingest channel closed"),
                Err(_) => return,
            }
        }
    }

    /// The events about `coid` so far.
    fn about(&self, coid: &str) -> Vec<Event> {
        self.seen.iter().filter(|e| coid_of(e) == Some(coid)).cloned().collect()
    }

    /// Submit and wait for the order to be ANSWERED (accepted, filled, cancelled or rejected).
    fn submit(&mut self, req: &OrderRequest) -> Vec<Event> {
        let coid = req.client_order_id.clone();
        self.client.submit(req);
        self.wait(60, &coid, |seen| answered(seen, &coid));
        self.about(&coid)
    }

    /// A market order, waited for until it ENDS.
    fn market(&mut self, side: i32, qty: f64, reduce_only: bool) -> Vec<Event> {
        let coid = self.coid(if reduce_only { "mc" } else { "mo" });
        self.client.submit(&request(&coid, side, qty, OrderType::Market, reduce_only));
        self.wait(60, &coid, |seen| ended(seen, &coid));
        self.about(&coid)
    }

    /// The venue's open trades `(trade id, side, units)` and its working orders `(type, side, units)`.
    fn books(&self) -> Books {
        let mut trades: Vec<_> = self
            .recon
            .fetch_fill_reports(0)
            .expect("the reconcile read of the open trades")
            .into_iter()
            .map(|f| (f.trade_id.to_string(), f.side, f.last_qty))
            .collect();
        trades.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let mut orders: Vec<_> = self
            .recon
            .fetch_order_status_reports(0)
            .expect("the reconcile read of the orders")
            .into_iter()
            .map(|o| (o.order_type.as_str().to_string(), o.side, o.qty))
            .collect();
        orders.sort_by(|a, b| a.partial_cmp(b).unwrap());
        (trades, orders)
    }

    /// The EUR/USD quote now, `(bid, ask)`, from the reconcile session's Offers table.
    fn quote(&self) -> (f64, f64) {
        let reads =
            vike_fxcm::reads::account_reads("").expect("the reconcile session is registered");
        let offers = reads.offers().expect("the Offers table");
        let o = offers
            .iter()
            .find(|o| o.instrument == INSTRUMENT)
            .expect("EUR/USD in the Offers table");
        (o.bid, o.ask)
    }

    /// Close whatever is open by reduce-only market orders (the path under test, used as the cleanup too): the venue
    /// takes the exits of a closed trade with it.
    fn flatten(&mut self) {
        for _ in 0..6 {
            let (trades, orders) = self.books();
            if trades.is_empty() && orders.is_empty() {
                return;
            }
            let long: f64 = trades.iter().filter(|t| t.1 > 0).map(|t| t.2).sum();
            let short: f64 = trades.iter().filter(|t| t.1 < 0).map(|t| t.2).sum();
            if long > 0.0 {
                self.market(-1, long, true);
            }
            if short > 0.0 {
                self.market(1, short, true);
            }
            std::thread::sleep(Duration::from_secs(3));
        }
    }
}

fn bare_fills(evs: &[Event]) -> Vec<(String, i32, f64, f64)> {
    evs.iter()
        .filter_map(|e| match e {
            Event::Fill(f) => Some((f.trade_id.to_string(), f.side, f.last_qty, f.last_px)),
            _ => None,
        })
        .collect()
}

fn rejection(evs: &[Event]) -> String {
    match evs.last() {
        Some(Event::OrderRejected(r)) => r.reason.to_string(),
        other => panic!("expected a rejection, got {other:?} in {:?}", names(evs)),
    }
}

fn count(evs: &[Event], name: &str) -> usize {
    names(evs).iter().filter(|n| **n == name).count()
}

fn the_steps(v: &mut Venue) {
    // 1. a plain BUY opens the trade the exits will protect (TWO lots, so that half of it can be asked for).
    let evs = v.market(1, TRADE, false);
    let trade = bare_fills(&evs)[0].0.clone();
    let (bid, _) = v.quote();
    assert_eq!(v.books().0, vec![(trade.clone(), 1, TRADE)]);
    println!("S1 BUY 2000: trade {trade}, bid {bid}");

    // The refusals that never reach the venue, each named.
    let c = v.coid("p");
    let why = rejection(&v.submit(&stop(&c, -1, LOT, grid(bid - 20.0 * PIP))));
    assert!(why.contains("WHOLE trade"), "a part of a trade: {why}");
    println!("S1 stop of 1000 on a trade of 2000: {why}");
    let c = v.coid("n");
    let plain = OrderRequest { reduce_only: false, ..stop(&c, -1, TRADE, grid(bid - 20.0 * PIP)) };
    let why = rejection(&v.submit(&plain));
    assert!(why.contains("not reduce-only"), "a stop that is not reduce-only: {why}");
    println!("S1 stop that is not reduce-only: {why}");
    let c = v.coid("m");
    let mark = OrderRequest {
        trigger_by: Some(TriggerBy::Mark),
        ..stop(&c, -1, TRADE, grid(bid - 20.0 * PIP))
    };
    let why = rejection(&v.submit(&mark));
    assert!(why.contains("cannot be chosen"), "a mark trigger: {why}");
    println!("S1 stop with a mark trigger: {why}");
    assert_eq!(v.books().1, vec![], "the refusals rested nothing");

    // 2. the stop and the take-profit bind the SAME trade and rest at the venue.
    let stop_coid = v.coid("s");
    let tp_coid = v.coid("t");
    let stop_req = stop(&stop_coid, -1, TRADE, grid(bid - 20.0 * PIP));
    let evs = v.submit(&stop_req);
    assert_eq!(names(&evs), ["OrderSubmitted", "OrderAccepted"], "{evs:?}");
    let evs = v.submit(&limit(&tp_coid, -1, TRADE, grid(bid + 20.0 * PIP)));
    assert_eq!(names(&evs), ["OrderSubmitted", "OrderAccepted"], "{evs:?}");
    let (trades, orders) = v.books();
    assert_eq!(trades, vec![(trade.clone(), 1, TRADE)], "binding an exit opens nothing");
    assert_eq!(
        orders,
        vec![("limit".to_string(), -1, TRADE), ("stop".to_string(), -1, TRADE)],
        "the venue holds the stop and the take-profit"
    );
    println!("S2 stop {stop_coid} and take-profit {tp_coid} rest at the venue: {orders:?}");
    let c = v.coid("d");
    let why = rejection(&v.submit(&stop(&c, -1, TRADE, grid(bid - 25.0 * PIP))));
    assert!(why.contains("already has a stop"), "{why}");
    println!("S2 second stop: {why}");

    // an amend moves the stop in place
    v.client.modify(&stop_req, None, Some(grid(bid - 19.0 * PIP)));
    v.wait(30, "the amend", |seen| amends(seen, &stop_coid).0 >= 1);
    assert_eq!(amends(&v.seen, &stop_coid), (1, 1), "{:?}", names(&v.about(&stop_coid)));
    println!("S2 amend of the stop: OrderModified");
    // a cancel removes the take-profit and leaves the stop
    v.client.cancel(&tp_coid);
    v.wait(30, "the cancel", |seen| ended(seen, &tp_coid));
    assert_eq!(
        names(&v.about(&tp_coid)),
        ["OrderSubmitted", "OrderAccepted", "OrderCanceled"],
        "one cancel"
    );
    assert_eq!(v.books().1, vec![("stop".to_string(), -1, TRADE)]);
    println!("S2 cancel of the take-profit: the stop stays");

    // 3. the trade closed by a reduce-only market SELL takes the stop with it.
    let evs = v.market(-1, TRADE, true);
    assert_eq!(names(&evs), ["OrderSubmitted", "OrderAccepted", "Fill", "OrderFilled"], "{evs:?}");
    v.wait(30, "the venue's removal of the stop", |seen| ended(seen, &stop_coid));
    let gone = v.about(&stop_coid);
    assert_eq!(count(&gone, "OrderCanceled"), 1, "the stop is cancelled ONCE: {:?}", names(&gone));
    assert_eq!(count(&gone, "OrderFilled"), 0, "{:?}", names(&gone));
    assert_eq!(v.books(), (vec![], vec![]), "no trade, no resting order");
    println!("S3 trade closed by a market order: the venue removed the stop, one OrderCanceled");

    // 4. a stop that REALLY FIRES, with a take-profit beside it.
    let evs = v.market(1, LOT, false);
    let trade = bare_fills(&evs)[0].0.clone();
    let (bid, _) = v.quote();
    let stop_coid = v.coid("s");
    let tp_coid = v.coid("t");
    let mut accepted = grid(bid - 20.0 * PIP);
    let stop_req = stop(&stop_coid, -1, LOT, accepted);
    v.submit(&stop_req);
    v.submit(&limit(&tp_coid, -1, LOT, grid(bid + 20.0 * PIP)));
    println!(
        "S4 trade {trade}; stop at {accepted} and take-profit rest; chasing the stop to the market"
    );
    let deadline = Instant::now() + Duration::from_secs(420);
    let mut asked = accepted;
    while !ended(&v.seen, &stop_coid) {
        assert!(
            Instant::now() < deadline,
            "the stop did not fire in 7 minutes: try a livelier hour"
        );
        let (bid, _) = v.quote();
        let want = grid(bid - 0.6 * PIP);
        if want > accepted && want != asked {
            let before = amends(&v.seen, &stop_coid);
            v.client.modify(&stop_req, None, Some(want));
            asked = want;
            v.wait(20, "the chase amend", |seen| {
                amends(seen, &stop_coid).0 > before.0 || ended(seen, &stop_coid)
            });
            if amends(&v.seen, &stop_coid).1 > before.1 {
                accepted = want;
            } else {
                asked = accepted;
            }
        }
        v.pump(Duration::from_millis(1200));
    }
    v.wait(30, "the take-profit's cancel", |seen| ended(seen, &tp_coid));
    let fired = v.about(&stop_coid);
    assert_eq!(count(&fired, "OrderFilled"), 1, "{:?}", names(&fired));
    assert_eq!(
        count(&fired, "OrderCanceled"),
        0,
        "a fired stop is not also cancelled: {:?}",
        names(&fired)
    );
    let (id, side, qty, px) = bare_fills(&fired)[0].clone();
    assert!(id.starts_with(&format!("{trade}-close-")), "the closing fill's own id: {id}");
    assert_eq!((side, qty), (-1, LOT));
    assert!((px - accepted).abs() <= 2.0 * PIP, "filled at {px}, the stop rested at {accepted}");
    let tp = v.about(&tp_coid);
    assert_eq!(
        count(&tp, "OrderCanceled"),
        1,
        "the take-profit is cancelled once: {:?}",
        names(&tp)
    );
    assert_eq!(v.books(), (vec![], vec![]), "the stop CLOSED the trade: no hedge, nothing resting");
    println!(
        "S4 the stop FIRED: {id} at {px} (rested at {accepted}); take-profit cancelled by the venue; flat"
    );

    // 5. a SHORT: a buy stop above the market is placed, and cancelled.
    let evs = v.market(-1, LOT, false);
    let short = bare_fills(&evs)[0].0.clone();
    let (_, ask) = v.quote();
    let c = v.coid("b");
    let evs = v.submit(&stop(&c, 1, LOT, grid(ask + 20.0 * PIP)));
    assert_eq!(names(&evs), ["OrderSubmitted", "OrderAccepted"], "{evs:?}");
    assert_eq!(v.books().1, vec![("stop".to_string(), 1, LOT)]);
    v.client.cancel(&c);
    v.wait(30, "the buy stop's cancel", |seen| ended(seen, &c));
    assert_eq!(v.books(), (vec![(short.clone(), -1, LOT)], vec![]));
    let evs = v.market(1, LOT, true);
    assert_eq!(names(&evs), ["OrderSubmitted", "OrderAccepted", "Fill", "OrderFilled"], "{evs:?}");
    assert_eq!(v.books(), (vec![], vec![]));
    println!("S5 short {short}: a buy stop above the ask rested and was cancelled; closed");
}

#[test]
#[ignore = "live demo; PLACES REAL ORDERS on the demo account (a handful of EUR/USD market orders of 1000 units); opens two sessions of the demo login"]
fn fxcm_a_stop_and_a_take_profit_bind_to_the_trade_and_the_stop_fires_on_the_hedging_demo() {
    vike_log::test_init();
    let Some(cfg) = config() else {
        eprintln!("skip: no fxcm demo account credentials");
        return;
    };
    if !vike_fxcm::sdk_available() {
        eprintln!("skip: no ForexConnect shim on this box (NOT a pass)");
        return;
    }
    println!("shim: {:?} abi {:?}", vike_fxcm::sdk_shim_path(), vike_fxcm::sdk_abi_version());
    let recon = recon_client_for(&cfg, SYMBOL, "").expect(
        "the reconcile login failed (re-run with --nocapture: the session thread logs FXCM's words)",
    );
    println!("LOGIN OK (reconcile session)");
    assert_eq!(
        recon.fetch_order_status_reports(0).expect("orders").len(),
        0,
        "the demo account holds a resting {SYMBOL} order: this smoke touches only its own"
    );
    let before = recon.fetch_fill_reports(0).expect("trades");
    assert!(before.is_empty(), "the demo account holds {SYMBOL} trades already: {before:?}");
    println!("balance before: {:?}", recon.fetch_balance());

    let (events, rx) = event_channel(256);
    let client = FxcmExecutionClient::spawn(cfg, events);
    let mut venue = Venue { client, rx, recon, seen: Vec::new(), seq: 0 };
    let outcome = catch_unwind(AssertUnwindSafe(|| the_steps(&mut venue)));
    // Whatever happened above, leave the account flat by the path under test.
    venue.flatten();
    let (left, orders) = venue.books();
    println!("balance after: {:?}", venue.recon.fetch_balance());
    venue.client.detach();
    if let Err(panic) = outcome {
        resume_unwind(panic);
    }
    assert_eq!(
        (left.len(), orders.len()),
        (0, 0),
        "the account must end flat: {left:?} {orders:?}"
    );
    println!("FXCM STOP SMOKE DONE: flat, no resting order");
}

/// **The refusal that needs no trade, on a flat account, with whichever shim is installed - and NO order placed.** A
/// reduce-only stop with nothing to protect must never rest as an entry order: with the shim of this release the binder
/// refuses it ("no open long trade ..."); with an OLDER shim (no `fc_open_exits` / `fc_place_exit`) the bridge refuses it
/// BEFORE the wire, naming the missing export and the remedy. Run it from a directory whose `../lib/libfcshim.so` is the
/// old shim to see the second (the loader's first rung); the account is checked flat before and after.
#[test]
#[ignore = "live demo; needs a FLAT account; places NO order when it passes (the order is refused before the wire)"]
fn fxcm_a_stop_with_nothing_to_protect_is_refused_by_name_and_rests_nothing() {
    vike_log::test_init();
    let Some(cfg) = config() else {
        eprintln!("skip: no fxcm demo account credentials");
        return;
    };
    if !vike_fxcm::sdk_available() {
        eprintln!("skip: no ForexConnect shim on this box (NOT a pass)");
        return;
    }
    println!("shim: {:?} abi {:?}", vike_fxcm::sdk_shim_path(), vike_fxcm::sdk_abi_version());
    let recon = recon_client_for(&cfg, SYMBOL, "").expect("the reconcile login failed");
    println!("LOGIN OK (reconcile session)");
    let held = recon.fetch_fill_reports(0).expect("trades");
    assert!(held.is_empty(), "the demo account holds {SYMBOL} trades already: {held:?}");

    let (events, rx) = event_channel(64);
    let client = FxcmExecutionClient::spawn(cfg, events);
    let mut venue = Venue { client, rx, recon, seen: Vec::new(), seq: 0 };
    let c = venue.coid("z");
    let evs = venue.submit(&stop(&c, -1, LOT, 0.5));
    let why = rejection(&evs);
    println!("REFUSED: {why}");
    assert!(
        !evs.iter().any(|e| matches!(e, Event::OrderAccepted(_))),
        "nothing may be accepted: {evs:?}"
    );
    assert!(
        why.contains("no open long trade on EUR/USD") || why.contains("exports no `fc_"),
        "a refusal that names neither the missing trade nor the missing export: {why}"
    );
    let after = venue.books();
    venue.client.detach();
    assert_eq!(after, (vec![], vec![]), "a refused stop rested an order or opened a trade");
    println!("FXCM STOP REFUSAL SMOKE DONE: nothing rested");
}
