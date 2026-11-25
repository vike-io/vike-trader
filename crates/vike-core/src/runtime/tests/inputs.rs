//! A live mount's INPUTS (decision 0271's live half, `crate::runtime::inputs`), white-box over the REAL
//! `dispatch` path: the opener's history and the bar lane's bars read as of the decision's close (the
//! boundary seen, the bar in progress never), an input series no mount hears moving nothing else, a missing
//! input refused by name at both doors, a stale input holding opening orders while a reduce passes, a
//! restart reading every input again, and a decision at a close it shares with an input bar waiting for that
//! bar (`crate::runtime::shared_close`): released by its arrival, by the grace, or by the mount's next bar.

use super::*;
use std::sync::Mutex as TestMutex;
use vike_execution::MountSpec;
use vike_execution::testing::RecordingClient;
use vike_model::OrderBroker;

use crate::runtime::runtime_mount_tests::{mount_command, order_client_id_fill, spec};
use crate::runtime::test_support::{core_with, order_client_id_of};

const H: i64 = 3_600_000;
const H4: i64 = 4 * H;
const DAY: i64 = 24 * H;
/// The traded symbol's own higher timeframe, on the mount's venue: the case where a stray bar would do harm.
const HIGHER: &str = "sim:BTCUSDT:4h";
/// A user's lane (a CSV import): its key leads the venue with the lane, as `vike-tradehub` spells it.
const LANE: &str = "user-mine:binance:USDT-USD:1d";

/// An FX pair on an FX-week broker: an input whose market keeps a calendar (`InputRead::calendar`). Its
/// stored hours end at the week's close.
const FX: &str = "fxcm:EURUSD:1h";
/// Another, whose stored hours end on a Thursday morning, its market open.
const FX_THU: &str = "fxcm:GBPUSD:1h";
/// 2024-01-05, a Friday, 00:00 UTC (winter: the FX week closes at 22:00 UTC and opens Sunday 22:00 UTC).
const FRI: i64 = 19_727 * DAY;

fn fx_key(symbol: &str) -> SeriesKey {
    ("fxcm".into(), symbol.into(), "1h".into())
}

/// FX's stored hours: Friday 00:00 to 21:00, the last closing at the week's close, 22:00.
fn fx_history() -> Vec<Bar> {
    (0..=21).map(|h| bar(FRI + h * H, 1.2)).collect()
}

/// FX_THU's stored hours: Thursday 00:00 to 10:00, the last closing at 11:00.
fn fx_thursday() -> Vec<Bar> {
    (0..=10).map(|h| bar(FRI - DAY + h * H, 1.1)).collect()
}

fn higher_key() -> SeriesKey {
    ("sim".into(), "BTCUSDT".into(), "4h".into())
}

fn lane_key() -> SeriesKey {
    ("user-mine:binance".into(), "USDT-USD".into(), "1d".into())
}

fn own_key() -> SeriesKey {
    ("sim".into(), "BTCUSDT".into(), "1h".into())
}

fn bar(ts: i64, close: f64) -> Bar {
    Bar::ohlcv(ts, close, close, close, close, 1.0)
}

/// The stored histories the opener answers: the 4h input's bars at 0 and 4h (closing at 4h and 8h), the
/// lane's one day bar at 0 (closing at 24h). Every name it is asked is recorded; any other name is refused.
fn opener(asked: &Arc<TestMutex<Vec<String>>>, higher: Vec<Bar>) -> InputOpener {
    let asked = Arc::clone(asked);
    Box::new(move |name: &str, _venue: &str, _symbol: &str, _interval: &str| {
        asked.lock().unwrap().push(name.to_string());
        match name {
            HIGHER => Ok(InputRead {
                key: higher_key(),
                interval_ms: H4,
                bars: higher.clone(),
                calendar: None,
            }),
            LANE => Ok(InputRead {
                key: lane_key(),
                interval_ms: DAY,
                bars: vec![bar(0, 1.0)],
                calendar: None,
            }),
            "sim:ETHUSDT:1h" => Ok(InputRead {
                key: ("sim".into(), "ETHUSDT".into(), "1h".into()),
                interval_ms: H,
                bars: Vec::new(),
                calendar: None,
            }),
            FX | FX_THU => Ok(InputRead {
                key: fx_key(if name == FX { "EURUSD" } else { "GBPUSD" }),
                interval_ms: H,
                bars: if name == FX { fx_history() } else { fx_thursday() },
                calendar: Some(vike_venues::session::FX_WEEK),
            }),
            other => Err(format!("the datahub holds no series named {other}")),
        }
    })
}

