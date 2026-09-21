//! Task 5 gate: deterministic replay through the REAL core + the state-hash determinism fence.
//!
//! `replay_offline` restores the engine from the journal's FIRST `Snap` (the earliest full-config
//! checkpoint), spawns a real core with a PURE no-op `ReplayClient` + a `QueueClock` that replays
//! the tail commands' recorded `now_ms`, re-pumps every post-checkpoint `Cmd` in order, and fences
//! the replayed exit-snapshot hash against the SOURCE journal's LAST `Snap` hash.
//!
//! The core scenarios below deliberately use `RecordingClient` (records, but synthesizes NO venue
//! events): every state change therefore rides the exec lane as a JOURNALED `Ingest` record, so the
//! no-op replay client re-derives the identical state, and the scenario stays minimal. A self-
//! advancing clock stamps a distinct `now_ms` per message, and the tail carries a `Command::Submit`
//! whose `ManagedOrder.created_ms` is `now_ms`-derived and hashed — so the fence genuinely exercises
//! `QueueClock` ordering.
//!
//! SINK-ENABLEMENT payoff (`synthesizing_client_session_now_replays`): a `TestExecutionClient`
//! synthesizes Submitted/Accepted/Fill/Filled INSIDE the submit dispatch and returns them via
//! `poll_events` — the `pump_client` path. Those events historically bypassed the journal's write-
//! ahead site, so a paper/test session's fills were NOT durable and replay could not reproduce them
//! (the reason the scenarios above avoid a synthesizing client). `pump_client` now journals each
//! polled event as the same `Ingest::Event` record a real venue event rides, so a synthesizing-
//! client session's fills are both durable (the live tearsheet reads them back) AND replay-
//! reproducible — pinned by that new test.
//!
//! PR-5 T3 adds `mounted_strategy_session_replays_deterministically`: the same fence, but the
//! source session has a MOUNTED submitting strategy instead of external `Command::Order`s — the
//! payoff of folding `JournalRecord::StrategySubmit` through `apply_intent` on replay (see
//! `crates/vike-core/src/replay.rs`'s module doc). `old_journal_without_strategy_submit_still_
//! replays` is the explicit regression guard: an old-shape (mount-free) journal is unaffected.

use crate::scratch::Scratch;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use vike_core::journal::{CommandJournal, JournalFileConfig, JournalRecord};
use vike_core::{CoreConfig, JournalConfig, LiveBroker, StrategyMount, spawn_core};
use vike_exec::testing::{RecordingClient, TestExecutionClient};
use vike_exec::{
    Account, BalanceMode, Command, ExecutionEngine, Ingest, MarketTick, OrderIntent, QuoteUpdate,
    RiskGate, RiskLimits,
};
use vike_model::events::{Event, FillEvent, OrderAccepted, OrderFilled, OrderSubmitted};
use vike_model::{Broker, Clock, OrderRequest, QuoteTick, Strategy};

/// A scratch journal directory, removed when the returned guard drops. The journal's own `open`
/// calls `create_dir_all`, so the path is RESERVED rather than created. Hold the guard for the
/// whole test — see `crates/vike-core/src/scratch.rs` for the leak this closed.
fn unique_dir(tag: &str) -> Scratch {
    Scratch::reserved(tag)
}

