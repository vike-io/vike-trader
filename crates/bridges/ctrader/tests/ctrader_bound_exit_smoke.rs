//! LIVE demo smoke for the BOUND reduce_only exit on the HEDGED cTrader demo account, driving the REAL
//! `CtraderExec` (through `live_runtime_for_account`, the seam production starts it through), never a raw
//! wire client:
//!
//!     cargo test -p vike-ctrader --test ctrader_bound_exit_smoke -- --ignored --nocapture --test-threads=1
//!
//! Why it exists. A `reduce_only` resting exit sent with no `positionId` OPENED a position when it
//! triggered on the hedged demo account (2026-10-25, a locked pair the node booked as flat); one sent with
//! the `positionId` of the open position CLOSED it (a raw-wire probe, 2026-10-26); and a bound exit the
//! venue does not cancel when its position closes another way reaches the market later and OPENS a
//! position. `crates/bridges/ctrader/CLAUDE.md`'s "A hedged account binds a reduce-only resting exit" is the
//! design; this file is its live proof, one scenario after another on ONE start:
//!
//!   A. an entry fills, a bound `reduce_only` SELL STOP placed ONE POINT under the market (kept there under a
//!      rising market for up to a minute, so the PRICE can walk through it; then, if it did not, moved to
//!      the market, distance 0, the stated minimum: `slDistance` is 0) TRIGGERS and CLOSES the position: 0
//!      positions after, no opposite position, no order left; the log says which way it fired;
//!   B. a BRACKET (stop-loss + take-profit, both bound to the one position) rests and protects it; the
//!      take-profit is moved to the market and fires, the position closes, and the BRIDGE cancels the
//!      stop-loss (nothing here is the engine's OCO);
//!   C. the orphan cancel after a MARKET exit: a stop and a take-profit are bound, a second independent stop is
//!      refused by name, a `reduce_only` market close closes the position, and the bridge cancels BOTH exits;
//!   D. the named rejection: with no position a `reduce_only` stop is refused and NOTHING rests;
//!   E. a RESTART (the client dropped, a fresh one started, which learns only what the venue's reconcile says)
//!      ADOPTS the resting stop: it covers the position, the fresh client cancels it, and a replacement placed
//!      at once binds;
//!   F. the adopted stop is cancelled by the fresh client when the position closes (the client that placed
//!      it is gone and the venue does not cancel a bound order);
//!   G. cancelling one exit of a bracket leaves the other resting, and a new stop placed at once binds;
//!   H. (0.1.97) ONE net-sized stop over TWO positions rests as two bound orders `<coid>L1`/`<coid>L2` and the
//!      engine sees ONE order: one accept for its coid, no leg coid on the lane, an amend refused, ONE
//!      `OrderCanceled` when the engine cancels it; then the same exit again, and two `reduce_only` market closes
//!      make the bridge cancel both legs (the orphan rule) with ONE `OrderCanceled` for the engine's coid;
//!   I. (0.1.97) on a FLAT hedged account a `reduce_only` MARKET SELL is refused by name (`NOTHING_TO_REDUCE`) and
//!      opens NOTHING (it used to go out as a plain market order and open a short beside nothing);
//!   J. (0.1.97) a bound stop whose position was closed while NO bridge was watching (the client that placed it
//!      gone, a data-only session closing the position) is NAMED by the next start (the `ORPHANED EXIT` error line
//!      of its connect-time look) and is NOT cancelled by it; the scenario cancels it by hand.
//!
//! (The symbol-state refresh of 0.1.97 has no scenario: a demo account cannot be made to change a symbol.)
//!
//! Every scenario ends with a venue read of 0 EURUSD positions and 0 resting orders, and a failure
//! cleans up before it panics. The repo's own remedy `ctrader_close_all_smoke` is the last word if anything is
//! ever left (run it after this).
//!
//! DEMO ONLY (`AccountTier::Demo`; the adapter dials the demo host only), `#[ignore]`d, SELF-SKIPPING
//! without credentials, with the market closed (no quote), or when the account is not flat in EURUSD at the
//! start (it touches nothing it did not open). 1000 units (the minimum) per entry. It places REAL demo
//! orders: never run it casually (`docs/ops/live-smoke-lane.md`).

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use vike_ctrader::CtraderReconClient;
use vike_ctrader::adapter::{CtraderLiveRuntime, live_runtime_for_account};
use vike_ctrader::config::CtraderConfig;
use vike_ctrader::conn::{Command, connect_and_auth};
use vike_ctrader::data::CtraderData;
use vike_ctrader::exec::{NO_POSITION_TO_BIND_AN_EXIT, NOTHING_TO_REDUCE};
use vike_data::DataClient;
use vike_exec::{ExecutionClient, Ingest, event_channel};
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::events::Event;
use vike_model::{AccountTier, HaltAdmit, OrderRequest, OrderType, now_ms};

use common::RecordingSink;

const SYMBOL: &str = "EURUSD";
const UNITS: f64 = 1000.0;
/// How long a venue answer (accept, fill, cancel) may take before the step fails.
const STEP: Duration = Duration::from_secs(25);
/// How long a quote may be before it is too old to place an order from.
const QUOTE_FRESH_MS: i64 = 8_000;

fn round5(price: f64) -> f64 {
    (price * 100_000.0).round() / 100_000.0
}