fn stored_higher() -> Vec<Bar> {
    vec![bar(0, 10.0), bar(H4, 20.0)]
}

/// What one decision read: its bar's open, the 4h input's close, the lane's close.
type Seen = Arc<TestMutex<Vec<(i64, Option<f64>, Option<f64>)>>>;

/// Reads both inputs at each decision and submits, at market, every `(side, qty)` queued in `orders`; the
/// reason of every order the core refused is in `denied`.
struct Reader {
    names: Vec<String>,
    seen: Seen,
    orders: Arc<TestMutex<Vec<(i32, f64)>>>,
    denied: Arc<TestMutex<Vec<String>>>,
}

impl Strategy<LiveBroker> for Reader {
    fn inputs(&self) -> Vec<String> {
        self.names.clone()
    }
    fn on_bar(&mut self, b: &mut LiveBroker, bar: &Bar) {
        let read = |name: &str| b.input(name).map(|x| x.close);
        let look = (bar.timestamp, read(HIGHER), read(LANE));
        self.seen.lock().unwrap().push(look);
        assert_eq!(b.input("sim:ETHUSDT:4h"), None, "an undeclared name reads nothing");
        for (side, qty) in self.orders.lock().unwrap().drain(..) {
            b.place("", "", side, qty, vike_model::OrderPlacement::Market, false);
        }
    }
    fn on_order_event(&mut self, _b: &mut LiveBroker, event: &vike_model::OrderEvent) {
        if let vike_model::OrderEventKind::Denied { reason } = &event.kind {
            self.denied.lock().unwrap().push(reason.clone());
        }
    }
}

fn reader(names: &[&str], seen: &Seen, orders: &Arc<TestMutex<Vec<(i32, f64)>>>) -> Reader {
    Reader {
        names: names.iter().map(|n| n.to_string()).collect(),
        seen: Arc::clone(seen),
        orders: Arc::clone(orders),
        denied: Arc::default(),
    }
}

fn mount(strategy: Box<dyn Strategy<LiveBroker> + Send>, id: &str) -> Mount {
    Mount {
        order_client_id_code: None,
        account: None,
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        interval: "1h".into(),
        strategy,
        symbols: Vec::new(),
        underlying_symbol: None,
        controller_id: Some(id.to_string()),
    }
}

fn close(core: &mut CoreThread<RecordingClient>, (venue, symbol, interval): SeriesKey, bar: Bar) {
    core.dispatch(Ingest::BarClose(Box::new(BarUpdate {
        venue,
        symbol,
        interval,
        bar,
        sides: None,
    })));
}

fn noted(core: &CoreThread<RecordingClient>, needle: &str) -> usize {
    core.recent.iter().filter(|l| l.contains(needle)).count()
}

struct Rig {
    core: CoreThread<RecordingClient>,
    seen: Seen,
    orders: Arc<TestMutex<Vec<(i32, f64)>>>,
    denied: Arc<TestMutex<Vec<String>>>,
    asked: Arc<TestMutex<Vec<String>>>,
}

fn rig(higher: Vec<Bar>) -> Rig {
    let (seen, orders, asked) = (Seen::default(), Arc::default(), Arc::default());
    let strategy = reader(&[HIGHER, LANE], &seen, &orders);
    let denied = Arc::clone(&strategy.denied);
    let mut core = core_with(CoreConfig {
        seed_cash: 10_000.0,
        strategy: Some(mount(Box::new(strategy), "reads")),
        input_opener: Some(opener(&asked, higher)),
        // The core's clock at the epoch, behind every bar here: the order-event lane's instant then never
        // runs ahead of the decisions the test makes (the stale hold judges the latest instant it saw).
        clock: Box::new(|| 0_i64),
        ..CoreConfig::default()
    });
    // What `spawn_core` arms for a core with a mount, so a refused order's `Denied` reaches its strategy.
    core.engine.collect_applied_fills = true;
    Rig { core, seen, orders, denied, asked }
}

