//! **The lean journal (decision 0127, amended 2026-11-19, the owner's "do all"): a strategy's `Modify` the core acts on
//! in no way is not journaled, and the journal still rebuilds the state the full one did.** White-box: one busy
//! maker session on a bybit engine (a native amend venue, as the CI box's maker), recorded twice, once journaling every
//! `Modify` (`CoreThread::journal_every_modify`, the journal before the amendment) and once lean. Per round the maker
//! re-prices its quote six times before the venue accepts it, three times while it rests (those reach the venue),
//! six times after it ended and once under an id no engine holds; a fill lands every third round. The engine drops
//! the thirteen; the lean journal leaves exactly those out, and both journals restore (the boot refold of the whole
//! tail) and replay (the fenced offline replay) to the live state: the same digest, positions and orders.

use super::*;
use crate::replay::{RestoreOutcome, replay_offline, restore_for_boot};
use crate::runtime::test_support::core_of;
use crate::scratch::Scratch;
use vike_execution::testing::{RecordingClient, engine_on};
use vike_execution::{EngineSnapshot, OrderIntent};
use vike_journal::{Journal, JournalFileConfig, JournalRecord};
use vike_model::events::{
    Event, FillEvent, OrderAccepted, OrderCanceled, OrderModified, OrderSubmitted, PositionSide,
    TradeId,
};

const VENUE: &str = "bybit";
const SYMBOL: &str = "BTCUSDT.P";
const ROUNDS: u32 = 40;
/// The re-prices per round the engine drops (6 before the accept, 6 after the end, 1 of no order) and the ones it
/// sends (3 while the quote rests).
const INERT_PER_ROUND: usize = 13;
const SENT_PER_ROUND: usize = 3;

fn journal(dir: &Scratch) -> JournalConfig {
    JournalConfig {
        dir: dir.to_path_buf(),
        file: JournalFileConfig { segment_bytes: 4 << 20, flush_every: 64 },
        // no cadence `Snapshot` inside the session: the refold covers the whole tail
        snapshot_every: u64::MAX,
    }
}

/// A strategy that does nothing on its own: the test lowers its intents.
struct Idle;

impl Strategy<LiveBroker> for Idle {}

fn mounted(dir: &Scratch) -> CoreThread<RecordingClient> {
    let mount = crate::testkit::mount_of(VENUE, SYMBOL, "1m", Box::new(Idle));
    let mut c = core_of(
        engine_on(VENUE, SYMBOL, RecordingClient::default()),
        Vec::new(),
        CoreConfig {
            seed_cash: 10_000.0,
            // every dispatch at one stamp, so the two recordings fold alike
            clock: Box::new(|| 5_000_i64),
            order_client_id_session: Some(("cafef00d".into(), 0)),
            strategy: Some(mount),
            journal: Some(journal(dir)),
            ..CoreConfig::default()
        },
    );
    c.engine.collect_applied_fills = true;
    c
}