/// Starts the exec client the way production does (a new connection, a new seed of the venue's books) and
/// returns it with the receiver of its event lane.
type Start<'a> = Box<dyn Fn() -> (CtraderLiveRuntime, tokio::sync::mpsc::Receiver<Ingest>) + 'a>;

struct Live<'a> {
    runtime: CtraderLiveRuntime,
    rx: tokio::sync::mpsc::Receiver<Ingest>,
    events: Vec<Event>,
    raw: CtraderReconClient,
    sink: Arc<RecordingSink>,
    _data: CtraderData,
    serial: u64,
    start: Start<'a>,
}

impl Live<'_> {
    /// A RESTART of the process, as far as this crate can tell: the running client is dropped (its sockets
    /// close) and a fresh one is started, which connects again, authenticates again and seeds its position
    /// book and order map from the venue's answer. What it did not learn from the venue, it does not know.
    fn restart(&mut self) {
        let (runtime, rx) = (self.start)();
        drop(std::mem::replace(&mut self.runtime, runtime));
        self.rx = rx;
        self.events.clear();
    }

    /// Drain the ingest lane into `events` (kept for the whole run).
    fn pump(&mut self) {
        while let Ok(ing) = self.rx.try_recv() {
            if let Ingest::Event(e) = ing {
                self.events.push(e);
            }
        }
    }

    /// Poll until `pred` holds over everything seen so far, or `STEP` passes.
    fn wait(&mut self, pred: impl Fn(&[Event]) -> bool) -> bool {
        let deadline = Instant::now() + STEP;
        loop {
            self.pump();
            if pred(&self.events) {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(150));
        }
    }

    fn coid(&mut self, tag: &str) -> String {
        self.serial += 1;
        format!("vtbx-{tag}-{}-{}", now_ms() % 10_000_000, self.serial)
    }

    /// The latest EURUSD quote `(bid, ask)`, if it is fresh.
    fn quote(&self) -> Option<(f64, f64)> {
        let quotes = self.sink.quotes();
        let (_, _, q) = quotes.last()?;
        (now_ms() - q.local_ts <= QUOTE_FRESH_MS).then_some((q.bid, q.ask))
    }

    /// A fresh quote, waiting up to 30 s for one (the feed goes quiet for seconds at a time).
    fn quote_wait(&self) -> (f64, f64) {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if let Some(q) = self.quote() {
                return q;
            }
            assert!(Instant::now() < deadline, "no fresh EURUSD quote for 30 s");
            std::thread::sleep(Duration::from_millis(250));
        }
    }

    fn open_positions(&self) -> Vec<(i64, i32, i64)> {
        self.raw
            .open_positions()
            .expect("read the open positions")
            .into_iter()
            .map(|p| (p.position_id, p.side, p.volume))
            .collect()
    }

    fn resting(&self) -> Vec<String> {
        self.runtime
            .recon
            .as_ref()
            .expect("the start's own reconcile client")
            .fetch_order_status_reports(0)
            .expect("read the resting orders")
            .into_iter()
            .map(|o| o.client_order_id.unwrap_or_else(|| format!("(no coid) {}", o.venue_order_id)))
            .collect()
    }

    /// Wait until the venue reads flat in EURUSD with nothing resting; returns what is left (empty = flat).
    fn settle_flat(&mut self) -> (Vec<(i64, i32, i64)>, Vec<String>) {
        let deadline = Instant::now() + STEP;
        loop {
            let (positions, orders) = (self.open_positions(), self.resting());
            if (positions.is_empty() && orders.is_empty()) || Instant::now() >= deadline {
                return (positions, orders);
            }
            std::thread::sleep(Duration::from_millis(500));
        }
    }

    /// Best effort back to flat after a failure: cancel what rests, close what is open (reduce_only
    /// MARKET, which closes by position id). Never panics.
    fn clean_up(&mut self) {
        self.pump();
        for coid in self.resting() {
            self.runtime.client.cancel(&coid);
        }
        for (_, side, volume) in self.open_positions() {
            let coid = self.coid("cleanup");
            self.runtime.client.submit(&OrderRequest {
                client_order_id: coid,
                venue: "ctrader".into(),
                symbol: SYMBOL.into(),
                side: -side,
                qty: volume as f64 / 100.0,
                order_type: OrderType::Market,
                reduce_only: true,
                ts: now_ms(),
                ..Default::default()
            });
        }
        let _ = self.settle_flat();
    }

    fn market(&mut self, coid: &str, side: i32, reduce_only: bool) {
        self.runtime.client.submit(&OrderRequest {
            client_order_id: coid.into(),
            venue: "ctrader".into(),
            symbol: SYMBOL.into(),
            side,
            qty: UNITS,
            order_type: OrderType::Market,
            reduce_only,
            ts: now_ms(),
            ..Default::default()
        });
    }

    /// The exit shape a bracket releases on the entry's fill: a `reduce_only` SELL of the position.
    fn exit_request(coid: &str, order_type: OrderType, price: f64) -> OrderRequest {
        OrderRequest {
            client_order_id: coid.into(),
            venue: "ctrader".into(),
            symbol: SYMBOL.into(),
            side: -1,
            qty: UNITS,
            price: matches!(order_type, OrderType::Limit).then_some(price),
            trigger_price: matches!(order_type, OrderType::Stop).then_some(price),
            order_type,
            reduce_only: true,
            ts: now_ms(),
            ..Default::default()
        }
    }

    /// Buy the minimum and wait for the fill and for the venue to show the long.
    fn enter_long(&mut self, tag: &str) -> f64 {
        let coid = self.coid(tag);
        self.market(&coid, 1, false);
        let c = coid.clone();
        assert!(
            self.wait(|e| e
                .iter()
                .any(|x| matches!(x, Event::OrderFilled(f) if f.client_order_id == c))),
            "the entry {coid} did not fill within {STEP:?}"
        );
        let px = self
            .events
            .iter()
            .find_map(|x| match x {
                Event::OrderFilled(f) if f.client_order_id == coid => Some(f.fill.last_px),
                _ => None,
            })
            .expect("fill price");
        let deadline = Instant::now() + STEP;
        while self.open_positions().len() != 1 {
            assert!(Instant::now() < deadline, "the venue does not show the entry's position");
            std::thread::sleep(Duration::from_millis(400));
        }
        px
    }

    fn accepted(&mut self, coid: &str) -> bool {
        let c = coid.to_string();
        self.wait(|e| {
            e.iter().any(|x| matches!(x, Event::OrderAccepted(a) if a.client_order_id == c))
        })
    }

    fn filled(&mut self, coid: &str) -> bool {
        let c = coid.to_string();
        self.wait(|e| {
            e.iter().any(|x| matches!(x, Event::OrderFilled(f) if f.client_order_id == c))
        })
    }

    /// Wait for the venue's `ORDER_REPLACED` of `coid` (an amend), as the canonical `OrderModified`.
    fn modified(&mut self, coid: &str) -> bool {
        let c = coid.to_string();
        self.wait(|e| {
            e.iter().any(|x| matches!(x, Event::OrderModified(m) if m.client_order_id == c))
        })
    }

    fn canceled(&mut self, coid: &str) -> bool {
        let c = coid.to_string();
        self.wait(|e| {
            e.iter().any(|x| matches!(x, Event::OrderCanceled(f) if f.client_order_id == c))
        })
    }

    fn rejection(&mut self, coid: &str) -> Option<String> {
        let c = coid.to_string();
        self.wait(|e| {
            e.iter().any(|x| matches!(x, Event::OrderRejected(r) if r.client_order_id == c))
        });
        self.events.iter().find_map(|x| match x {
            Event::OrderRejected(r) if r.client_order_id == coid => Some(r.reason.to_string()),
            _ => None,
        })
    }
}

