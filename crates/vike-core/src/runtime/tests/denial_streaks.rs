//! **Repeated gate denials of one strategy order are COUNTED in the journal, not written three records each**
//! (`crates/vike-core/src/runtime/denial_streaks.rs`, journal format 35). White-box: a mounted strategy on a bybit
//! engine keeps sending a buy the gate refuses (the engine is halted by a journaled command), as the CI box's maker re-sent a
//! quote over its exposure ceiling. Pinned: ten thousand denials journal a handful of records while the live core
//! still mints, lists and delivers each one; a crash before the counts are written restores a session that never
//! mints a counted denial's id again; and a session of counted denials restores and replays to exactly the live state,
//! as the same session journaled whole (the journal before format 35, still 34) does.

use std::collections::HashSet;

use super::*;
use crate::replay::{RestoreOutcome, replay_offline, restore_for_boot};
use crate::runtime::denial_streaks::DENIAL_RANGE;
use crate::runtime::test_support::core_of;
use crate::scratch::Scratch;
use vike_execution::EngineSnapshot;
use vike_execution::testing::{RecordingClient, engine_on};
use vike_journal::{Journal, JournalFileConfig, JournalRecord};
use vike_model::events::{FillEvent, OrderAccepted, OrderSubmitted, PositionSide, TradeId};
use vike_model::orders::client_id::OrderClientIdGenerator;

const VENUE: &str = "bybit";
const SYMBOL: &str = "BTCUSDT.P";

fn journal(dir: &Scratch) -> JournalConfig {
    JournalConfig {
        dir: dir.to_path_buf(),
        file: JournalFileConfig { segment_bytes: 4 << 20, flush_every: 64 },
        // no cadence `Snapshot` inside a session: the counts are written by the test's own
        snapshot_every: u64::MAX,
    }
}

/// A strategy that does nothing on its own: the test lowers its intents.
struct Idle;

impl Strategy<LiveBroker> for Idle {}

fn config(dir: &Scratch, session: Option<&str>) -> CoreConfig {
    CoreConfig {
        seed_cash: 10_000.0,
        clock: Box::new(|| 5_000_i64),
        order_client_id_session: session.map(|s| (s.to_string(), 0)),
        strategy: Some(crate::testkit::mount_of(VENUE, SYMBOL, "1m", Box::new(Idle))),
        journal: Some(journal(dir)),
        ..CoreConfig::default()
    }
}

fn core(config: CoreConfig) -> CoreThread<RecordingClient> {
    let mut c = core_of(engine_on(VENUE, SYMBOL, RecordingClient::default()), Vec::new(), config);
    c.engine.collect_applied_fills = true;
    c
}

/// A fresh v2 order client id session, the shape a live node mints under.
fn v2_session() -> String {
    OrderClientIdGenerator::new_v2(None).state().0
}

fn bid(price: f64) -> OrderIntent {
    OrderIntent::Submit(Box::new(vike_model::OrderRequest {
        venue: VENUE.into(),
        symbol: SYMBOL.into(),
        side: 1,
        qty: 1.0,
        order_type: vike_model::OrderType::Limit,
        price: Some(price),
        ..Default::default()
    }))
}

/// Halt or resume trading through the journaled door, so a refold sees it too.
fn trading(c: &mut CoreThread<RecordingClient>, state: TradingState) {
    c.dispatch(Ingest::Command(Command::SetTradingState(state)));
}

/// The strategy's `n` bids the halted engine refuses, at `now..`: the ids it was handed.
fn denied(c: &mut CoreThread<RecordingClient>, n: i64, now: i64) -> Vec<String> {
    (0..n).map(|t| c.apply_strategy_intent(bid(100.0), now + t, 0).remove(0)).collect()
}

/// `(StrategySubmit, MintedSubmit, ExecutionReport of one of `ids`, DeniedRepeat records, their counts summed)`.
fn kinds(dir: &Scratch, ids: &HashSet<String>) -> (usize, usize, usize, usize, u64) {
    let mut out = (0, 0, 0, 0, 0);
    for r in Journal::read_all(dir).unwrap() {
        match r {
            JournalRecord::StrategySubmit { .. } => out.0 += 1,
            JournalRecord::MintedSubmit { .. } => out.1 += 1,
            JournalRecord::ExecutionReport { report, .. }
                if ids.contains(&report.order_client_id) =>
            {
                out.2 += 1;
            }
            JournalRecord::DeniedRepeat { repeat, .. } => {
                out.3 += 1;
                out.4 += repeat.count;
            }
            _ => {}
        }
    }
    out
}