fn modify(order_client_id: &str, price: f64) -> OrderIntent {
    OrderIntent::Modify {
        order_client_id: order_client_id.into(),
        new_qty: None,
        new_price: Some(price),
        new_trigger_price: None,
    }
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

/// The session, `full` journaling every `Modify`. Returns the core, before any exit `Snapshot`.
fn session(dir: &Scratch, full: bool) -> CoreThread<RecordingClient> {
    let mut c = mounted(dir);
    c.journal_every_modify = full;
    c.write_snap(1); // the restore's base
    let mut t = 10;
    let mut tick = || {
        t += 1;
        t
    };
    for round in 0..ROUNDS {
        let price = 100.0 - f64::from(round % 7) * 0.5;
        let id = c.apply_strategy_intent(bid(price), tick(), 0)[0].clone();
        for k in 0..6 {
            c.apply_strategy_intent(modify(&id, price - 0.1 * f64::from(k)), tick(), 0);
        }
        c.dispatch(Ingest::Event(Event::OrderSubmitted(OrderSubmitted {
            order_client_id: id.clone(),
            timestamp: tick(),
        })));
        c.dispatch(Ingest::Event(Event::OrderAccepted(OrderAccepted {
            order_client_id: id.clone(),
            venue_order_id: Some(format!("v{round}").into()),
            timestamp: tick(),
        })));
        for k in 0..SENT_PER_ROUND {
            c.apply_strategy_intent(modify(&id, price + 0.1 * k as f64), tick(), 0);
        }
        c.dispatch(Ingest::Event(Event::OrderModified(OrderModified {
            order_client_id: id.clone(),
            venue_order_id: None,
            new_qty: None,
            new_price: Some(price + 0.2),
            new_trigger_price: None,
            timestamp: tick(),
        })));
        if round % 3 == 0 {
            let at = tick();
            c.dispatch(Ingest::Event(Event::Fill(FillEvent {
                trade_id: TradeId::prefixed("t-", &id),
                order_client_id: id.clone(),
                venue: VENUE.into(),
                symbol: SYMBOL.into(),
                side: 1,
                last_qty: 0.4,
                last_price: price + 0.2,
                commission: 0.01,
                commission_asset: "USDT".into(),
                liquidity_side: "maker".to_string().into(),
                timestamp: at,
                mark_price: None,
                position_side: PositionSide::Both,
            })));
            c.dispatch_applied_fills();
        }
        c.apply_strategy_intent(OrderIntent::Cancel(id.clone()), tick(), 0);
        c.dispatch(Ingest::Event(Event::OrderCanceled(OrderCanceled {
            order_client_id: id.clone(),
            reason: "venue".to_string().into(),
            timestamp: tick(),
            post_only_would_cross: false,
            refusal_id: None,
        })));
        for _ in 0..6 {
            c.apply_strategy_intent(modify(&id, price), tick(), 0);
        }
        c.apply_strategy_intent(modify("held-by-no-engine", price), tick(), 0);
    }
    c
}

/// The strategy `Modify` records of the journal at `dir`.
fn modifies(dir: &Scratch) -> usize {
    Journal::read_all(dir)
        .unwrap()
        .iter()
        .filter(|r| {
            matches!(r, JournalRecord::StrategySubmit { intent: OrderIntent::Modify { .. }, .. })
        })
        .count()
}

/// The journal at `dir` WITHOUT its strategy `Modify` records, each record as JSON with its sequence numbers taken
/// out (the lean journal numbers its records on without the ones it left out).
fn the_rest(dir: &Scratch) -> Vec<serde_json::Value> {
    fn unnumbered(v: &mut serde_json::Value) {
        match v {
            serde_json::Value::Object(map) => {
                map.retain(|k, _| k != "sequence" && k != "caused_by_sequence");
                map.values_mut().for_each(unnumbered);
            }
            serde_json::Value::Array(items) => items.iter_mut().for_each(unnumbered),
            _ => {}
        }
    }
    Journal::read_all(dir)
        .unwrap()
        .iter()
        .filter(|r| {
            !matches!(r, JournalRecord::StrategySubmit { intent: OrderIntent::Modify { .. }, .. })
        })
        .map(|r| {
            let mut v = serde_json::to_value(r).unwrap();
            unnumbered(&mut v);
            v
        })
        .collect()
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

/// **A crash restore of the lean journal rebuilds what one of the full journal does**, and both the live state: the
/// lean journal is the full one less exactly the thirteen re-prices per round the engine dropped, record for record.
#[test]
fn the_lean_journal_restores_the_state_the_full_one_does() {
    let (full_dir, lean_dir) =
        (Scratch::reserved("vco-lean-full"), Scratch::reserved("vco-lean-lean"));
    let (full, lean) = (session(&full_dir, true), session(&lean_dir, false));
    let live = full.engine.snapshot_state();
    assert_eq!(state(&live), state(&lean.engine.snapshot_state()), "the live runs are alike");
    assert!(live.account.positions.iter().any(|(_, p)| p.size > 0.0), "premise: fills folded");
    drop((full, lean)); // a crash: no exit `Snapshot`

    let per_round = INERT_PER_ROUND + SENT_PER_ROUND;
    assert_eq!(
        modifies(&full_dir),
        ROUNDS as usize * per_round,
        "the full journal: every re-price"
    );
    assert_eq!(
        modifies(&lean_dir),
        ROUNDS as usize * SENT_PER_ROUND,
        "the lean one: the sent ones"
    );
    assert_eq!(the_rest(&lean_dir), the_rest(&full_dir), "and nothing else differs");

    for dir in [&full_dir, &lean_dir] {
        let tmp = Scratch::created("vco-lean-restore-tmp");
        let restore = restore_for_boot(Some(&journal(dir)), &tmp, 5_000, &[]).unwrap();
        assert_eq!(restore.outcome(), &RestoreOutcome::Full, "{:?}", restore.notes());
        assert_eq!(state(&restore.restored_engines()[0]), state(&live), "the live state rebuilt");
    }
}

/// **The fenced offline replay of the lean journal reproduces the full one's digest**, each journal's own exit
/// `Snapshot` its fence.
#[test]
fn the_lean_journal_replays_to_the_full_ones_digest() {
    let (full_dir, lean_dir) =
        (Scratch::reserved("vco-lean-replay-full"), Scratch::reserved("vco-lean-replay-lean"));
    let mut hashes = Vec::new();
    for (dir, full) in [(&full_dir, true), (&lean_dir, false)] {
        let mut c = session(dir, full);
        let live = c.engine.snapshot_state();
        c.write_snap(9_000);
        drop(c);
        let out = replay_offline(dir).unwrap_or_else(|e| panic!("full={full}: the fence: {e}"));
        assert_eq!(out.final_hash, vike_execution::state_hash(std::slice::from_ref(&live)));
        hashes.push(out.final_hash);
    }
    assert_eq!(hashes[0], hashes[1], "one digest, whichever journal");
}
