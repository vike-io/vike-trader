//! Task 4 gate: the core journals every exec-lane message BEFORE folding it (write-ahead),
//! snapshots on cadence AND on shutdown, and the last Snap carries the coid session + the
//! `state_hash` determinism fence the T5 replay consumes. Journaling defaults off, so this is
//! the ONLY test that opens a journal — every other core test stays byte-identical.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use vike_core::{
    journal::JournalRecord, spawn_core, strategy_state::mount_id_of, CoreConfig, JournalConfig,
    LiveBroker, StrategyMount,
};
use vike_exec::testing::{RecordingClient, TestExecutionClient};
use vike_exec::{
    Account, BalanceMode, Command, ExecutionEngine, Ingest, OrderIntent, QuoteUpdate, RiskGate,
    RiskLimits,
};
use vike_model::events::{Event, FillEvent, TradeId};
use vike_model::{Broker, OrderRequest, QuoteTick, Strategy};

/// A bare external Fill event on the sim venue — the exec-lane message the core folds and
/// (with journaling on) write-ahead-journals. Fields mirror the CURRENT struct (Tier-1b:
/// `trade_id` a `TradeId` over the same CompactString, `venue`/`symbol`/`commission_asset` Ustr,
/// `liquidity_side`/`position_side` enums) — the same shape `runtime_smoke.rs::fill` uses.
fn fill(coid: &str, tid: &str, qty: f64, px: f64) -> Event {
    Event::Fill(FillEvent {
        // `&str`: the caller mints `t{i}` in a loop, so this cannot be `&'static str`.
        trade_id: TradeId::new(tid).expect("test trade ids are non-empty"),
        client_order_id: coid.into(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        last_qty: qty,
        last_px: px,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "taker".into(),
        ts: 1,
        mark_price: Some(px),
        position_side: "BOTH".into(),
    })
}

/// Six unique fills + a shutdown, with a cadence of 4: expect 6 Cmd records (write-ahead),
/// one cadence Snap (at the 4th record) + one shutdown Snap, and the tail Snap reflecting the
/// fully-folded state (all 6 trade_ids, the "deadbeef" coid session, hash == state_hash).
#[test]
fn journals_before_fold_and_snapshots_on_shutdown() {
    let dir = crate::scratch::Scratch::reserved("wire");
    let engine = ExecutionEngine::new(
        Account::new(1.0, "sim", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::default()),
        TestExecutionClient::new("sim", 100.0),
        "sim",
        "BTCUSDT",
    );
    // struct-update (not field-reassign) — the codebase clippy gate forbids reassigning fields on
    // a `Default::default()` value (see runtime_smoke.rs::test_config).
    let cfg = CoreConfig {
        clock: Box::new(vike_model::TestClock::new(1_000)),
        coid_session: Some(("deadbeef".into(), 0)),
        journal: Some(JournalConfig {
            dir: dir.to_path_buf(),
            file: vike_core::journal::JournalFileConfig {
                segment_bytes: 1024 * 1024,
                flush_every: 8,
            },
            snapshot_every: 4,
        }),
        ..CoreConfig::default()
    };
    let handle = spawn_core(engine, cfg);
    let sender = handle.event_sender();
    for i in 0..6 {
        sender.blocking_send(fill("", &format!("t{i}"), 1.0, 100.0)).unwrap();
    }
    handle.send_command(Command::Shutdown);
    handle.shutdown_and_join();

    let recs = vike_core::journal::CommandJournal::read_all(&dir).unwrap();
    let cmds = recs.iter().filter(|r| matches!(r, JournalRecord::Cmd { .. })).count();
    let snaps: Vec<_> = recs.iter().filter(|r| matches!(r, JournalRecord::Snap { .. })).collect();
    assert!(cmds >= 6, "all exec-lane messages journaled (got {cmds})");
    assert!(snaps.len() >= 2, "cadence snap + shutdown snap (got {})", snaps.len());
    // The FIRST cadence Snap (first Snap in journal order) must reflect the message that TRIGGERED
    // it, already folded. With snapshot_every = 4 the cadence snap fires right after the 4th fill
    // (t3) is FOLDED, so its engine carries 4 trade_ids (t0..t3) — NOT 3. This is the crash-recovery
    // fence: under the old before-fold bug the snap was written BEFORE folding the 4th message, so
    // it reflected only 3 fills (t0..t2) while t3 sat at a LOWER seq than the snap and would be
    // dropped on a cadence-snap restore (replay restores the snap, then applies only seq > snap).
    // Asserting 4 here is exactly what FAILS on before-fold and PASSES on the after-fold fix.
    if let JournalRecord::Snap { engines, .. } = snaps.first().unwrap() {
        assert_eq!(
            engines[0].seen_trade_ids.len(),
            4,
            "first cadence snap must reflect the 4th (just-folded) fill, not 3"
        );
    } else {
        unreachable!("filtered to Snap records");
    }
    if let JournalRecord::Snap { engines, coid_session, hash, .. } = snaps.last().unwrap() {
        assert_eq!(coid_session, "deadbeef");
        assert_eq!(*hash, vike_exec::state_hash(engines));
        assert!(engines[0].seen_trade_ids.len() == 6);
    } else {
        unreachable!("filtered to Snap records");
    }
}

/// A resting limit buy on the sim venue — the intent that, once folded, drives the paper/test
/// client to synthesize its Submitted → Accepted → Fill → Filled lifecycle.
fn order_req(coid: &str) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.into(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 1.0,
        order_type: "limit".into(),
        price: Some(100.0),
        ..Default::default()
    }
}