/// **A mount reads its inputs as of its decision's CLOSE**, from the stored history and from the bars the bar
/// lane brings after: an input bar closing exactly at the decision is seen, the one in progress never is,
/// and a lane's seed under its own key is filed like a venue's bar.
#[test]
fn a_mount_reads_its_inputs_as_of_the_close_of_its_bar() {
    let Rig { mut core, seen, asked, .. } = rig(stored_higher());
    assert!(core.mounts[0].is_some(), "mounted: {:?}", core.recent);
    assert_eq!(*asked.lock().unwrap(), vec![HIGHER.to_string(), LANE.to_string()]);

    // Decides at 07:00: the 4h bar opened at 4h closes at 8h, not yet; the day bar closes at 24h.
    close(&mut core, own_key(), bar(6 * H, 100.0));
    // Decides at 08:00, exactly when the 4h bar opened at 4h closes: seen.
    close(&mut core, own_key(), bar(7 * H, 101.0));
    // The next 4h bar arrives on the bar lane; the decision at 12:00 sees it, the one at 11:00 does not.
    close(&mut core, higher_key(), bar(2 * H4, 30.0));
    close(&mut core, own_key(), bar(10 * H, 102.0));
    close(&mut core, own_key(), bar(11 * H, 103.0));
    // Decides at 24:00, the day bar's close; then a seed of the lane's next day, seen at 48:00.
    close(&mut core, own_key(), bar(23 * H, 104.0));
    core.dispatch(Ingest::BarSeed(Box::new(BarSeed {
        venue: lane_key().0,
        symbol: lane_key().1,
        interval: lane_key().2,
        bars: vec![bar(0, 1.0), bar(DAY, 2.0)],
    })));
    close(&mut core, own_key(), bar(2 * DAY - 2 * H, 105.0));
    close(&mut core, own_key(), bar(2 * DAY - H, 106.0));

    assert_eq!(
        *seen.lock().unwrap(),
        vec![
            (6 * H, Some(10.0), None),
            (7 * H, Some(20.0), None),
            (10 * H, Some(20.0), None),
            (11 * H, Some(30.0), None),
            (23 * H, Some(30.0), Some(1.0)),
            (2 * DAY - 2 * H, Some(30.0), Some(1.0)),
            (2 * DAY - H, Some(30.0), Some(2.0)),
        ]
    );
}

/// **A bar of an input series no mount hears moves nothing else**: the 4h bar of the traded symbol reaches
/// no paper fill clock, no mark, no bar cache and no `on_bar`, because its range spans prices from before
/// the orders resting now.
#[test]
fn a_bar_of_an_input_series_no_mount_hears_moves_nothing_else() {
    let Rig { mut core, seen, .. } = rig(stored_higher());
    close(&mut core, own_key(), bar(6 * H, 100.0));
    let mark = core.engine.account.mark_of("sim", "BTCUSDT");
    let (heard, clocked) = (seen.lock().unwrap().len(), core.engine.client.bars.len());

    close(&mut core, higher_key(), Bar::ohlcv(2 * H4, 1.0, 500.0, 1.0, 250.0, 1.0));

    assert_eq!(core.engine.account.mark_of("sim", "BTCUSDT"), mark, "no mark moved");
    assert_eq!(core.engine.client.bars.len(), clocked, "no paper fill clock ticked");
    assert_eq!(seen.lock().unwrap().len(), heard, "no strategy stepped");
    assert!(!core.bars.contains_key(&higher_key()), "no bar cache holds it");
}