fn require_flat(live: &mut Live, scenario: &str) {
    let (positions, orders) = live.settle_flat();
    assert!(
        positions.is_empty() && orders.is_empty(),
        "{scenario}: NOT flat at the end: positions {positions:?}, resting orders {orders:?}"
    );
}

#[test]
#[ignore = "network + REAL cTrader demo endpoint + a demo account's creds: places REAL demo orders (see module doc)"]
fn ctrader_bound_exit_round_trips_on_the_hedged_demo_account() {
    vike_log::test_init();
    let directory = vike_secrets::smoke_directory();
    let credentials =
        directory.credentials_for("ctrader", &AccountLabel::Default, AccountTier::Demo);
    let Some(config) = CtraderConfig::from_credentials(AccountTier::Demo, &credentials) else {
        tracing::warn!(target: "vike_ctrader::smoke", "SKIP: demo account creds absent (live gate)");
        return;
    };

    // THE unit under test: the start production uses, and the REAL `CtraderExec` behind it.
    let start = || {
        let (tx, rx) = event_channel(1024);
        let runtime = live_runtime_for_account(
            &AccountLabel::Default,
            &credentials,
            None,
            SYMBOL,
            true,
            &tx,
            HaltAdmit::default(),
            // A sentinel this smoke owns and never creates.
            std::path::Path::new("ctrader-bound-exit-smoke-never-engaged/HALT"),
        )
        .expect("creds present: the exec handshake must succeed");
        (runtime, rx)
    };
    let (runtime, rx) = start();

    // Quotes (a data-only socket: it cannot place anything) and a raw reconcile socket for the
    // positions, which the exec's own recon reports drop the ids of.
    let sink = Arc::new(RecordingSink::default());
    let data_handle = match connect_and_auth(config.to_conn_config(), sink.clone()) {
        Ok(h) => h,
        Err(e) => {
            tracing::warn!(target: "vike_ctrader::smoke", "SKIP: data connect failed: {e}");
            return;
        }
    };
    let mut data = CtraderData::new(data_handle);
    data.subscribe_quotes(SYMBOL).expect("subscribe EURUSD quotes");
    let raw = CtraderReconClient::connect(&config.to_conn_config(), SYMBOL)
        .expect("a raw reconcile connection");

    let mut live = Live {
        runtime,
        rx,
        events: Vec::new(),
        raw,
        sink,
        _data: data,
        serial: 0,
        start: Box::new(start),
    };

    let deadline = Instant::now() + Duration::from_secs(45);
    while live.quote().is_none() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(300));
    }
    if live.quote().is_none() {
        tracing::warn!(target: "vike_ctrader::smoke", "SKIP: no fresh EURUSD quote (FX closed)");
        return;
    }
    let (positions, orders) = (live.open_positions(), live.resting());
    if !positions.is_empty() || !orders.is_empty() {
        tracing::warn!(
            target: "vike_ctrader::smoke",
            ?positions, ?orders,
            "SKIP: the account is not flat in EURUSD; this smoke touches only what it opens \
             (run ctrader_close_all_smoke first)"
        );
        return;
    }
    tracing::info!(target: "vike_ctrader::smoke", "baseline: EURUSD flat, nothing resting");

    // Run every scenario; on a panic clean up first, then let it fail the test.
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        scenario_a_bound_stop_triggers_and_closes(&mut live);
        scenario_b_bracket_protects_and_the_bridge_cancels_the_other_leg(&mut live);
        scenario_c_orphan_cancel_after_a_market_exit(&mut live);
        scenario_d_no_position_is_refused_by_name(&mut live);
        scenario_e_a_restart_adopts_the_resting_exit(&mut live);
        scenario_f_an_adopted_exit_is_cancelled_when_its_position_closes(&mut live);
        scenario_g_cancelling_one_exit_leaves_the_other_and_a_new_stop_binds(&mut live);
        scenario_h_one_exit_over_two_positions_rests_as_two_bound_orders(&mut live);
        scenario_i_a_reduce_only_market_order_on_a_flat_account_opens_nothing(&mut live);
        scenario_j_a_stale_bound_exit_is_named_at_connect_and_never_cancelled(&mut live, &config);
    }));
    if outcome.is_err() {
        tracing::error!(target: "vike_ctrader::smoke", "FAILED: cleaning the account up before failing");
    }
    live.clean_up();
    let (positions, orders) = live.settle_flat();
    tracing::info!(
        target: "vike_ctrader::smoke",
        ?positions, ?orders,
        "END STATE: EURUSD positions {} / resting orders {}", positions.len(), orders.len()
    );
    assert!(
        positions.is_empty() && orders.is_empty(),
        "left behind: positions {positions:?}, orders {orders:?}: run ctrader_close_all_smoke"
    );
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}