/// **Ten thousand identical denials journal a handful of records, not thirty thousand**: the first is journaled
/// whole (its `StrategySubmit`, `MintedSubmit` and `Denied` report), the rest are counted in two `DeniedRepeat`s
/// (the range's reservation and the count the `Snapshot` writes). Live, nothing changed: each one got an id of its
/// own, was listed for the alert and reached its strategy, and none reached the venue.
#[test]
fn ten_thousand_identical_denials_journal_a_handful_of_records() {
    let dir = Scratch::reserved("vco-denials-count");
    let mut c = core(config(&dir, Some(&v2_session())));
    c.write_snap(1);
    trading(&mut c, TradingState::Halted);
    let n = 10_000;
    let ids: HashSet<String> = denied(&mut c, n, 10).into_iter().collect();

    assert_eq!(ids.len(), n as usize, "every denial has an id of its own");
    assert!(c.engine.client.submissions.is_empty(), "nothing reached the venue");
    let listed = c.denied_orders.as_deref().unwrap_or_default();
    assert_eq!(listed.last().map(|d| d.sequence), Some(n as u64), "each one listed for the alert");
    let heard = c
        .engine
        .order_events
        .iter()
        .filter(|e| matches!(e.event.kind, vike_model::OrderEventKind::Denied { .. }))
        .count();
    assert_eq!(heard, n as usize, "each one heard by its strategy");
    c.write_snap(20_000);
    drop(c);

    let (strategy, minted, reports, repeats, counted) = kinds(&dir, &ids);
    assert_eq!((strategy, minted, reports), (1, 1, 1), "the first denial journaled whole");
    assert_eq!(counted, n as u64 - 1, "every other one counted");
    assert_eq!(repeats, 2, "in the range's reservation and the count before the Snapshot");
    assert!(Journal::read_all(&dir).unwrap().len() < 10, "a handful of records");
    assert_eq!(Journal::latest_segment_version(&dir).unwrap(), Some(35), "the format-35 door");
}

/// **A crash before the counts are written never mints a counted denial's id again**: the range the counted ids came
/// from was journaled before the first of them, so the boot restore resumes the session past it, and the restarted
/// core's orders, admitted or denied, all get new ids.
#[test]
fn a_restart_after_a_crash_never_mints_a_counted_denials_id_again() {
    let dir = Scratch::reserved("vco-denials-crash");
    let session = v2_session();
    let mut c = core(config(&dir, Some(&session)));
    c.write_snap(1);
    trading(&mut c, TradingState::Halted);
    let mut handed: HashSet<String> = denied(&mut c, 50, 10).into_iter().collect();
    drop(c); // a crash: no exit `Snapshot`, the count of the last 49 never written

    let tmp = Scratch::created("vco-denials-crash-tmp");
    let restore = restore_for_boot(Some(&journal(&dir)), &tmp, 5_000, &[]).unwrap();
    assert_eq!(restore.outcome(), &RestoreOutcome::Full, "{:?}", restore.notes());
    let mut booted = config(&dir, None);
    restore.apply_to(&mut booted);
    let mut c = core(booted);
    assert_eq!(c.order_client_id_gen.state().0, session, "premise: the session was resumed");
    assert!(
        c.order_client_id_gen.sequence() > DENIAL_RANGE,
        "past the range: {}",
        c.order_client_id_gen.sequence()
    );

    trading(&mut c, TradingState::Halted);
    let again = denied(&mut c, 50, 1_000);
    trading(&mut c, TradingState::Active);
    let admitted: Vec<String> =
        (0..5).map(|t| c.apply_strategy_intent(bid(99.0), 2_000 + t, 0).remove(0)).collect();
    assert_eq!(c.engine.client.submissions.len(), 5, "premise: the bids were sent");
    for id in again.into_iter().chain(admitted) {
        assert!(handed.insert(id.clone()), "{id} was handed out before the crash");
    }
}

/// What a test compares of an engine: the digest, the positions and the orders (id, status, price, filled).
#[derive(Debug, PartialEq)]
struct State {
    digest: u64,
    positions: serde_json::Value,
    orders: Vec<(String, String, Option<f64>, f64)>,
}

fn state(e: &EngineSnapshot) -> State {
    State {
        digest: vike_execution::state_hash(std::slice::from_ref(e)),
        positions: serde_json::to_value(&e.account.positions).unwrap(),
        orders: e
            .registry
            .iter()
            .map(|(id, o)| (id.clone(), format!("{:?}", o.status), o.request.price, o.filled_qty))
            .collect(),
    }
}

