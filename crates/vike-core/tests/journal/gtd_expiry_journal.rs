//! Emulator PR-5: the MANAGED GTD/Day expiry sweep is journaled write-ahead, and a `gtd_sweep`
//! session REPLAYS AND CRASH-RESTORES — the capability that used to be a loud
//! `ReplayError::Unsupported` refusal.
//!
//! This is the last member of the runtime-internal residual class `replay.rs` documented, and it is
//! journaled DIFFERENTLY from its two siblings on purpose. Conditional FIRE (PR-1) and margin-call
//! auto-liquidation (PR-4) RELEASE an order, so their record must be re-applied through
//! `apply_intent` on replay. This sweep releases nothing: it calls `ExecutionEngine::cancel_order`,
//! which publishes NOTHING locally — the venue's authoritative `OrderCanceled` is what advances the
//! FSM, and that arrives as its own journaled `Ingest::Event` and replays independently. So the
//! sweep's contribution to `state_hash` is exactly NIL, and [`JournalRecord::GtdExpire`] is a
//! DECISION MARKER (replay-neutral, never re-applied) rather than a replayable command.
//!
//! What the marker buys is that the expiry never has to be RE-DECIDED on replay — which is what let
//! `gtd_sweep` drop out of the `journal_waker_records` refusal gate. The gate is SHARED with two
//! other wall-clock features, so `the_refusal_still_stands_for_the_stuck_order_watchdog` fences that
//! it was narrowed and not removed.

use crate::scratch::Scratch;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use vike_core::journal::{CommandJournal, JournalFileConfig, JournalRecord};
use vike_core::replay::{replay_offline, restore_from_journal};
use vike_core::{CoreConfig, JournalConfig, spawn_core};
use vike_exec::{
    Account, BalanceMode, Command, ExecutionClient, ExecutionEngine, OrderIntent, OrderStatus,
    RiskGate, RiskLimits,
};
use vike_model::events::{Event, OrderAccepted, OrderCanceled, OrderSubmitted};
use vike_model::{OrderRequest, TimeInForce};

/// A scratch journal directory, removed when the returned guard drops. The journal's own `open`
/// calls `create_dir_all`, so the path is RESERVED rather than created. Hold the guard for the
/// whole test — see `crates/vike-core/src/scratch.rs` for the leak this closed.
fn unique_dir(tag: &str) -> Scratch {
    Scratch::reserved(&format!("gtd-{tag}"))
}

/// A resting-order client (the `core_ergonomics` shape): `submit` acks so the order RESTS, and
/// `cancel` terminalizes it with the venue's authoritative `OrderCanceled` — the event that
/// actually moves the FSM, and the one replay re-folds from its own journaled record.
#[derive(Debug, Default)]
struct RestingClient {
    pending: std::collections::VecDeque<Event>,
    cancels: Arc<AtomicUsize>,
}

impl ExecutionClient for RestingClient {
    fn submit(&mut self, request: &OrderRequest) {
        self.pending.push_back(Event::OrderSubmitted(OrderSubmitted {
            client_order_id: request.client_order_id.clone(),
            ts: request.ts,
        }));
        self.pending.push_back(Event::OrderAccepted(OrderAccepted {
            client_order_id: request.client_order_id.clone(),
            venue_order_id: None,
            ts: request.ts,
        }));
    }
    fn cancel(&mut self, client_order_id: &str) {
        self.cancels.fetch_add(1, Ordering::Relaxed);
        self.pending.push_back(Event::OrderCanceled(OrderCanceled {
            client_order_id: client_order_id.to_string(),
            reason: "gtd".into(),
            ts: 0,
        }));
    }
    fn poll_events(&mut self) -> Option<Event> {
        self.pending.pop_front()
    }
}