/// SINK-ENABLEMENT gate: a paper/test client synthesizes its order lifecycle (Submitted → Accepted
/// → Fill → Filled) INSIDE the submit dispatch and returns it via `poll_events` — the `pump_client`
/// path, which bypasses `dispatch`'s write-ahead journal site. Historically those events never
/// reached the journal (documented in `replay_fence.rs`'s module doc), so a paper session's fill
/// stream was not durable and a live tearsheet could not be reconstructed from the journal. This
/// pins the fix: after submitting ONE order through a `TestExecutionClient`, the journal carries the
/// synthesized `Event::Fill` (and the rest of the lifecycle) as `Ingest::Event` `Cmd` records — the
/// same shape a real venue fill rides over the ingest lane.
#[test]
fn paper_synthesized_fill_is_journaled() {
    let dir = crate::scratch::Scratch::reserved("paper-fill");
    let engine = ExecutionEngine::new(
        Account::new(1.0, "sim", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::default()),
        TestExecutionClient::new("sim", 100.0),
        "sim",
        "BTCUSDT",
    );
    let cfg = CoreConfig {
        seed_cash: 10_000.0,
        clock: Box::new(vike_model::TestClock::new(1_000)),
        coid_session: Some(("deadbeef".into(), 0)),
        journal: Some(JournalConfig {
            dir: dir.to_path_buf(),
            file: vike_core::journal::JournalFileConfig {
                segment_bytes: 1024 * 1024,
                flush_every: 8,
            },
            snapshot_every: 1_000, // large: keep the scenario to just the shutdown Snap
        }),
        ..CoreConfig::default()
    };
    let handle = spawn_core(engine, cfg);
    // The lifecycle the client synthesizes for this order is drained by `pump_client` at teardown
    // (the submit dispatch itself does not pump), which is BEFORE the journal is closed.
    handle.send_command(Command::Order(OrderIntent::Submit(Box::new(order_req("op1")))));
    handle.shutdown_and_join();

    let recs = vike_core::journal::CommandJournal::read_all(&dir).unwrap();
    let fills: Vec<&FillEvent> = recs
        .iter()
        .filter_map(|r| match r {
            JournalRecord::Cmd { msg: Ingest::Event(Event::Fill(f)), .. } => Some(f),
            _ => None,
        })
        .collect();
    assert!(
        fills.iter().any(|f| f.client_order_id.as_str() == "op1"),
        "the paper-synthesized fill for op1 must be journaled as an Ingest::Event Cmd record \
         (got {} Fill record(s) among {} journal record(s))",
        fills.len(),
        recs.len()
    );
}

const VENUE: &str = "sim";
const SYMBOL: &str = "BTCUSDT";
const INTERVAL: &str = "1m";