/// **An input the datahub cannot serve refuses the mount BY NAME, at both doors**: at spawn the slot is
/// tombstoned beside a mount that keeps running, at run time no slot is made. The note names each input and
/// why; a core handed no opener refuses the same way.
#[test]
fn a_missing_input_is_refused_by_name_at_both_doors() {
    let (seen, orders, asked) = (Seen::default(), Arc::default(), Arc::default());
    let missing = ["binance:SOLUSDT:4h", "sim:ETHUSDT:1h"];
    let factory_seen = Arc::clone(&seen);
    let factory_orders = Arc::clone(&orders);
    let mut core = core_with(CoreConfig {
        seed_cash: 10_000.0,
        strategy: Some(mount(Box::new(reader(&missing, &seen, &orders)), "reads")),
        extra_mounts: vec![mount(Box::new(reader(&[HIGHER], &seen, &orders)), "healthy")],
        input_opener: Some(opener(&asked, stored_higher())),
        strategy_factory: Some(Box::new(move |_: &MountSpec| {
            Ok((
                Box::new(reader(&missing, &factory_seen, &factory_orders))
                    as Box<dyn Strategy<LiveBroker> + Send>,
                None,
            ))
        })),
        ..CoreConfig::default()
    });
    assert!(core.mounts[0].is_none(), "the spawn door tombstones it");
    assert!(core.mounts[1].is_some(), "the mount beside it keeps running");
    let note = core.recent.iter().find(|l| l.contains("MOUNT REFUSED: `reads`")).cloned();
    let note = note.unwrap_or_else(|| panic!("recent: {:?}", core.recent));
    assert!(note.contains("`binance:SOLUSDT:4h`: the datahub holds no series named"), "{note}");
    assert!(note.contains("`sim:ETHUSDT:1h`: the datahub holds no bar of it up to now"), "{note}");

    core.dispatch(mount_command(spec("rt-reads")));
    assert_eq!(core.mounts.len(), 2, "the runtime door makes no slot");
    assert_eq!(
        noted(&core, "MOUNT REFUSED: `rt_reads` reads an input the datahub does not serve"),
        1
    );

    let (bare_seen, bare_orders) = (Arc::clone(&seen), Arc::clone(&orders));
    let mut bare = core_with(CoreConfig {
        seed_cash: 10_000.0,
        strategy: Some(mount(Box::new(reader(&[LANE], &seen, &orders)), "bare")),
        strategy_factory: Some(Box::new(move |_: &MountSpec| {
            Ok((
                Box::new(reader(&[LANE], &bare_seen, &bare_orders))
                    as Box<dyn Strategy<LiveBroker> + Send>,
                None,
            ))
        })),
        ..CoreConfig::default()
    });
    assert!(bare.mounts[0].is_none());
    assert_eq!(
        noted(&bare, "MOUNT REFUSED: `bare` reads the input `user-mine:binance:USDT-USD:1d`"),
        1
    );
    bare.dispatch(mount_command(spec("rt-bare")));
    assert_eq!(noted(&bare, "MOUNT REFUSED: `rt_bare` reads the input"), 1);
}

/// **A stale input holds OPENING orders and never a reduce**: once the 4h input's newest bar closed more than
/// two of its bars before the decision, a buy is refused by name and reaches no venue while a covered sell
/// does; the stretch is said once, and once again when a fresh bar ends it.
#[test]
fn a_stale_input_holds_opening_orders_and_passes_a_reduce() {
    let Rig { mut core, orders, denied, .. } = rig(stored_higher());
    // Fresh at 08:00 (the newest 4h bar closes then): the buy reaches the venue and fills.
    orders.lock().unwrap().push((1, 2.0));
    close(&mut core, own_key(), bar(7 * H, 100.0));
    assert_eq!(core.engine.client.submissions.len(), 1, "a fresh input holds nothing");
    let order_client_id = order_client_id_of(&core, 0);
    core.bus.publish(
        Event::Fill(order_client_id_fill(&order_client_id, 1, 2.0, 100.0)),
        &mut core.engine,
    );
    core.dispatch_applied_fills();

    // At 17:00 the newest bar closed nine hours ago, more than two 4h bars: the buy is held, the sell passes.
    orders.lock().unwrap().extend([(1, 1.0), (-1, 1.0)]);
    close(&mut core, own_key(), bar(16 * H, 101.0));
    core.dispatch_applied_fills();
    let sent = &core.engine.client.submissions;
    assert_eq!(sent.len(), 2, "only the reduce reached the venue: {sent:?}");
    assert_eq!(sent[1].side, -1);
    assert_eq!(noted(&core, "INPUT STALE: `reads` reads `sim:BTCUSDT:4h`"), 1);
    let held = denied.lock().unwrap().clone();
    assert_eq!(held.len(), 1, "the held buy is refused, and heard by its strategy: {held:?}");
    assert!(held[0].starts_with("input-stale: the input `sim:BTCUSDT:4h`"), "{held:?}");

    // Still stale an hour later: held again, said no more.
    orders.lock().unwrap().push((1, 1.0));
    close(&mut core, own_key(), bar(17 * H, 102.0));
    assert_eq!(core.engine.client.submissions.len(), 2);
    assert_eq!(noted(&core, "INPUT STALE"), 1, "said once a stretch");

    // A fresh 4h bar (opened at 12:00, closing at 16:00) ends the stretch: said once, the buy goes.
    close(&mut core, higher_key(), bar(3 * H4, 40.0));
    orders.lock().unwrap().push((1, 1.0));
    close(&mut core, own_key(), bar(18 * H, 103.0));
    assert_eq!(core.engine.client.submissions.len(), 3, "fresh again, the buy reaches the venue");
    assert_eq!(noted(&core, "INPUT FRESH: `reads` reads `sim:BTCUSDT:4h` fresh again"), 1);
}