const ROUNDS: u32 = 6;
const DENIED_PER_ROUND: i64 = 25;

/// The session: per round, the halted engine refuses 25 bids, trading resumes, one bid is admitted and filled. `whole`
/// journals every denial whole, as before format 35. Returns the core, before any exit `Snapshot`.
fn session(dir: &Scratch, whole: bool) -> CoreThread<RecordingClient> {
    let mut c = core(config(dir, Some("cafef00d")));
    c.journal_every_denial = whole;
    c.write_snap(1); // the restore's base
    let mut t = 10;
    for round in 0..ROUNDS {
        trading(&mut c, TradingState::Halted);
        denied(&mut c, DENIED_PER_ROUND, t);
        t += DENIED_PER_ROUND;
        trading(&mut c, TradingState::Active);
        let id = c.apply_strategy_intent(bid(99.0), t, 0).remove(0);
        c.dispatch(Ingest::Event(Event::OrderSubmitted(OrderSubmitted {
            order_client_id: id.clone(),
            timestamp: t + 1,
        })));
        c.dispatch(Ingest::Event(Event::OrderAccepted(OrderAccepted {
            order_client_id: id.clone(),
            venue_order_id: Some(format!("v{round}").into()),
            timestamp: t + 2,
        })));
        c.dispatch(Ingest::Event(Event::Fill(FillEvent {
            trade_id: TradeId::prefixed("t-", &id),
            order_client_id: id.clone(),
            venue: VENUE.into(),
            symbol: SYMBOL.into(),
            side: 1,
            last_qty: 1.0,
            last_price: 99.0,
            commission: 0.01,
            commission_asset: "USDT".into(),
            liquidity_side: "maker".to_string().into(),
            timestamp: t + 3,
            mark_price: None,
            position_side: PositionSide::Both,
        })));
        c.dispatch_applied_fills();
        t += 10;
    }
    c
}

/// **A crash restore of the counted journal rebuilds the live state, as one of the whole journal does**: the same
/// positions, and, past every range a counted denial took its id from, the same orders under the ids the live core
/// minted. The whole journal is still format 34, the one the build before it reads.
#[test]
fn the_counted_journal_restores_the_state_the_whole_one_does() {
    let (whole_dir, counted_dir) =
        (Scratch::reserved("vco-denials-whole"), Scratch::reserved("vco-denials-counted"));
    let (whole, counted) = (session(&whole_dir, true), session(&counted_dir, false));
    let (whole_live, counted_live) =
        (whole.engine.snapshot_state(), counted.engine.snapshot_state());
    assert_eq!(
        state(&whole_live).positions,
        state(&counted_live).positions,
        "the live runs hold the same book"
    );
    assert!(counted_live.account.positions.iter().any(|(_, p)| p.size > 0.0), "premise: fills");
    drop((whole, counted)); // a crash: no exit `Snapshot`

    assert_eq!(Journal::latest_segment_version(&whole_dir).unwrap(), Some(34));
    assert_eq!(Journal::latest_segment_version(&counted_dir).unwrap(), Some(35));
    let records = |dir: &Scratch| Journal::read_all(dir).unwrap().len();
    assert!(records(&counted_dir) * 4 < records(&whole_dir), "the counted journal is the smaller");

    for (dir, live) in [(&whole_dir, &whole_live), (&counted_dir, &counted_live)] {
        let tmp = Scratch::created("vco-denials-restore-tmp");
        let restore = restore_for_boot(Some(&journal(dir)), &tmp, 5_000, &[]).unwrap();
        assert_eq!(restore.outcome(), &RestoreOutcome::Full, "{:?}", restore.notes());
        assert_eq!(state(&restore.restored_engines()[0]), state(live), "the live state rebuilt");
    }
}

/// **The fenced offline replay of the counted journal reproduces the live digest**, each journal's own exit
/// `Snapshot` its fence: the replay core skips each reserved range where the live core jumped it, so it mints the
/// admitted bids' ids byte for byte.
#[test]
fn the_counted_journal_replays_to_the_live_digest() {
    for whole in [true, false] {
        let dir = Scratch::reserved("vco-denials-replay");
        let mut c = session(&dir, whole);
        let live = c.engine.snapshot_state();
        c.write_snap(9_000);
        drop(c);
        let out = replay_offline(&dir).unwrap_or_else(|e| panic!("whole={whole}: the fence: {e}"));
        assert_eq!(out.final_hash, vike_execution::state_hash(std::slice::from_ref(&live)));
    }
}