/// A bare external fill on the sim venue (empty coid) — the journaled `Ingest::Event` that folds
/// into position/pnl without any client involvement, so replay reproduces it exactly.
fn fill(tid: &'static str, qty: f64, px: f64) -> Event {
    Event::Fill(FillEvent {
        trade_id: tid.into(),
        client_order_id: String::new(),
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

fn limit(coid: &str, side: i32, qty: f64, px: f64) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.into(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side,
        qty,
        order_type: "limit".into(),
        price: Some(px),
        ..Default::default()
    }
}

// ---- PR-5 T3: mounted-strategy replay -----------------------------------------------------

/// A minimal maker-shaped mount: submits exactly ONE market buy, on the FIRST quote tick it
/// sees, then goes quiet. Mirrors `AlwaysSubmitStrategy` in `runtime/mod.rs`'s own `#[cfg(test)]`
/// module (which is private to that module and unreachable from this integration test) but fires
/// once instead of every tick, so the scenario mints exactly one deterministic coid.
struct SubmitOnceStrategy {
    submitted: bool,
}
impl Strategy<LiveBroker> for SubmitOnceStrategy {
    fn on_quote_tick(&mut self, broker: &mut LiveBroker, _q: &QuoteTick) {
        if !self.submitted {
            self.submitted = true;
            broker.submit_market("BTCUSDT", 1, 1.0);
        }
    }
}

fn submit_once_mount() -> StrategyMount {
    StrategyMount {
        account: None,
        symbols: Vec::new(),
        controller_id: None,
        underlying_symbol: None,
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        interval: "1m".into(),
        strategy: Box::new(SubmitOnceStrategy { submitted: false }),
    }
}

/// Deterministically-minted client_order_id of the mount's one-and-only order. `coid_session`
/// seeds `("cafef00d", 0)` in [`build_source_journal_with_mount`] and NOTHING else in that
/// scenario ever calls `coid_gen.generate()` before the strategy's submit (the 4 warmup fills are
/// bare — no coid), so the mint is `<session><seq>` = "cafef00d0", pinned by
/// `client_order_id.rs::generate_starts_at_zero_and_increments`.
const MOUNT_ORDER_COID: &str = "cafef00d0";

/// A venue fill tied to [`MOUNT_ORDER_COID`] (unlike [`fill`]'s bare/untied fills) — used for both
/// the raw `Event::Fill` (Account bookkeeping) and the `Event::OrderFilled` wrap (FSM terminal).
fn mount_fill(tid: &'static str, qty: f64, px: f64) -> FillEvent {
    FillEvent {
        trade_id: tid.into(),
        client_order_id: MOUNT_ORDER_COID.into(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        last_qty: qty,
        last_px: px,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "taker".into(),
        ts: 5,
        mark_price: Some(px),
        position_side: "BOTH".into(),
    }
}

/// Build a source journal from a REAL spawned core with a MOUNTED submitting strategy — the PR-5
/// payoff scenario. `snapshot_every: 4`, 4 bare warmup fills (the 4th trips the cadence Snap = the
/// replay BASE), then a quote tick that drives [`SubmitOnceStrategy`] to submit ONE market buy
/// (journaled as `StrategySubmit`, PR-5 T2, minted [`MOUNT_ORDER_COID`]), followed by the venue's
/// Submitted -> Accepted -> Fill -> OrderFilled lifecycle for that exact order, delivered
/// EXTERNALLY over the exec lane (`handle.event_sender()`) exactly as a real venue WS pump would —
/// `RecordingClient` (unlike `TestExecutionClient`) synthesizes NO lifecycle events on its own (not
/// even `OrderSubmitted` — that is a real adapter's synchronous-emit convention, not the engine's;
/// see `gate_and_register`), so every state change here rides the JOURNALED lane replay re-derives.
/// The resulting tail interleaves a `StrategySubmit` record with four ordinary `Cmd` records, in
/// file order — exactly the shape T3's new replay arm must fold correctly.
fn build_source_journal_with_mount(dir: &Path) {
    let engine = ExecutionEngine::new(
        Account::new(1.0, "sim", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "sim",
        "BTCUSDT",
    );
    let t = Arc::new(AtomicI64::new(0));
    let clock: Box<dyn Clock + Send> = Box::new(move || t.fetch_add(1, Ordering::Relaxed));
    let cfg = CoreConfig {
        seed_cash: 10_000.0,
        clock,
        coid_session: Some(("cafef00d".into(), 0)),
        strategy: Some(submit_once_mount()),
        journal: Some(JournalConfig {
            dir: dir.to_path_buf(),
            file: JournalFileConfig { segment_bytes: 1024 * 1024, flush_every: 8 },
            snapshot_every: 4,
        }),
        ..CoreConfig::default()
    };
    let handle = spawn_core(engine, cfg);
    let sender = handle.event_sender();
    sender.blocking_send(fill("t0", 1.0, 100.0)).unwrap();
    sender.blocking_send(fill("t1", 1.0, 100.0)).unwrap();
    sender.blocking_send(fill("t2", 1.0, 100.0)).unwrap();
    sender.blocking_send(fill("t3", 1.0, 100.0)).unwrap(); // 4th record -> cadence Snap (BASE)

    // quote tick drives the mount -> journaled StrategySubmit, minted MOUNT_ORDER_COID
    handle
        .tick_sender()
        .quote(QuoteUpdate {
            venue: "sim".into(),
            symbol: "BTCUSDT".into(),
            quote: QuoteTick {
                ts: 4,
                local_ts: 4,
                bid: 99.0,
                ask: 101.0,
                bid_size: 1.0,
                ask_size: 1.0,
                symbol: "BTCUSDT".into(),
            },
        })
        .unwrap();

    // the venue's lifecycle for that exact order, delivered externally (journaled Cmd records).
    // OrderSubmitted first: RecordingClient (unlike TestExecutionClient) never synthesizes it, so
    // without this the order would be stuck INITIALIZED forever (Accepted is illegal from there).
    sender
        .blocking_send(Event::OrderSubmitted(OrderSubmitted {
            client_order_id: MOUNT_ORDER_COID.into(),
            ts: 5,
        }))
        .unwrap();
    sender
        .blocking_send(Event::OrderAccepted(OrderAccepted {
            client_order_id: MOUNT_ORDER_COID.into(),
            venue_order_id: None,
            ts: 5,
        }))
        .unwrap();
    let f = mount_fill("t4", 1.0, 100.0);
    sender.blocking_send(Event::Fill(f.clone())).unwrap();
    sender
        .blocking_send(Event::OrderFilled(OrderFilled {
            client_order_id: MOUNT_ORDER_COID.into(),
            fill: f,
            ts: 5,
        }))
        .unwrap();

    handle.shutdown_and_join();
}

/// Run the source scenario, producing a journal at `dir` that spans a cadence Snap + the exit
/// Snap. `snapshot_every = 4`, 7 exec-lane messages: 4 bare fills (the 4th trips the cadence
/// Snap = the replay BASE), then a `Submit` + a bare fill + a `Cancel` in the tail (so the tail
/// is non-empty AND carries both an `Ingest::Event` and an `Ingest::Command`).
fn build_source_journal(dir: &Path) {
    let engine = ExecutionEngine::new(
        Account::new(1.0, "sim", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "sim",
        "BTCUSDT",
    );
    // self-advancing clock: message k is dispatched with now_ms == k (distinct per message, so a
    // wrong QueueClock replay of the Submit's created_ms would break the hash)
    let t = Arc::new(AtomicI64::new(0));
    let clock: Box<dyn Clock + Send> = Box::new(move || t.fetch_add(1, Ordering::Relaxed));
    let cfg = CoreConfig {
        seed_cash: 10_000.0,
        clock,
        coid_session: Some(("cafef00d".into(), 0)),
        journal: Some(JournalConfig {
            dir: dir.to_path_buf(),
            file: JournalFileConfig { segment_bytes: 1024 * 1024, flush_every: 8 },
            snapshot_every: 4,
        }),
        ..CoreConfig::default()
    };
    let handle = spawn_core(engine, cfg);
    let sender = handle.event_sender();
    sender.blocking_send(fill("t0", 1.0, 100.0)).unwrap();
    sender.blocking_send(fill("t1", 1.0, 100.0)).unwrap();
    sender.blocking_send(fill("t2", 1.0, 100.0)).unwrap();
    sender.blocking_send(fill("t3", 1.0, 100.0)).unwrap(); // 4th record -> cadence Snap (BASE)
    handle
        .send_command(Command::Order(OrderIntent::Submit(Box::new(limit("ord1", 1, 1.0, 100.0)))));
    sender.blocking_send(fill("t5", 1.0, 100.0)).unwrap();
    handle.send_command(Command::Order(OrderIntent::Cancel("ord1".into())));
    handle.shutdown_and_join();
}

fn last_snap_hash(recs: &[JournalRecord]) -> u64 {
    recs.iter()
        .rev()
        .find_map(|r| match r {
            JournalRecord::Snap { hash, .. } => Some(*hash),
            _ => None,
        })
        .expect("a Snap is always written on shutdown")
}

/// POSITIVE: replay through the real core reproduces the source's final state hash bit-for-bit.
#[test]
fn replay_reproduces_final_state_hash() {
    let dir = unique_dir("replay-pos");
    build_source_journal(&dir);

    let recs = CommandJournal::read_all(&dir).unwrap();
    let snaps = recs.iter().filter(|r| matches!(r, JournalRecord::Snap { .. })).count();
    assert!(snaps >= 2, "scenario must span a cadence Snap + the exit Snap (got {snaps})");

    let out = vike_core::replay::replay_offline(&dir).expect("replay verifies");
    let live_final = last_snap_hash(&recs);
    assert_eq!(out.final_hash, live_final, "determinism fence: replay reproduces final state");
    let first_snap_hash = recs
        .iter()
        .find_map(|r| match r {
            JournalRecord::Snap { hash, .. } => Some(*hash),
            _ => None,
        })
        .unwrap();
    assert_ne!(
        first_snap_hash, live_final,
        "replayed tail must change state — else the fence is vacuous"
    );
    assert!(out.snaps_compared >= 1);
    assert!(out.records > 0);
    // the restored+replayed single engine is surfaced for Task 6/7 reuse
    assert_eq!(out.engines.len(), 1, "v1 single-engine replay");
    assert_eq!(out.coid_session, "cafef00d");
}

/// NEGATIVE: a journal whose LAST Snap carries a corrupted hash fails the fence — replay's honest
/// recomputation of the true final state cannot equal the doctored recorded hash.
#[test]
fn corrupted_final_hash_fails_the_fence() {
    let good = unique_dir("replay-neg-good");
    build_source_journal(&good);
    let bad = unique_dir("replay-neg-bad");
    copy_journal_files(&good, &bad);

    // read the good LAST snap, then re-append a doctored final Snap into `bad` with a correctly
    // framed (CRC-valid) record but a deliberately WRONG stored hash, so read_all keeps it AS the
    // last Snap (a raw byte-flip of the hash would fail the frame CRC and be dropped as a torn
    // tail — leaving the honest last Snap in place, which would NOT test the fence).
    let recs = CommandJournal::read_all(&good).unwrap();
    let (engines, session, seq, good_hash) = recs
        .iter()
        .rev()
        .find_map(|r| match r {
            JournalRecord::Snap { engines, coid_session, coid_seq, hash, .. } => {
                Some((engines.clone(), coid_session.clone(), *coid_seq, *hash))
            }
            _ => None,
        })
        .unwrap();
    let wrong_hash = good_hash.wrapping_add(1);
    {
        let mut j = CommandJournal::open(
            &bad,
            JournalFileConfig { segment_bytes: 1024 * 1024, flush_every: 8 },
        )
        .unwrap();
        j.append_snap(999, &engines, &session, seq, 0, &[], &[], &[], wrong_hash).unwrap();
        j.flush().unwrap();
    }

    match vike_core::replay::replay_offline(&bad) {
        Err(vike_core::replay::ReplayError::HashMismatch { expected, got }) => {
            assert_eq!(expected, wrong_hash, "fence target is the corrupted recorded hash");
            assert_eq!(got, good_hash, "replay honestly recomputes the TRUE final hash");
        }
        other => panic!("expected HashMismatch, got {other:?}"),
    }
}

/// A TORN TAIL is reported as TRUNCATION, not as a determinism divergence.
///
/// The same journal, twice: intact it verifies; with its LAST record's payload cut short (a crash
/// between the length write and the payload write — the ordinary shape after a power cut) the read
/// halts before the exit `Snap`, so the fence target becomes the surviving mid-session cadence
/// `Snap` and the honestly re-folded tail cannot match it. That used to surface as `HashMismatch`
/// — "the engine diverged / this journal cannot be trusted" — for the most benign failure mode the
/// journal has. It now surfaces as `Truncated { cause: OobLen }`.
///
/// DIAGNOSTIC ONLY: the intact half of this test pins that nothing about a clean journal moved, and
/// the halt policy (`read_all` keeps the valid prefix and discards the rest) is untouched.
#[test]
fn a_torn_tail_reports_truncation_not_a_determinism_divergence() {
    let good = unique_dir("replay-torn-good");
    build_source_journal(&good);
    vike_core::replay::replay_offline(&good).expect("the INTACT journal still verifies");

    let torn = unique_dir("replay-torn");
    copy_journal_files(&good, &torn);
    let intact_records = CommandJournal::read_all(&torn).unwrap().len();

    // Cut ONE byte off the end of the written data: the final frame (the exit `Snap`) then claims
    // more bytes than the segment holds, which is `CorruptCause::OobLen`. Everything before it is
    // byte-identical, so this plants exactly one defect. (`rposition` over the zero-filled tail is
    // the same trick `frame.rs`'s torn-tail fixture uses — the write cursor is not exposed.)
    let seg = std::fs::read_dir(&torn)
        .unwrap()
        .filter_map(|e| {
            let p = e.unwrap().path();
            (p.extension().and_then(|s| s.to_str()) == Some("vjl")).then_some(p)
        })
        .next()
        .expect("the scenario writes one segment");
    let bytes = std::fs::read(&seg).unwrap();
    let last_nonzero = bytes.iter().rposition(|&b| b != 0).unwrap();
    std::fs::write(&seg, &bytes[..last_nonzero]).unwrap();

    let kept = CommandJournal::read_all(&torn).unwrap();
    assert!(
        kept.len() < intact_records,
        "the scenario must actually LOSE the tail (kept {} of {intact_records})",
        kept.len()
    );
    assert!(
        kept.iter().any(|r| matches!(r, JournalRecord::Snap { .. })),
        "a mid-session Snap must survive — else this would be `Empty`, a different path"
    );

    match vike_core::replay::replay_offline(&torn) {
        Err(vike_core::replay::ReplayError::Truncated { cause }) => assert_eq!(
            cause,
            vike_core::journal::CorruptCause::OobLen,
            "a cut-short final frame is a torn LENGTH, the benign post-crash cause"
        ),
        other => panic!("expected Truncated{{ cause: OobLen }}, got {other:?}"),
    }
}

fn copy_journal_files(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for e in std::fs::read_dir(src).unwrap() {
        let p = e.unwrap().path();
        if p.extension().and_then(|s| s.to_str()) == Some("vjl") {
            std::fs::copy(&p, dst.join(p.file_name().unwrap())).unwrap();
        }
    }
}

/// Like [`build_source_journal`], but sends a NON-journaled market/mark tick AFTER the last exec
/// `Cmd`. That tick overwrites the (sim, BTCUSDT) mark the fills set (100.0 -> 42_000.0) via
/// `set_mark` and — because the core stamps `engine.now_ms = clock.now_ms()` for EVERY dispatched
/// message — bumps the source's final `now_ms`. Neither is carried on the journaled exec lane, so
/// offline replay (which re-folds only that lane) cannot re-derive them. This is exactly the live-
/// session shape that made the fence spuriously fail before the hash excluded those two fields.
fn build_source_journal_with_trailing_mark(dir: &Path) {
    let engine = ExecutionEngine::new(
        Account::new(1.0, "sim", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "sim",
        "BTCUSDT",
    );
    let t = Arc::new(AtomicI64::new(0));
    let clock: Box<dyn Clock + Send> = Box::new(move || t.fetch_add(1, Ordering::Relaxed));
    let cfg = CoreConfig {
        seed_cash: 10_000.0,
        clock,
        coid_session: Some(("cafef00d".into(), 0)),
        journal: Some(JournalConfig {
            dir: dir.to_path_buf(),
            file: JournalFileConfig { segment_bytes: 1024 * 1024, flush_every: 8 },
            snapshot_every: 4,
        }),
        ..CoreConfig::default()
    };
    let handle = spawn_core(engine, cfg);
    let sender = handle.event_sender();
    sender.blocking_send(fill("t0", 1.0, 100.0)).unwrap();
    sender.blocking_send(fill("t1", 1.0, 100.0)).unwrap();
    sender.blocking_send(fill("t2", 1.0, 100.0)).unwrap();
    sender.blocking_send(fill("t3", 1.0, 100.0)).unwrap(); // 4th record -> cadence Snap (BASE)
    handle
        .send_command(Command::Order(OrderIntent::Submit(Box::new(limit("ord1", 1, 1.0, 100.0)))));
    sender.blocking_send(fill("t5", 1.0, 100.0)).unwrap();
    handle.send_command(Command::Order(OrderIntent::Cancel("ord1".into())));
    // The load-bearing line: a NON-journaled mark tick AFTER the last exec Cmd (FIFO-ordered ahead
    // of the Shutdown that shutdown_and_join sends). It lands in the source's final snapshot but
    // never in the journal.
    handle.market_sender().publish(MarketTick {
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        px: 42_000.0,
        ts: 999,
    });
    handle.shutdown_and_join();
}

/// FIX-1 GATE (whole-branch review): a non-journaled market tick after the last exec `Cmd` must NOT
/// break the determinism fence. Before Fix 1 (`state_hash` hashed `now_ms`/`marks`) the source's
/// final snapshot — carrying the tick's 42_000.0 mark + bumped now_ms — could not be reproduced by
/// replay, so `replay_offline` returned `HashMismatch` on a perfectly valid live journal. After Fix
/// 1 both fields are excluded, so replay SUCCEEDS. Fails before Fix 1, passes after — pins the
/// boundary.
#[test]
fn market_tick_after_last_cmd_does_not_break_fence() {
    let dir = unique_dir("replay-market-tick");
    build_source_journal_with_trailing_mark(&dir);

    let recs = CommandJournal::read_all(&dir).unwrap();
    // Non-vacuous: the source's final snapshot really DID record the non-journaled mark, so the fence
    // genuinely exercises the excluded-field path (replay can never re-derive this mark).
    let final_marks = recs
        .iter()
        .rev()
        .find_map(|r| match r {
            JournalRecord::Snap { engines, .. } => Some(engines[0].account.marks.clone()),
            _ => None,
        })
        .expect("a Snap is always written on shutdown");
    assert!(
        final_marks.iter().any(|((_, sym), px)| sym.as_str() == "BTCUSDT" && *px == 42_000.0),
        "source final snapshot must carry the non-journaled mark tick (else the test is vacuous)"
    );

    // The fence must PASS: now_ms/marks are excluded from state_hash, so re-folding only the exec
    // lane reproduces the recoverable state. (This returned HashMismatch before Fix 1.)
    let out = vike_core::replay::replay_offline(&dir)
        .expect("fence must hold: now_ms/marks are excluded from state_hash");
    assert_eq!(out.final_hash, last_snap_hash(&recs), "replay reproduces the fenced final state");
}

/// THE PAYOFF (PR-5 T3): a session with a MOUNTED submitting strategy — previously out of v1
/// replay scope per this module's own docs, surfacing as `HashMismatch` because a strategy's
/// resolved order intents rode `JournalRecord::StrategySubmit`, which the tail-extraction
/// `filter_map` dropped (`_ => None`) instead of folding through `apply_intent` — now replays
/// deterministically. `build_source_journal_with_mount` drives `SubmitOnceStrategy` to submit ONE
/// market order (journaled `StrategySubmit`) and then feeds that exact order's venue lifecycle
/// (Accepted -> Fill -> OrderFilled) over the exec lane, so the scenario exercises submit AND
/// fill, with the `StrategySubmit` record interleaved with ordinary `Cmd` records in the tail.
#[test]
fn mounted_strategy_session_replays_deterministically() {
    let dir = unique_dir("replay-mount");
    build_source_journal_with_mount(&dir);

    let recs = CommandJournal::read_all(&dir).unwrap();
    // Non-vacuous: the scenario really did journal a StrategySubmit (else this test would pass
    // trivially without ever exercising the new arm).
    assert!(
        recs.iter().any(|r| matches!(r, JournalRecord::StrategySubmit { .. })),
        "scenario must produce a StrategySubmit record (mounted strategy actually submitted)"
    );
    let snaps = recs.iter().filter(|r| matches!(r, JournalRecord::Snap { .. })).count();
    assert!(snaps >= 2, "scenario must span a cadence Snap + the exit Snap (got {snaps})");
    // The submitted order really did reach FILLED, so the fenced state includes the fill's
    // effect on both the registry (FSM) and the account (position/pnl) — not just a bare submit.
    let final_engines = recs
        .iter()
        .rev()
        .find_map(|r| match r {
            JournalRecord::Snap { engines, .. } => Some(engines.clone()),
            _ => None,
        })
        .expect("a Snap is always written on shutdown");
    let mo = final_engines[0]
        .registry
        .iter()
        .find(|(coid, _)| coid == MOUNT_ORDER_COID)
        .map(|(_, mo)| mo)
        .expect("the mount's order is in the final registry");
    assert_eq!(
        mo.status,
        vike_exec::OrderStatus::Filled,
        "the mount's order must have reached FILLED (else the fence is a weaker submit-only check)"
    );

    let src_hash = last_snap_hash(&recs);
    let out = vike_core::replay::replay_offline(&dir).expect(
        "replay must fold StrategySubmit through apply_intent and reproduce the source's final hash",
    );
    assert_eq!(
        out.final_hash, src_hash,
        "mounted-session replay (submit + fill) reproduces the source's final state"
    );

    // Non-vacuous the other way too: the replayed tail must actually have changed state from the
    // base cadence snap, else the fence would trivially hold on an unchanged restore.
    let base_hash = recs
        .iter()
        .find_map(|r| match r {
            JournalRecord::Snap { hash, .. } => Some(*hash),
            _ => None,
        })
        .unwrap();
    assert_ne!(base_hash, src_hash, "the replayed tail (submit + fill) must change state");
}

/// Build a source journal from a REAL spawned core whose client is a `TestExecutionClient` — a
/// SYNTHESIZING client (unlike `RecordingClient`): it emits the full Submitted → Accepted → Fill →
/// Filled lifecycle for each submitted order via `poll_events`, drained by `pump_client`. Four bare
/// warmup fills over the exec lane trip the cadence Snap (the replay BASE); then one EXTERNAL
/// `Command::Order` (`op1`) drives the client to synthesize its lifecycle, which `pump_client`
/// journals at teardown (sink-enablement — the submit dispatch itself does not pump). Before sink-
/// enablement that lifecycle never reached the journal, so this journal would have been
/// unreplayable; now the tail carries the command plus the four synthesized `Ingest::Event` records.
fn build_source_journal_with_test_client(dir: &Path) {
    let engine = ExecutionEngine::new(
        Account::new(1.0, "sim", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        TestExecutionClient::new("sim", 100.0),
        "sim",
        "BTCUSDT",
    );
    let t = Arc::new(AtomicI64::new(0));
    let clock: Box<dyn Clock + Send> = Box::new(move || t.fetch_add(1, Ordering::Relaxed));
    let cfg = CoreConfig {
        seed_cash: 10_000.0,
        clock,
        coid_session: Some(("cafef00d".into(), 0)),
        journal: Some(JournalConfig {
            dir: dir.to_path_buf(),
            file: JournalFileConfig { segment_bytes: 1024 * 1024, flush_every: 8 },
            snapshot_every: 4,
        }),
        ..CoreConfig::default()
    };
    let handle = spawn_core(engine, cfg);
    let sender = handle.event_sender();
    sender.blocking_send(fill("t0", 1.0, 100.0)).unwrap();
    sender.blocking_send(fill("t1", 1.0, 100.0)).unwrap();
    sender.blocking_send(fill("t2", 1.0, 100.0)).unwrap();
    sender.blocking_send(fill("t3", 1.0, 100.0)).unwrap(); // 4th record -> cadence Snap (BASE)
    // External submit -> TestExecutionClient synthesizes Submitted/Accepted/Fill/Filled into its
    // pending queue; pump_client drains + (now) journals them at teardown.
    handle.send_command(Command::Order(OrderIntent::Submit(Box::new(limit("op1", 1, 1.0, 100.0)))));
    handle.shutdown_and_join();
}

/// SINK-ENABLEMENT payoff: a session driven by a SYNTHESIZING client (`TestExecutionClient`) — whose
/// fills were previously invisible to the journal and thus unreplayable (see this module's doc) —
/// now (a) carries its synthesized `Event::Fill` in the journal and (b) replays to the source's
/// exact final state hash, because `pump_client` journals each polled event as an `Ingest::Event`.
#[test]
fn synthesizing_client_session_now_replays() {
    let dir = unique_dir("replay-paper");
    build_source_journal_with_test_client(&dir);

    let recs = CommandJournal::read_all(&dir).unwrap();
    // (a) Non-vacuous: the paper-synthesized Fill really is in the journal now — the whole point.
    assert!(
        recs.iter().any(|r| matches!(
            r,
            JournalRecord::Cmd { msg: Ingest::Event(Event::Fill(f)), .. }
                if f.client_order_id.as_str() == "op1"
        )),
        "sink-enablement: the paper-synthesized Fill for op1 must be journaled as an Ingest::Event"
    );
    let snaps = recs.iter().filter(|r| matches!(r, JournalRecord::Snap { .. })).count();
    assert!(snaps >= 2, "scenario must span a cadence Snap + the exit Snap (got {snaps})");

    // (b) The determinism fence must hold: the journaled synthesized lifecycle re-folds to the
    // source's exact final state.
    let src_hash = last_snap_hash(&recs);
    let out = vike_core::replay::replay_offline(&dir).expect(
        "a synthesizing-client session now replays: its fills are journaled at pump_client",
    );
    assert_eq!(
        out.final_hash, src_hash,
        "paper-session replay (external submit + synthesized fill) reproduces the final state"
    );
    // Non-vacuous the other way: the replayed tail must have moved state off the base cadence snap.
    let base_hash = recs
        .iter()
        .find_map(|r| match r {
            JournalRecord::Snap { hash, .. } => Some(*hash),
            _ => None,
        })
        .unwrap();
    assert_ne!(
        base_hash, src_hash,
        "the replayed tail (submit + synthesized fill) must change state"
    );
}

/// Regression guard (PR-5 T3): an OLD-SHAPE journal — exec-lane only, no mounted strategy, so it
/// carries zero `StrategySubmit` records — must keep replaying exactly as it did before the new
/// arm was added. `replay_reproduces_final_state_hash` above already exercises this journal shape
/// end-to-end; this test pins "old journal shape has zero StrategySubmit records" explicitly, so
/// the regression intent is a named assertion rather than incidental.
#[test]
fn old_journal_without_strategy_submit_still_replays() {
    let dir = unique_dir("replay-old-shape");
    build_source_journal(&dir);

    let recs = CommandJournal::read_all(&dir).unwrap();
    assert!(
        !recs.iter().any(|r| matches!(r, JournalRecord::StrategySubmit { .. })),
        "an unmounted session must carry zero StrategySubmit records (the old journal shape)"
    );

    let src_hash = last_snap_hash(&recs);
    let out =
        vike_core::replay::replay_offline(&dir).expect("old-shape journal still replays clean");
    assert_eq!(
        out.final_hash, src_hash,
        "old-shape replay is unaffected by the new StrategySubmit arm"
    );
}