/// **A restart reads every input again**, from the store as it stands then: nothing of an input is kept
/// across a restart, and building the core again cancels no order.
#[test]
fn a_restart_reads_every_input_again() {
    let first = rig(stored_higher());
    assert_eq!(first.asked.lock().unwrap().len(), 2);
    drop(first);

    // The store grew while the node was down: the restarted mount reads the 4h bar opened at 8h.
    let mut grown = stored_higher();
    grown.push(bar(2 * H4, 30.0));
    let Rig { mut core, seen, asked, .. } = rig(grown);
    assert_eq!(*asked.lock().unwrap(), vec![HIGHER.to_string(), LANE.to_string()], "read again");
    close(&mut core, own_key(), bar(11 * H, 100.0));
    assert_eq!(seen.lock().unwrap()[0], (11 * H, Some(30.0), None));
    assert!(core.engine.client.cancels.is_empty(), "a restart cancels no order");
}

/// A rig of two mounts on the traded series: `reads` reads the 4h input, `plain` reads none. What each decision
/// of `plain` read is in the second `Seen`.
fn shared_rig() -> (Rig, Seen) {
    let (seen, orders, asked) = (Seen::default(), Arc::default(), Arc::default());
    let plain_seen = Seen::default();
    let strategy = reader(&[HIGHER, LANE], &seen, &orders);
    let denied = Arc::clone(&strategy.denied);
    let mut core = core_with(CoreConfig {
        seed_cash: 10_000.0,
        strategy: Some(mount(Box::new(strategy), "reads")),
        extra_mounts: vec![mount(Box::new(reader(&[], &plain_seen, &Arc::default())), "plain")],
        input_opener: Some(opener(&asked, stored_higher())),
        clock: Box::new(|| 0_i64),
        ..CoreConfig::default()
    });
    core.engine.collect_applied_fills = true;
    (Rig { core, seen, orders, denied, asked }, plain_seen)
}

/// **A decision at a close it SHARES with an input bar waits for that bar** (decision 0271, its limit (a)
/// closed): at 12:00 the 4h bar closing then has not arrived, so the reading mount's step is held while the bar's
/// fills, mark and the other mount's step go ahead; the input bar arriving releases it, and it reads that bar
/// where it would have read the one before (the backtest reads the new one). Nothing is said.
#[test]
fn a_decision_at_a_shared_close_waits_for_the_input_bar_closing_then() {
    let (Rig { mut core, seen, orders, .. }, plain_seen) = shared_rig();
    orders.lock().unwrap().push((1, 1.0));
    close(&mut core, own_key(), bar(11 * H, 100.0));
    assert!(seen.lock().unwrap().is_empty(), "the reading mount's step is held");
    assert_eq!(
        plain_seen.lock().unwrap().len(),
        1,
        "the mount that reads no input stepped at once"
    );
    assert_eq!(core.shared_closes.len(), 1);
    assert!(
        core.bars[&own_key()].closed.iter().any(|b| b.timestamp == 11 * H),
        "the bar joined the series"
    );
    assert!(core.engine.client.submissions.is_empty(), "the held step placed nothing yet");

    close(&mut core, higher_key(), bar(2 * H4, 30.0));
    assert_eq!(
        *seen.lock().unwrap(),
        vec![(11 * H, Some(30.0), None)],
        "it read the bar closing then"
    );
    assert!(core.shared_closes.is_empty());
    assert_eq!(core.engine.client.submissions.len(), 1, "its order went out at the release");
    assert_eq!(noted(&core, "SHARED CLOSE TIMEOUT"), 0, "an arrival is not said");

    // The next decisions share no close with the input: nothing waits.
    close(&mut core, own_key(), bar(12 * H, 101.0));
    assert_eq!(seen.lock().unwrap().len(), 2);
    assert!(core.shared_closes.is_empty());
}