/// A. The headline: a bound `reduce_only` stop TRIGGERS and CLOSES the position, no hedge.
fn scenario_a_bound_stop_triggers_and_closes(live: &mut Live) {
    let entry_px = live.enter_long("a-entry");
    let (bid, _) = live.quote_wait();
    // One point (a tenth of a pip) under the market: it rests, at the smallest distance there is.
    let sl = live.coid("a-sl");
    let mut request = Live::exit_request(&sl, OrderType::Stop, round5(bid - 0.00001));
    live.runtime.client.submit(&request);
    assert!(live.accepted(&sl), "A: the bound stop was not accepted: {:?}", live.rejection(&sl));
    assert!(live.resting().contains(&sl), "A: the stop is not resting at the venue");
    assert_eq!(live.open_positions().len(), 1, "A: the long must still be the only position");

    let filled_now = |live: &mut Live| {
        live.pump();
        live.events.iter().any(|x| matches!(x, Event::OrderFilled(f) if f.client_order_id == sl))
    };
    // Phase 1, the price drives it: keep the stop one point under a RISING market (never lower it) for up to
    // a minute and let the market come down through it.
    let mut fired = false;
    let mut by_price = false;
    let phase1 = Instant::now() + Duration::from_secs(60);
    let mut next_recentre = Instant::now() + Duration::from_secs(4);
    while Instant::now() < phase1 && !fired {
        fired = filled_now(live);
        if !fired && Instant::now() >= next_recentre {
            next_recentre = Instant::now() + Duration::from_secs(4);
            if let Some((bid, _)) = live.quote() {
                let wanted = round5(bid - 0.00001);
                if request.trigger_price.is_some_and(|t| wanted > t) {
                    live.runtime.client.modify(&request, None, Some(wanted));
                    request.trigger_price = Some(wanted);
                }
            }
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    if fired {
        by_price = true;
    }
    // Phase 2, if the market did not come down: move it to the market (distance 0, the stated minimum:
    // `slDistance` is 0) and it fires.
    let deadline = Instant::now() + Duration::from_secs(90);
    while Instant::now() < deadline && !fired {
        if let Some((bid, _)) = live.quote() {
            live.runtime.client.modify(&request, None, Some(round5(bid)));
            request.trigger_price = Some(round5(bid));
        }
        let until = Instant::now() + Duration::from_secs(6);
        while Instant::now() < until && !fired {
            fired = filled_now(live);
            std::thread::sleep(Duration::from_millis(200));
        }
    }
    assert!(fired, "A: the bound stop never triggered");
    tracing::info!(
        target: "vike_ctrader::smoke",
        by_price,
        "A: the stop fired {}",
        if by_price { "by the PRICE walking through it (distance one point)" } else { "after being moved to the market" }
    );
    let fill_side = live.events.iter().find_map(|x| match x {
        Event::OrderFilled(f) if f.client_order_id == sl => Some((f.fill.side, f.fill.last_px)),
        _ => None,
    });
    let (side, px) = fill_side.expect("the stop's fill");
    assert_eq!(side, -1, "A: the stop's fill is the SELL that closes the long");
    require_flat(live, "A");
    tracing::info!(
        target: "vike_ctrader::smoke",
        "SCENARIO A PASS: entry {entry_px}, the bound stop filled SELL at {px} and CLOSED the position; \
         0 positions, 0 orders, no opposite position"
    );
}

/// B. A bracket on the hedged account is protected: both legs bind the one position and rest; the take-profit
/// fires, the position closes, and the bridge cancels the stop-loss.
fn scenario_b_bracket_protects_and_the_bridge_cancels_the_other_leg(live: &mut Live) {
    live.enter_long("b-entry");
    let (bid, ask) = live.quote_wait();
    let (sl, tp) = (live.coid("b-sl"), live.coid("b-tp"));
    let sl_request = Live::exit_request(&sl, OrderType::Stop, round5(bid - 0.0100));
    let mut tp_request = Live::exit_request(&tp, OrderType::Limit, round5(ask + 0.0100));
    live.runtime.client.submit(&sl_request);
    live.runtime.client.submit(&tp_request);
    assert!(live.accepted(&sl), "B: stop-loss not accepted: {:?}", live.rejection(&sl));
    assert!(live.accepted(&tp), "B: take-profit not accepted: {:?}", live.rejection(&tp));
    let resting = live.resting();
    assert!(resting.contains(&sl) && resting.contains(&tp), "B: both legs must rest: {resting:?}");
    let positions = live.open_positions();
    assert_eq!(positions.len(), 1, "B: one long, protected by both legs: {positions:?}");
    assert_eq!(positions[0].1, 1, "B: and it is a LONG, no opposite position");
    tracing::info!(target: "vike_ctrader::smoke", "B: bracket resting, both legs bound to {positions:?}");

    // Amend the take-profit to another far price. The position shape of the venue's ORDER_REPLACED event is
    // not known from the probe, so it is checked here: the amend is confirmed, and the stop-loss and the
    // take-profit both still REST afterwards (no event about one exit may orphan or cancel the other).
    let (_, ask) = live.quote_wait();
    live.runtime.client.modify(&tp_request, None, Some(round5(ask + 0.0120)));
    tp_request.price = Some(round5(ask + 0.0120));
    assert!(live.modified(&tp), "B: the take-profit's amend was not confirmed");
    std::thread::sleep(Duration::from_secs(3));
    let resting = live.resting();
    assert!(
        resting.contains(&sl) && resting.contains(&tp),
        "B: after the take-profit's ORDER_REPLACED both legs must still rest: {resting:?}"
    );
    assert_eq!(live.open_positions().len(), 1, "B: and the long is still the only position");

    // Fire the take-profit: a sell limit AT the bid is marketable.
    let (bid, _) = live.quote_wait();
    live.runtime.client.modify(&tp_request, None, Some(round5(bid)));
    tp_request.price = Some(round5(bid));
    assert!(live.filled(&tp), "B: the take-profit never filled");
    assert!(
        live.canceled(&sl),
        "B: the stop-loss was not cancelled once its position closed (the venue does not; the bridge must)"
    );
    require_flat(live, "B");
    tracing::info!(
        target: "vike_ctrader::smoke",
        "SCENARIO B PASS: the bracket's two legs rested bound to one position; the take-profit fired and \
         closed it; the bridge cancelled the stop-loss; 0 positions, 0 orders"
    );
}

/// C. Orphan cancel after a MARKET exit (the road `market-exit`, `flatten` and `close_all` take), and the
/// second independent stop refused by name.
fn scenario_c_orphan_cancel_after_a_market_exit(live: &mut Live) {
    live.enter_long("c-entry");
    let (bid, ask) = live.quote_wait();
    let (sl, tp, sl2) = (live.coid("c-sl"), live.coid("c-tp"), live.coid("c-sl2"));
    live.runtime.client.submit(&Live::exit_request(&sl, OrderType::Stop, round5(bid - 0.0100)));
    live.runtime.client.submit(&Live::exit_request(&tp, OrderType::Limit, round5(ask + 0.0100)));
    assert!(live.accepted(&sl) && live.accepted(&tp), "C: the bracket legs were not accepted");
    // An independent second stop would close more than the position holds: refused, by name.
    live.runtime.client.submit(&Live::exit_request(&sl2, OrderType::Stop, round5(bid - 0.0150)));
    let reason = live.rejection(&sl2).expect("C: the second stop must be refused");
    assert!(
        reason.contains("already has exits of this kind"),
        "C: the refusal names why: {reason}"
    );
    assert!(!live.resting().contains(&sl2), "C: the refused stop must not rest");

    let close = live.coid("c-close");
    live.market(&close, -1, true);
    assert!(live.filled(&close), "C: the reduce_only market close did not fill");
    assert!(live.canceled(&sl), "C: the stop-loss was not cancelled when its position closed");
    assert!(live.canceled(&tp), "C: the take-profit was not cancelled when its position closed");
    require_flat(live, "C");
    tracing::info!(
        target: "vike_ctrader::smoke",
        "SCENARIO C PASS: a second stop was refused by name; a reduce_only market close closed the \
         position and the bridge cancelled BOTH bound exits; 0 positions, 0 orders"
    );
}

/// D. With no position there is nothing to bind to: refused by name, nothing rests, nothing opens.
fn scenario_d_no_position_is_refused_by_name(live: &mut Live) {
    require_flat(live, "D (before)");
    let (bid, _) = live.quote_wait();
    let sl = live.coid("d-sl");
    live.runtime.client.submit(&Live::exit_request(&sl, OrderType::Stop, round5(bid - 0.0100)));
    let reason = live.rejection(&sl).expect("D: the exit must be refused");
    assert_eq!(reason, NO_POSITION_TO_BIND_AN_EXIT, "D: the refusal is the named one");
    // Give a wrongly-sent order time to show up before reading the venue.
    std::thread::sleep(Duration::from_secs(4));
    require_flat(live, "D");
    tracing::info!(
        target: "vike_ctrader::smoke",
        "SCENARIO D PASS: with no position a reduce_only stop was refused by name; nothing rests, \
         nothing opened"
    );
}

/// E. A RESTART adopts the exit that rests on an open position: a stop is rested bound to a long, the client
/// is dropped and a fresh one started (it learns only what the venue's reconcile tells it), and then the
/// fresh client (1) counts the stop as covering the position (a second stop is refused), (2) cancels it
/// through the order map it seeded, (3) binds a replacement placed at once, without waiting for the cancel's
/// answer (a trailing stop done as cancel + new stop).
fn scenario_e_a_restart_adopts_the_resting_exit(live: &mut Live) {
    live.enter_long("e-entry");
    let (bid, _) = live.quote_wait();
    let sl = live.coid("e-sl");
    live.runtime.client.submit(&Live::exit_request(&sl, OrderType::Stop, round5(bid - 0.0100)));
    assert!(live.accepted(&sl), "E: the stop was not accepted: {:?}", live.rejection(&sl));

    live.restart();
    let sl2 = live.coid("e-sl2");
    live.runtime.client.submit(&Live::exit_request(&sl2, OrderType::Stop, round5(bid - 0.0150)));
    let reason = live.rejection(&sl2).expect("E: a second stop must be refused");
    assert!(
        reason.contains("already has exits of this kind"),
        "E: the adopted stop must count as covering the position: {reason}"
    );

    live.runtime.client.cancel(&sl);
    let sl3 = live.coid("e-sl3");
    live.runtime.client.submit(&Live::exit_request(&sl3, OrderType::Stop, round5(bid - 0.0120)));
    assert!(live.canceled(&sl), "E: the adopted stop was not cancelled through the fresh client");
    assert!(live.accepted(&sl3), "E: the replacement did not bind: {:?}", live.rejection(&sl3));
    let resting = live.resting();
    assert!(resting.contains(&sl3) && !resting.contains(&sl), "E: resting: {resting:?}");

    let close = live.coid("e-close");
    live.market(&close, -1, true);
    assert!(live.filled(&close), "E: the market close did not fill");
    assert!(live.canceled(&sl3), "E: the replacement was not cancelled when the position closed");
    require_flat(live, "E");
    tracing::info!(
        target: "vike_ctrader::smoke",
        "SCENARIO E PASS: after a restart the resting stop was ADOPTED (it covered the position, the \
         fresh client cancelled it, a replacement placed at once bound); 0 positions, 0 orders"
    );
}

/// F. The adopted exit does its job: after a restart the position is closed through the fresh client and the
/// BRIDGE cancels the stop it did not place (only the adopted row can make that happen; the client that placed
/// it is gone and the venue does not cancel a bound order).
fn scenario_f_an_adopted_exit_is_cancelled_when_its_position_closes(live: &mut Live) {
    live.enter_long("f-entry");
    let (bid, _) = live.quote_wait();
    let sl = live.coid("f-sl");
    live.runtime.client.submit(&Live::exit_request(&sl, OrderType::Stop, round5(bid - 0.0100)));
    assert!(live.accepted(&sl), "F: the stop was not accepted: {:?}", live.rejection(&sl));

    live.restart();
    let close = live.coid("f-close");
    live.market(&close, -1, true);
    assert!(live.filled(&close), "F: the market close did not fill");
    assert!(
        live.canceled(&sl),
        "F: the stop placed before the restart was left resting: the adoption did not happen"
    );
    require_flat(live, "F");
    tracing::info!(
        target: "vike_ctrader::smoke",
        "SCENARIO F PASS: a stop placed before a restart was cancelled by the fresh client when its \
         position closed; 0 positions, 0 orders"
    );
}

/// G. Cancelling ONE exit of a bracket while the position stays open leaves the other resting, and a new
/// stop placed at once binds (the cancelled one no longer counts as covering).
fn scenario_g_cancelling_one_exit_leaves_the_other_and_a_new_stop_binds(live: &mut Live) {
    live.enter_long("g-entry");
    let (bid, ask) = live.quote_wait();
    let (sl, tp) = (live.coid("g-sl"), live.coid("g-tp"));
    live.runtime.client.submit(&Live::exit_request(&sl, OrderType::Stop, round5(bid - 0.0100)));
    live.runtime.client.submit(&Live::exit_request(&tp, OrderType::Limit, round5(ask + 0.0100)));
    assert!(live.accepted(&sl) && live.accepted(&tp), "G: the bracket legs were not accepted");

    live.runtime.client.cancel(&sl);
    let sl2 = live.coid("g-sl2");
    live.runtime.client.submit(&Live::exit_request(&sl2, OrderType::Stop, round5(bid - 0.0120)));
    assert!(live.canceled(&sl), "G: the stop was not cancelled");
    assert!(live.accepted(&sl2), "G: the new stop did not bind: {:?}", live.rejection(&sl2));
    std::thread::sleep(Duration::from_secs(3));
    let resting = live.resting();
    assert!(
        resting.contains(&tp) && resting.contains(&sl2) && !resting.contains(&sl),
        "G: the take-profit must survive the stop's cancel: {resting:?}"
    );
    assert_eq!(live.open_positions().len(), 1, "G: the position is still open");

    // And an amend of the take-profit (an ORDER_REPLACED) leaves the new stop resting too.
    let (_, ask) = live.quote_wait();
    let tp_request = Live::exit_request(&tp, OrderType::Limit, round5(ask + 0.0100));
    live.runtime.client.modify(&tp_request, None, Some(round5(ask + 0.0130)));
    assert!(live.modified(&tp), "G: the take-profit's amend was not confirmed");
    std::thread::sleep(Duration::from_secs(3));
    let resting = live.resting();
    assert!(
        resting.contains(&tp) && resting.contains(&sl2),
        "G: after the take-profit's ORDER_REPLACED the new stop must still rest: {resting:?}"
    );

    let close = live.coid("g-close");
    live.market(&close, -1, true);
    assert!(live.filled(&close), "G: the market close did not fill");
    assert!(live.canceled(&tp) && live.canceled(&sl2), "G: the exits were not cancelled with it");
    require_flat(live, "G");
    tracing::info!(
        target: "vike_ctrader::smoke",
        "SCENARIO G PASS: cancelling the stop left the take-profit resting, a stop placed at once bound; \
         the close cancelled both; 0 positions, 0 orders"
    );
}

/// How many events of the lane carry `coid` as a terminal-or-accept of that exact order.
fn count_events(events: &[Event], coid: &str, pick: impl Fn(&Event) -> Option<&str>) -> usize {
    events.iter().filter(|e| pick(e) == Some(coid)).count()
}

fn accepted_coid(e: &Event) -> Option<&str> {
    match e {
        Event::OrderAccepted(a) => Some(a.client_order_id.as_str()),
        _ => None,
    }
}

fn canceled_coid(e: &Event) -> Option<&str> {
    match e {
        Event::OrderCanceled(c) => Some(c.client_order_id.as_str()),
        _ => None,
    }
}

/// H. (0.1.97) ONE net-sized stop over TWO positions rests as two bound orders and the engine sees ONE order.
fn scenario_h_one_exit_over_two_positions_rests_as_two_bound_orders(live: &mut Live) {
    live.enter_long("h-entry1");
    let second = live.coid("h-entry2");
    live.market(&second, 1, false);
    assert!(live.filled(&second), "H: the second entry did not fill");
    let deadline = Instant::now() + STEP;
    while live.open_positions().len() != 2 {
        assert!(Instant::now() < deadline, "H: the hedged account does not show two positions");
        std::thread::sleep(Duration::from_millis(400));
    }

    let (bid, _) = live.quote_wait();
    let net = live.coid("h-net");
    let (leg1, leg2) = (format!("{net}L1"), format!("{net}L2"));
    let mut request = Live::exit_request(&net, OrderType::Stop, round5(bid - 0.0100));
    request.qty = UNITS * 2.0; // the net size of both positions
    live.runtime.client.submit(&request);
    assert!(
        live.accepted(&net),
        "H: the net-sized stop was not accepted: {:?}",
        live.rejection(&net)
    );
    std::thread::sleep(Duration::from_secs(3));
    let resting = live.resting();
    assert!(
        resting.contains(&leg1) && resting.contains(&leg2) && !resting.contains(&net),
        "H: the venue must hold the two legs, not the engine's coid: {resting:?}"
    );
    assert_eq!(live.open_positions().len(), 2, "H: both positions are still open");
    assert_eq!(count_events(&live.events, &net, accepted_coid), 1, "H: ONE accept for the exit");
    assert!(
        !live.events.iter().any(|e| format!("{e:?}").contains(&format!("{net}L"))),
        "H: a leg's coid reached the engine's lane"
    );

    // An amend is refused (non-terminal), nothing moves.
    live.runtime.client.modify(&request, None, Some(round5(bid - 0.0110)));
    assert!(
        live.wait(|e| e.iter().any(|x| matches!(
            x,
            Event::OrderModifyRejected(m)
                if m.client_order_id == net && m.reason.contains("cannot be amended in place")
        ))),
        "H: the amend of a split exit must be refused by name"
    );

    // The engine cancels ITS order: both legs go, the engine hears ONE cancel.
    live.runtime.client.cancel(&net);
    assert!(live.canceled(&net), "H: the exit was not cancelled");
    std::thread::sleep(Duration::from_secs(3));
    assert!(live.resting().is_empty(), "H: a leg is still resting: {:?}", live.resting());
    assert_eq!(count_events(&live.events, &net, canceled_coid), 1, "H: ONE cancel for the exit");

    // The same exit again, then both positions closed by two reduce_only market orders: the bridge cancels
    // each leg when its position goes (the venue will not), and the engine hears ONE cancel when the last is gone.
    let net2 = live.coid("h-net2");
    let mut request = Live::exit_request(&net2, OrderType::Stop, round5(bid - 0.0100));
    request.qty = UNITS * 2.0;
    live.runtime.client.submit(&request);
    assert!(live.accepted(&net2), "H: the second net-sized stop was not accepted");
    for tag in ["h-close1", "h-close2"] {
        let close = live.coid(tag);
        live.market(&close, -1, true);
        assert!(live.filled(&close), "H: the market close {close} did not fill");
    }
    assert!(live.canceled(&net2), "H: the bridge did not cancel the legs of the closed positions");
    require_flat(live, "H");
    assert_eq!(count_events(&live.events, &net2, canceled_coid), 1, "H: ONE cancel for the exit");
    tracing::info!(
        target: "vike_ctrader::smoke",
        "SCENARIO H PASS: one stop over two positions rested as two bound orders and the engine saw one \
         order (one accept, one cancel); an amend was refused; the closes cancelled both legs"
    );
}

/// I. (0.1.97) on a FLAT hedged account a `reduce_only` MARKET SELL is refused by name and opens NOTHING.
fn scenario_i_a_reduce_only_market_order_on_a_flat_account_opens_nothing(live: &mut Live) {
    require_flat(live, "I (precondition)");
    let coid = live.coid("i-flat");
    live.market(&coid, -1, true);
    let reason = live
        .rejection(&coid)
        .expect("I: a reduce_only market order with nothing to reduce must be refused");
    assert_eq!(reason, NOTHING_TO_REDUCE, "I: refused, but not by name");
    std::thread::sleep(Duration::from_secs(3));
    require_flat(live, "I");
    tracing::info!(
        target: "vike_ctrader::smoke",
        "SCENARIO I PASS: a reduce_only market SELL on a flat hedged account was refused by name; 0 positions"
    );
}

/// J. (0.1.97) a bound stop whose position was closed while NO bridge watched is NAMED by the next start and is
/// not cancelled by it.
fn scenario_j_a_stale_bound_exit_is_named_at_connect_and_never_cancelled(
    live: &mut Live,
    config: &CtraderConfig,
) {
    // A spare client, started on the flat account before anything is placed: it takes the placing client's
    // seat so that one can be DROPPED (no bridge watching) without a fresh one being started yet. It places
    // nothing and holds no exit rows, so it cancels nothing.
    let (spare, spare_rx) = (live.start)();
    live.enter_long("j-entry");
    let (bid, _) = live.quote_wait();
    let sl = live.coid("j-sl");
    live.runtime.client.submit(&Live::exit_request(&sl, OrderType::Stop, round5(bid - 0.0100)));
    assert!(live.accepted(&sl), "J: the stop was not accepted: {:?}", live.rejection(&sl));
    let positions = live.open_positions();
    assert_eq!(positions.len(), 1, "J: one position expected, got {positions:?}");
    let (position_id, _, volume) = positions[0];

    // The client that placed the stop goes away; a data-only session (it keeps no exit table) closes the position.
    drop(std::mem::replace(&mut live.runtime, spare));
    live.rx = spare_rx;
    live.events.clear();
    let closer = connect_and_auth(config.to_conn_config(), live.sink.clone())
        .expect("J: a data-only session to close the position with");
    closer
        .tx
        .send(Command::ClosePosition {
            position_id,
            volume,
            client_order_id: format!("vtbx-j-raw-{}", now_ms() % 10_000_000),
        })
        .expect("J: the closer's command channel");
    let deadline = Instant::now() + STEP;
    while !live.open_positions().is_empty() {
        assert!(Instant::now() < deadline, "J: the raw close never reached the venue");
        std::thread::sleep(Duration::from_millis(400));
    }
    closer.shutdown();
    assert!(
        live.resting().contains(&sl),
        "J: the venue keeps the stop after its position closed (the danger)"
    );

    // The next start: its connect-time look names the stop, and nothing cancels it.
    let ((), log) = vike_log::capture::captured(|| live.restart());
    let named: Vec<_> = log
        .iter()
        .filter(|e| e.level == tracing::Level::ERROR && e.message.contains("ORPHANED EXIT"))
        .collect();
    assert!(!named.is_empty(), "J: no ORPHANED EXIT line at connect; log: {log:#?}");
    // Which form of the look-up the venue answered (unbounded, or over the last week) is not measured yet: print it.
    let answered: Vec<_> = log
        .iter()
        .filter(|e| e.message.contains("answered the order look-up"))
        .map(|e| e.field("bounded").unwrap_or("?").to_string())
        .collect();
    tracing::info!(target: "vike_ctrader::smoke", ?answered, "J: the look-up answered with bounded=");
    std::thread::sleep(Duration::from_secs(5));
    assert!(
        live.resting().contains(&sl),
        "J: the stop must be left alone: the bridge never cancels it"
    );
    assert!(live.open_positions().is_empty(), "J: the stale stop must not have opened a position");
    // By hand, as the log asks.
    live.runtime.client.cancel(&sl);
    assert!(live.canceled(&sl), "J: the hand cancel of the stale stop did not go through");
    require_flat(live, "J");
    tracing::info!(
        target: "vike_ctrader::smoke",
        lines = named.len(),
        "SCENARIO J PASS: a stop left behind by a closed position was named at connect and left alone"
    );
}