/// Submits a 1-lot market buy on every quote tick — mirrors `runtime::tests::AlwaysSubmitStrategy`
/// / `readiness_gate_wiring.rs`'s copy, rewritten here against only the public
/// `vike_model`/`vike_core` surface (the runtime-internal original is not exported).
struct AlwaysSubmitStrategy {
    calls: Arc<AtomicUsize>,
}

impl Strategy<LiveBroker> for AlwaysSubmitStrategy {
    fn on_quote_tick(&mut self, broker: &mut LiveBroker, _q: &QuoteTick) {
        self.calls.fetch_add(1, Ordering::Relaxed);
        broker.submit_market(SYMBOL, 1, 1.0);
    }
}

fn quote_tick(ts: i64) -> QuoteTick {
    QuoteTick {
        ts,
        local_ts: 0,
        bid: 99.0,
        ask: 101.0,
        bid_size: 1.0,
        ask_size: 1.0,
        symbol: SYMBOL.into(),
    }
}

/// Portfolio-observer PR-5 T2 gate: a mounted strategy's buffered submit, drained through
/// `drain_broker` -> `apply_strategy_intent`, is journaled WRITE-AHEAD as a
/// `JournalRecord::StrategySubmit` (the mount's derived `mount_id_of` + the already-resolved
/// `OrderIntent`) BEFORE it ever reaches `apply_intent` — the same write-ahead discipline
/// `dispatch()`'s `Cmd` gate applies to every `Command`/`Event`. Must run through a REAL
/// spawned-thread core (`spawn_core`) so `drain_broker` actually executes: a synchronous
/// `CoreThread` built directly, as the runtime-internal unit tests do, never runs the `run()` fold
/// loop that drives a live quote tick through `drive_strategy_tick` -> `drain_broker`.
#[test]
fn drain_broker_journals_strategy_submits() {
    let dir = crate::scratch::Scratch::reserved("strategy-submit");

    let engine = ExecutionEngine::new(
        Account::new(1.0, VENUE, None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        VENUE,
        SYMBOL,
    );
    let calls = Arc::new(AtomicUsize::new(0));
    let cfg = CoreConfig {
        seed_cash: 10_000.0,
        strategy: Some(StrategyMount {
            account: None,
            symbols: Vec::new(),
            controller_id: None,
            underlying_symbol: None,
            venue: VENUE.into(),
            symbol: SYMBOL.into(),
            interval: INTERVAL.into(),
            strategy: Box::new(AlwaysSubmitStrategy { calls: Arc::clone(&calls) }),
        }),
        journal: Some(JournalConfig {
            dir: dir.to_path_buf(),
            file: vike_core::journal::JournalFileConfig {
                segment_bytes: 1024 * 1024,
                flush_every: 8,
            },
            snapshot_every: 1_000, // large: keep this scenario to just the shutdown Snap
        }),
        ..CoreConfig::default()
    };
    let handle = spawn_core(engine, cfg);

    handle
        .tick_sender()
        .quote(QuoteUpdate { venue: VENUE.into(), symbol: SYMBOL.into(), quote: quote_tick(1) })
        .expect("core alive");

    handle.shutdown_and_join();
    assert!(calls.load(Ordering::Relaxed) >= 1, "sanity: the strategy hook must have run");

    let recs = vike_core::journal::CommandJournal::read_all(&dir).unwrap();
    let expected_mount_id = mount_id_of(VENUE, SYMBOL, INTERVAL);
    let submits: Vec<(&String, &OrderIntent)> = recs
        .iter()
        .filter_map(|r| match r {
            JournalRecord::StrategySubmit { mount_id, intent, .. } => Some((mount_id, intent)),
            _ => None,
        })
        .collect();
    assert!(
        !submits.is_empty(),
        "expected >=1 JournalRecord::StrategySubmit among {} journal record(s), got none",
        recs.len()
    );
    let (mount_id, intent) = submits[0];
    assert_eq!(mount_id, &expected_mount_id, "mount_id must be the runtime's derived mount_id_of");
    assert!(
        matches!(intent, OrderIntent::Submit(req) if req.symbol == SYMBOL && req.side == 1),
        "expected an OrderIntent::Submit for {SYMBOL}, got {intent:?}"
    );
}

/// Portfolio-observer PR-5 T4 regression guard: a `MountState::Pending` mount's discarded submit
/// must never reach the journal. Mirrors `drain_broker_journals_strategy_submits`'s spawn+journal
/// harness above, but with `CoreConfig::readiness_gate: true` and no price for the symbol yet, so
/// the mount starts `Pending` (`crates/vike-core/src/runtime/mod.rs`'s `assemble_core` seeds
/// every mount `Pending` when the gate is on). The FIRST quote both primes the price board
/// (the `set_mark` in `crates/vike-core/src/runtime/strategy_drive.rs`'s `drive_strategy_tick`)
/// AND fires the strategy hook, buffering a submit — but `drain_broker` reads
/// `self.mount_states[mount_idx]` as of the START of that SAME dispatch, which is still
/// `Pending` (the readiness boundary probe re-runs only AFTER `self.handle()` returns,
/// `crates/vike-core/src/runtime/mod.rs`'s `CoreThread::run`), so the buffered submit is
/// discarded right there, BEFORE `apply_strategy_intent` — the journal-write site — is ever
/// reached (`crates/vike-core/src/runtime/strategy_drive.rs`'s `drain_broker` doc). This
/// mirrors `readiness_gate_wiring.rs`'s
/// `readiness_gate_flips_via_real_run_loop_boundary_probe`, which asserts the same discard via
/// `handle.snapshot().orders.len() == 0`; this test adds the journal-side assertion that gap
/// leaves untested. Sending exactly ONE quote then shutting down immediately (never poking a
/// second time) keeps the mount's only buffered submit inside that discarded window — the boundary
/// probe flips the mount `Ready` only AFTER this dispatch, and no further quote ever arrives to
/// drain a second submit — so the journal must show zero `JournalRecord::StrategySubmit` records.
#[test]
fn pending_mount_discards_submit_before_it_reaches_the_journal() {
    let dir = crate::scratch::Scratch::reserved("pending-strategy-submit");

    let engine = ExecutionEngine::new(
        Account::new(1.0, VENUE, None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        VENUE,
        SYMBOL,
    );
    let calls = Arc::new(AtomicUsize::new(0));
    let cfg = CoreConfig {
        seed_cash: 10_000.0,
        readiness_gate: true,
        strategy: Some(StrategyMount {
            account: None,
            symbols: Vec::new(),
            controller_id: None,
            underlying_symbol: None,
            venue: VENUE.into(),
            symbol: SYMBOL.into(),
            interval: INTERVAL.into(),
            strategy: Box::new(AlwaysSubmitStrategy { calls: Arc::clone(&calls) }),
        }),
        journal: Some(JournalConfig {
            dir: dir.to_path_buf(),
            file: vike_core::journal::JournalFileConfig {
                segment_bytes: 1024 * 1024,
                flush_every: 8,
            },
            snapshot_every: 1_000, // large: keep this scenario to just the shutdown Snap
        }),
        ..CoreConfig::default()
    };
    let handle = spawn_core(engine, cfg);

    // The ONE quote this test sends both primes the price board and fires the strategy hook, but
    // the mount is still Pending as of THIS dispatch, so drain_broker discards the buffered submit.
    // Do not send a second quote: by then the boundary probe has flipped the mount Ready, and a
    // second submit would land + journal, which is not what this test pins.
    handle
        .tick_sender()
        .quote(QuoteUpdate { venue: VENUE.into(), symbol: SYMBOL.into(), quote: quote_tick(1) })
        .expect("core alive");
    std::thread::sleep(Duration::from_millis(200));
    assert!(calls.load(Ordering::Relaxed) >= 1, "sanity: the strategy hook must have run");
    assert_eq!(
        handle.snapshot().orders.len(),
        0,
        "sanity: the FIRST quote's submit must be discarded while the mount is still Pending"
    );

    handle.shutdown_and_join();

    let recs = vike_core::journal::CommandJournal::read_all(&dir).unwrap();
    let submits = recs.iter().filter(|r| matches!(r, JournalRecord::StrategySubmit { .. })).count();
    assert_eq!(submits, 0, "a Pending mount's discarded submit must never reach the journal");
}