/// **An input bar that does not come within the grace**: the held step goes ahead at the boundary once
/// `SHARED_CLOSE_GRACE_MS` passed (not a millisecond before), on the input bar before it, said ONCE on the ring;
/// the late bar then steps nothing again.
#[test]
fn a_shared_close_whose_input_bar_does_not_come_goes_ahead_after_the_grace_and_says_so() {
    let (Rig { mut core, seen, orders, .. }, _) = shared_rig();
    orders.lock().unwrap().push((1, 1.0));
    close(&mut core, own_key(), bar(11 * H, 100.0));
    core.release_due_shared_closes(crate::SHARED_CLOSE_GRACE_MS - 1);
    assert!(seen.lock().unwrap().is_empty(), "still held a millisecond before the grace ends");

    core.release_due_shared_closes(crate::SHARED_CLOSE_GRACE_MS);
    assert_eq!(*seen.lock().unwrap(), vec![(11 * H, Some(20.0), None)], "it read the bar before");
    assert_eq!(core.engine.client.submissions.len(), 1, "its order went out at the timeout");
    assert_eq!(
        noted(
            &core,
            "SHARED CLOSE TIMEOUT: `reads` decided at 43200000 ms without `sim:BTCUSDT:4h`'s bar"
        ),
        1
    );

    close(&mut core, higher_key(), bar(2 * H4, 30.0));
    core.release_due_shared_closes(10 * crate::SHARED_CLOSE_GRACE_MS);
    assert_eq!(seen.lock().unwrap().len(), 1, "the late bar steps nothing again");
    assert_eq!(noted(&core, "SHARED CLOSE TIMEOUT"), 1, "said once");
}

/// **The mount's next bar arriving before the input's** releases the held step FIRST (a bar shorter than the
/// grace): the two decisions run in order, and the held one is said as a timeout.
#[test]
fn a_held_step_runs_before_the_mounts_next_bar() {
    let (Rig { mut core, seen, .. }, _) = shared_rig();
    close(&mut core, own_key(), bar(11 * H, 100.0));
    close(&mut core, own_key(), bar(12 * H, 101.0));
    assert_eq!(
        *seen.lock().unwrap(),
        vec![(11 * H, Some(20.0), None), (12 * H, Some(20.0), None)],
        "the held decision first, then the next"
    );
    assert!(core.shared_closes.is_empty());
    assert_eq!(noted(&core, "SHARED CLOSE TIMEOUT: `reads` decided at 43200000 ms"), 1);
}

/// A core mounting one reader of `name` (the FX inputs), its strategy's denials in the rig.
fn fx_rig(name: &str) -> Rig {
    let (seen, orders, asked) = (Seen::default(), Arc::default(), Arc::default());
    let strategy = reader(&[name], &seen, &orders);
    let denied = Arc::clone(&strategy.denied);
    let mut core = core_with(CoreConfig {
        seed_cash: 10_000.0,
        strategy: Some(mount(Box::new(strategy), "fx")),
        input_opener: Some(opener(&asked, stored_higher())),
        clock: Box::new(|| 0_i64),
        ..CoreConfig::default()
    });
    core.engine.collect_applied_fills = true;
    Rig { core, seen, orders, denied, asked }
}

/// **A market's CLOSED time is not a missing bar** (an input with a trading calendar): over the FX weekend the
/// input's newest bar closed at Friday's close, far more than two of its bars before a Saturday decision, and
/// still holds nothing (the plain rule would hold the buy). The week's first hour, late at the shared close it
/// was due at (23:00 Sunday), holds nothing either: only two of its bars of OPEN market (Monday 00:00) make it
/// missing, and the next buy after that is held by name.
#[test]
fn an_input_with_a_calendar_is_not_stale_over_its_markets_closed_time() {
    let Rig { mut core, orders, denied, .. } = fx_rig(FX);
    orders.lock().unwrap().push((1, 1.0));
    close(&mut core, own_key(), bar(FRI + DAY + 11 * H, 100.0)); // decides Saturday 12:00
    assert_eq!(core.engine.client.submissions.len(), 1, "closed time holds nothing");
    assert_eq!(noted(&core, "INPUT STALE"), 0);

    // Sunday 23:00: the hour that opened at the week's open (22:00) closed, and no bar of it came.
    orders.lock().unwrap().push((1, 1.0));
    close(&mut core, own_key(), bar(FRI + 2 * DAY + 22 * H, 101.0));
    assert_eq!(core.shared_closes.len(), 1, "the shared close at 23:00 waits for it");
    core.release_due_shared_closes(i64::MAX); // its grace passes
    assert_eq!(core.engine.client.submissions.len(), 2, "a late bar holds nothing");
    orders.lock().unwrap().push((1, 1.0));
    close(&mut core, own_key(), bar(FRI + 2 * DAY + 23 * H, 102.0)); // Monday 00:00: two open hours
    assert_eq!(core.engine.client.submissions.len(), 3, "not more than two of its bars yet");
    assert_eq!(noted(&core, "INPUT STALE"), 0);

    orders.lock().unwrap().push((1, 1.0));
    close(&mut core, own_key(), bar(FRI + 3 * DAY, 103.0)); // Monday 01:00
    core.dispatch_applied_fills(); // the refusal's `Denied` reaches the strategy
    assert_eq!(core.engine.client.submissions.len(), 3, "the missing bar holds the buy");
    let held = denied.lock().unwrap().clone();
    assert!(
        held[0].starts_with(&format!(
            "input-stale: the input `fxcm:EURUSD:1h` is missing its bar closing at {} ms",
            FRI + 2 * DAY + 23 * H
        )),
        "{held:?}"
    );
    assert_eq!(noted(&core, "INPUT STALE: `fx` reads `fxcm:EURUSD:1h`, whose bar closing at"), 1);
}