fn engine_with(client: RestingClient) -> ExecutionEngine<RestingClient> {
    ExecutionEngine::new(
        Account::new(1.0, "sim", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        client,
        "sim",
        "BTCUSDT",
    )
}

fn gtd_request(coid: &str, expiry_ms: i64) -> Box<OrderRequest> {
    Box::new(OrderRequest {
        client_order_id: coid.into(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 1.0,
        order_type: "limit".into(),
        price: Some(100.0),
        time_in_force: TimeInForce::Gtd,
        gtd_expiry: Some(expiry_ms),
        ..Default::default()
    })
}

fn day_request(coid: &str) -> Box<OrderRequest> {
    Box::new(OrderRequest {
        client_order_id: coid.into(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 1.0,
        order_type: "limit".into(),
        price: Some(100.0),
        time_in_force: TimeInForce::Day,
        ..Default::default()
    })
}

fn wait_until(
    cell: &arc_swap::ArcSwap<vike_core::CoreSnapshot>,
    secs: u64,
    what: &str,
    pred: impl Fn(&vike_core::CoreSnapshot) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        if pred(&cell.load_full()) {
            return;
        }
        assert!(Instant::now() < deadline, "timed out waiting for: {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// `snapshot_every = 3` ⇒ the submit `Cmd` + the two ack events trip the cadence Snap, which
/// becomes the replay BASE with the order already RESTING. Everything the expiry produces (the
/// `GtdExpire` marker + the venue's `OrderCanceled` event) then lands in the TAIL, which is exactly
/// what `replay_offline` re-folds and fences.
fn core_config_gtd(dir: &Path, now: Arc<AtomicI64>) -> CoreConfig {
    CoreConfig {
        gtd_sweep: Some(Duration::from_millis(20)),
        clock: Box::new(move || now.load(Ordering::Relaxed)),
        coid_session: Some(("cafef00d".into(), 0)),
        journal: Some(JournalConfig {
            dir: dir.to_path_buf(),
            file: JournalFileConfig { segment_bytes: 1024 * 1024, flush_every: 1 },
            snapshot_every: 3,
        }),
        ..CoreConfig::default()
    }
}

/// Rest a GTD order, cross its deadline on the injected clock so the boundary sweep expires it, and
/// shut down cleanly. Returns the cancel counter so a caller can prove the sweep actually fired.
fn build_expired_journal(dir: &Path, day: bool) -> usize {
    let cancels = Arc::new(AtomicUsize::new(0));
    let client = RestingClient { pending: Default::default(), cancels: Arc::clone(&cancels) };
    let start = if day { 20_650 * vike_model::MS_PER_DAY + 1_000 } else { 1_000 };
    let now = Arc::new(AtomicI64::new(start));
    let handle = spawn_core(engine_with(client), core_config_gtd(dir, Arc::clone(&now)));
    let cell = handle.snapshot_cell();

    let req = if day { day_request("g1") } else { gtd_request("g1", 5_000) };
    handle.try_command(Command::Order(OrderIntent::Submit(req))).unwrap();
    wait_until(&cell, 5, "order accepted", |s| {
        s.order("g1").map(|o| o.status) == Some(OrderStatus::Accepted)
    });

    // Cross the deadline (GTD: past 5_000; Day: into the next UTC day) — the sweep now expires it.
    now.store(if day { 20_651 * vike_model::MS_PER_DAY + 1 } else { 5_000 }, Ordering::Relaxed);
    wait_until(&cell, 5, "gtd cancel", |s| {
        s.order("g1").map(|o| o.status) == Some(OrderStatus::Canceled)
    });
    handle.shutdown_and_join();
    cancels.load(Ordering::Relaxed)
}

fn records(dir: &Path) -> Vec<JournalRecord> {
    CommandJournal::read_all(dir).unwrap()
}

/// How many `Ingest::Watchdog` records the journal at `dir` currently holds.
///
/// That record IS the refusal's entire input: `replay.rs`'s `replay_from` refuses a journal iff one
/// appears in the tail after the base `Snap`. Nothing else about the session matters to it.
fn waker_records(dir: &Path) -> usize {
    records(dir)
        .iter()
        .filter(|r| matches!(r, JournalRecord::Cmd { msg: vike_exec::Ingest::Watchdog, .. }))
        .count()
}

/// Block until the boundary waker has actually journaled a record — the PRECONDITION any test about
/// the refusal depends on — or fail saying so.
///
/// ⚠ THIS REPLACES A BARE `sleep(150)`, AND THE DIFFERENCE IS THE WHOLE BUG. The waker is a free-
/// running OS thread (`runtime/mod.rs`'s `assemble_core` spawns `vt-core-watchdog` on a
/// `waker_tick` = `(t/2).max(50ms)` sleep loop) while the rest of the session runs on an injected
/// clock frozen at 1000. A fixed sleep is therefore a RACE between two unsynchronised timers, and it
/// is lost exactly when the box is busy: measured on the CI box at f95e6640, 491 failures under CPU
/// saturation plus a spawn storm against 0 in 670 quiet runs, with the CI signature reproduced
/// byte-for-byte (`ReplayOutcome { records: 5, .. }` — arithmetically the ZERO-waker journal: submit
/// `Cmd` + two ack events trip `snapshot_every = 3`, then the exit `Snap`).
///
/// So the old shape asserted a downstream property whose precondition it never established. Every
/// load-immune test in this file polls for what it needs (see [`wait_until`]); the only two that
/// used a bare sleep were the only two that flaked. Polling is also STRICTLY more informative: if
/// the waker genuinely stops journaling, this fails naming that, instead of failing later as a
/// mystery about replay.
fn wait_for_waker_record(dir: &Path, secs: u64) {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        if waker_records(dir) > 0 {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the boundary waker journaled no `Ingest::Watchdog` record within {secs}s. That record \
             is the refusal's only input, so without it the journal is legitimately replayable and \
             any assertion about refusal is vacuous. Either `vt-core-watchdog` is not running, or \
             `journal_waker_records` is off for this config."
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// The record shape: exactly ONE `GtdExpire` per expired order (the fire-once guard), naming the
/// coid and the engine it was routed to — and it is written BEFORE the venue's `OrderCanceled`
/// lands, which is what makes it a write-ahead record rather than a post-hoc note.
#[test]
fn the_expiry_is_journaled_write_ahead_of_the_cancel() {
    let dir = unique_dir("record");
    assert_eq!(build_expired_journal(&dir, false), 1, "the sweep cancelled exactly once");
    let recs = records(&dir);

    let expiries: Vec<_> = recs
        .iter()
        .filter_map(|r| match r {
            JournalRecord::GtdExpire { coid, engine, .. } => Some((coid.clone(), *engine)),
            _ => None,
        })
        .collect();
    assert_eq!(expiries.len(), 1, "one expiry decided ⇒ one record (fire-once)");
    assert_eq!(expiries[0], ("g1".to_string(), 0), "the coid + its routed engine index");

    let marker = recs.iter().position(|r| matches!(r, JournalRecord::GtdExpire { .. })).unwrap();
    let canceled = recs
        .iter()
        .position(|r| {
            matches!(
                r,
                JournalRecord::Cmd { msg: vike_exec::Ingest::Event(Event::OrderCanceled(_)), .. }
            )
        })
        .expect("the venue's authoritative OrderCanceled is journaled as its own event");
    assert!(marker < canceled, "the DECISION is on disk before the effect it caused");

    let _ = std::fs::remove_dir_all(&dir);
}

/// THE CAPABILITY TEST (it returned `ReplayError::Unsupported` before this PR): a session that
/// swept a GTD expiry now replays to a bit-identical `state_hash` AND crash-restores. The terminal
/// outcome is reproduced by the venue's own journaled `OrderCanceled`, not by re-issuing the
/// cancel — which is precisely why the marker does not need re-applying.
#[test]
fn a_gtd_swept_session_now_replays_and_restores() {
    let dir = unique_dir("replay");
    build_expired_journal(&dir, false);

    let out = replay_offline(&dir).expect("PR-5: a gtd_sweep session must now replay");
    assert_eq!(out.snaps_compared, 1);
    assert_eq!(out.engines.len(), 1);
    assert_eq!(
        out.engines[0].registry.iter().find(|(c, _)| c == "g1").map(|(_, o)| o.status),
        Some(OrderStatus::Canceled),
        "the replayed terminal outcome matches the live one"
    );

    let state = restore_from_journal(&dir).unwrap().expect("a clean shutdown always leaves a Snap");
    assert_eq!(
        state.engines[0].registry.iter().find(|(c, _)| c == "g1").map(|(_, o)| o.status),
        Some(OrderStatus::Canceled),
        "crash-restore reaches the same terminal state"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// The same, for the `Day` arm #474 added to the sweep's predicate — one law, one record, one
/// replay path.
#[test]
fn a_day_swept_session_now_replays() {
    let dir = unique_dir("replay-day");
    assert_eq!(build_expired_journal(&dir, true), 1, "the Day arm expired the order");
    assert_eq!(
        records(&dir).iter().filter(|r| matches!(r, JournalRecord::GtdExpire { .. })).count(),
        1,
        "a Day expiry journals the same marker as a Gtd one"
    );
    replay_offline(&dir).expect("PR-5: a Day-swept session must replay too");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The narrowing, proved from the journal itself: a `gtd_sweep`-ONLY core writes NO `Ingest::Watchdog`
/// record (the record that forces the refusal), while the stuck-order watchdog — which shares the
/// gate and is NOT made replayable by this PR — still writes them.
#[test]
fn only_gtd_sweep_dropped_out_of_the_waker_record_gate() {
    let gtd_dir = unique_dir("waker-gtd");
    build_expired_journal(&gtd_dir, false);
    assert_eq!(waker_records(&gtd_dir), 0, "gtd_sweep alone no longer journals the refusal record");

    // The sibling that still does: the stuck-order watchdog on the same boundary waker.
    let wd_dir = unique_dir("waker-stuck");
    let now = Arc::new(AtomicI64::new(1_000));
    let cfg = CoreConfig {
        submit_ack_timeout: Some(Duration::from_millis(20)),
        ..core_config_gtd(&wd_dir, now)
    };
    let handle = spawn_core(engine_with(RestingClient::default()), cfg);
    wait_for_waker_record(&wd_dir, 10); // was a bare sleep(150) — see the helper for why it flaked
    handle.shutdown_and_join();
    assert!(waker_records(&wd_dir) > 0, "the stuck-order watchdog still journals waker records");

    let _ = std::fs::remove_dir_all(&gtd_dir);
    let _ = std::fs::remove_dir_all(&wd_dir);
}

/// THE NEGATIVE CONTROL — the fence stays load-bearing. The `journal_waker_records` gate was
/// NARROWED, not removed: a session running the stuck-order watchdog (whose wall-clock sweep this
/// PR does NOT make replayable) must still refuse LOUDLY with `Unsupported`.
#[test]
fn the_refusal_still_stands_for_the_stuck_order_watchdog() {
    let dir = unique_dir("still-refused");
    let now = Arc::new(AtomicI64::new(1_000));
    // COMBINED on purpose: `core_config_gtd` leaves `gtd_sweep: Some`, so this session runs the
    // now-replayable GTD sweep AND the still-unreplayable stuck-order watchdog. The narrowing must
    // NOT admit it — one unreplayable wall-clock feature is enough to keep the loud refusal.
    let cfg = CoreConfig {
        submit_ack_timeout: Some(Duration::from_millis(20)),
        ..core_config_gtd(&dir, now)
    };
    let handle = spawn_core(engine_with(RestingClient::default()), cfg);
    handle.try_command(Command::Order(OrderIntent::Submit(gtd_request("s1", 9_000)))).unwrap();
    // ⚠ Wait for the RECORD, not for a duration. A bare `sleep(150)` here raced the free-running
    // waker thread and lost under load — the journal then held zero `Ingest::Watchdog` records, was
    // legitimately replayable, and this assert failed in CI claiming the refusal had broken when
    // nothing about the refusal had changed. See `wait_for_waker_record`.
    wait_for_waker_record(&dir, 10);
    handle.shutdown_and_join();

    let err = replay_offline(&dir).expect_err("a watchdog session must still refuse to replay");
    assert!(
        format!("{err}").contains("watchdog"),
        "the loud refusal must still name the watchdog, got: {err}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// COMPAT: a pre-bump (v9) journal — one written before `GtdExpire` existed, so it carries no such
/// record — still replays unchanged under the v10 binary. Built by running a sweep-free session and
/// restamping every segment header to 9; `MIN_READABLE_VERSION` stays 4 and the added variant is
/// purely additive, so the accepted range widened without changing which commands fold.
#[test]
fn a_pre_bump_v9_journal_replays_unchanged() {
    let dir = unique_dir("v9-compat");
    let now = Arc::new(AtomicI64::new(1_000));
    let cfg = CoreConfig { gtd_sweep: None, ..core_config_gtd(&dir, now) };
    let handle = spawn_core(engine_with(RestingClient::default()), cfg);
    handle.try_command(Command::Order(OrderIntent::Submit(gtd_request("v1", 9_000)))).unwrap();
    let cell = handle.snapshot_cell();
    wait_until(&cell, 5, "order accepted", |s| {
        s.order("v1").map(|o| o.status) == Some(OrderStatus::Accepted)
    });
    handle.try_command(Command::Order(OrderIntent::Cancel("v1".into()))).unwrap();
    wait_until(&cell, 5, "cancel", |s| {
        s.order("v1").map(|o| o.status) == Some(OrderStatus::Canceled)
    });
    handle.shutdown_and_join();

    assert!(
        !records(&dir).iter().any(|r| matches!(r, JournalRecord::GtdExpire { .. })),
        "a sweep-free session writes exactly the record set a v9 binary wrote"
    );

    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|s| s.to_str()) != Some("vjl") {
            continue;
        }
        let mut bytes = std::fs::read(&path).unwrap();
        bytes[4..8].copy_from_slice(&9u32.to_le_bytes());
        std::fs::write(&path, bytes).unwrap();
    }

    replay_offline(&dir).expect("a v9 journal must still replay under the v10 binary");
    restore_from_journal(&dir).unwrap().expect("and still crash-restore");
    let _ = std::fs::remove_dir_all(&dir);
}

/// EMULATOR PR-6, the documented-exclusion half for this feature: `CoreThread::gtd_canceled` — the
/// sweep's fire-once set — is deliberately NOT snapshotted, and this proves that choice cannot
/// cost determinism. The set exists only to suppress duplicate cancels within one episode; the
/// record it gates is replay-neutral and the cancel it gates publishes nothing local, so the
/// marker must be INERT to the fence in BOTH directions.
///
/// Same journal, twice: intact, and with every `GtdExpire` record stripped (the shape a restart
/// that lost the fire-once set would perturb — an extra or missing marker for one logical expiry).
/// Both must replay green AND reproduce the SAME `state_hash`. If the marker could move fenced
/// state, these two hashes would differ — and `gtd_canceled` would have to ride the `Snap`.
#[test]
fn the_gtd_fire_once_set_is_outside_the_fence_by_construction() {
    let src = unique_dir("pr6-gtd-marker");
    assert_eq!(build_expired_journal(&src, false), 1, "the sweep fired");
    let recs = records(&src);
    assert_eq!(
        recs.iter().filter(|r| matches!(r, JournalRecord::GtdExpire { .. })).count(),
        1,
        "precondition: the intact journal carries the marker"
    );
    let intact = replay_offline(&src).expect("the intact swept session replays").final_hash;

    // The same records minus every marker, re-emitted as valid frames.
    let stripped_dir = unique_dir("pr6-gtd-marker-stripped");
    let mut j = CommandJournal::open(&stripped_dir, JournalFileConfig::default()).unwrap();
    for r in recs {
        match r {
            JournalRecord::GtdExpire { .. } => {} // the field under test
            JournalRecord::Cmd { now_ms, msg, .. } => {
                j.append_cmd(now_ms, &msg).unwrap();
            }
            JournalRecord::Snap {
                now_ms,
                engines,
                coid_session,
                coid_seq,
                arm_seq,
                conditionals,
                contingencies,
                mount_attr,
                hash,
                ..
            } => {
                j.append_snap(
                    now_ms,
                    &engines,
                    &coid_session,
                    coid_seq,
                    arm_seq.unwrap_or(0),
                    &conditionals,
                    &contingencies,
                    &mount_attr,
                    hash,
                )
                .unwrap();
            }
            JournalRecord::MintedSubmit { now_ms, req, .. } => {
                j.append_minted_submit(now_ms, &req).unwrap();
            }
            other => panic!("this scenario writes no {other:?}"),
        }
    }
    j.flush().unwrap();
    drop(j);

    let stripped = replay_offline(&stripped_dir)
        .expect("a journal without the marker replays identically — the marker is not a command")
        .final_hash;
    assert_eq!(
        intact, stripped,
        "the GtdExpire marker (and therefore the un-snapshotted fire-once set behind it) cannot \
         move the fenced state in either direction"
    );

    let _ = std::fs::remove_dir_all(&src);
    let _ = std::fs::remove_dir_all(&stripped_dir);
}