/// **A bar that is only LATE holds nothing; a MISSING one holds, by name** (an input with a calendar, its market
/// open): on a Thursday the hour closing at 12:00 has not come by a decision at 13:00, which buys (the plain rule's
/// two bars); at 14:00 two of its bars of open market passed, and the buy is held. The hour after it then arrives
/// past the hole, which is said once, and the input is fresh again.
#[test]
fn a_lost_bar_of_an_input_with_a_calendar_holds_once_it_is_missing_and_its_hole_is_said() {
    let Rig { mut core, orders, denied, .. } = fx_rig(FX_THU);
    orders.lock().unwrap().push((1, 1.0));
    close(&mut core, own_key(), bar(FRI - DAY + 12 * H, 100.0)); // decides Thursday 13:00
    assert_eq!(core.engine.client.submissions.len(), 1, "a late bar holds nothing");
    orders.lock().unwrap().push((1, 1.0));
    close(&mut core, own_key(), bar(FRI - DAY + 13 * H, 101.0)); // decides 14:00
    core.dispatch_applied_fills();
    assert_eq!(core.engine.client.submissions.len(), 1, "the missing bar holds the buy");
    let held = denied.lock().unwrap().clone();
    assert!(held[0].contains("is missing its bar closing at"), "{held:?}");

    close(&mut core, fx_key("GBPUSD"), bar(FRI - DAY + 12 * H, 1.3)); // the hour 12:00-13:00, past the hole
    assert_eq!(noted(&core, "INPUT GAP: `fx` reads `fxcm:GBPUSD:1h`, which has no bar from"), 1);
    orders.lock().unwrap().push((1, 1.0));
    close(&mut core, own_key(), bar(FRI - DAY + 14 * H, 102.0)); // decides 15:00, its input fresh
    assert_eq!(core.engine.client.submissions.len(), 2, "fresh again, the buy goes");
    assert_eq!(noted(&core, "INPUT GAP"), 1, "said once");
}

/// **A decision BEHIND its input's feed** (the input's next hour already filed, the hour before it never): the bar
/// the decision reads as its newest is missing, because a later one came, so its buy is held at once; the decision
/// that reads the later bar is fresh.
#[test]
fn a_bar_a_later_one_came_past_holds_the_decision_that_should_read_it() {
    let Rig { mut core, orders, denied, .. } = fx_rig(FX_THU);
    close(&mut core, fx_key("GBPUSD"), bar(FRI - DAY + 12 * H, 1.3)); // 12:00-13:00; 11:00-12:00 never came
    assert_eq!(noted(&core, "INPUT GAP"), 1);
    orders.lock().unwrap().push((1, 1.0));
    close(&mut core, own_key(), bar(FRI - DAY + 11 * H, 100.0)); // decides 12:00
    core.dispatch_applied_fills();
    assert!(core.engine.client.submissions.is_empty(), "the bar it should read is missing");
    let held = denied.lock().unwrap().clone();
    assert!(
        held[0].contains(&format!("is missing its bar closing at {} ms", FRI - DAY + 12 * H)),
        "{held:?}"
    );

    orders.lock().unwrap().push((1, 1.0));
    close(&mut core, own_key(), bar(FRI - DAY + 12 * H, 101.0)); // decides 13:00, reads 12:00-13:00
    assert_eq!(core.engine.client.submissions.len(), 1, "fresh, the buy goes");
}
