//! RUNTIME strategy mount/unmount tests (split-plane B5) — the regression gates for the dynamic
//! slot machinery `Command::MountStrategy` / `Command::UnmountStrategy` drive:
//!
//! - a mount added AT RUNTIME trades, and its fills route to ITS attribution (ledger + `on_fill`);
//! - unmount CANCELS the mount's attributed resting orders (the spec's named safety decision) and
//!   SAVES its durable-state sidecar, and the slot tombstones (`None`, index never reused);
//! - a duplicate mount id is a REFUSAL note in recent-events, never the spawn-time panic;
//! - an unknown unmount id, an unknown venue and a factory-less core each refuse the same way;
//! - the `any_mount_*` inert-when-empty gates re-derive on mount AND unmount.
//!
//! White-box, in-crate like `multi_mount_tests` (`use super::*` re-exports the runtime module);
//! commands are driven through the REAL `dispatch(Ingest::Command(..))` path so the journaled-set
//! exclusion and the arm wiring are part of what is under test.

use super::*;
use std::sync::Mutex as TestMutex;
use vike_exec::testing::RecordingClient;
use vike_exec::{Account, MountSpec, RiskGate, RiskLimits};
use vike_model::events::{FillEvent, TradeId};

/// Assemble a `CoreThread<RecordingClient>` from a caller-supplied config, the way `spawn_core`
/// does minus the OS thread (the `multi_mount_tests::core_with` twin — duplicated rather than
/// shared because a private helper in a sibling test module is not reachable from here).
fn core_with(config: CoreConfig) -> CoreThread<RecordingClient> {
    let engine = ExecutionEngine::new(
        Account::new(1.0, "sim", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "sim",
        "BTCUSDT",
    );
    let market = Arc::new(Conflated {
        state: Mutex::new(ConflatedState::default()),
        drops: AtomicU64::new(0),
    });
    let snapshot =
        Arc::new(ArcSwap::from_pointee(CoreSnapshot::empty(&engine.venue, &engine.symbol)));
    assemble_core(engine, Vec::new(), config, market, snapshot, Arc::new(AtomicU64::new(0)))
}

/// What the mounted test strategy does on the next `on_feed_status` dispatch (the
/// `multi_mount_tests::Step` idiom — feed-status fans out to every mount, so one driver call
/// exercises any mount without needing a bar series).
#[derive(Clone, Copy)]
enum Step {
    /// submit a tagged limit under the literal tag `"bid"`
    Quote,
    /// do nothing
    Idle,
}

/// The runtime-mounted test strategy: quotes/idles per its shared `step` cell, records every
/// `on_fill` under its label, and round-trips durable state through `save_state`/`load_state` so
/// the unmount-saves / re-mount-loads contract is assertable.
struct RtQuoter {
    label: &'static str,
    step: Arc<TestMutex<Step>>,
    fills: Arc<TestMutex<Vec<(&'static str, f64)>>>,
    /// what `save_state` returns (`None` = nothing durable).
    state_out: Option<serde_json::Value>,
    /// every value `load_state` received, appended in call order.
    loaded: Arc<TestMutex<Vec<serde_json::Value>>>,
}

impl Strategy<LiveBroker> for RtQuoter {
    fn on_feed_status(&mut self, broker: &mut LiveBroker, _status: FeedStatus) {
        match *self.step.lock().unwrap() {
            Step::Quote => broker.submit_limit_tagged("bid", 1, 1.0, 100.0),
            Step::Idle => {}
        }
    }

    fn on_fill(&mut self, _broker: &mut LiveBroker, fill: &Fill) {
        self.fills.lock().unwrap().push((self.label, fill.size));
    }

    fn save_state(&self) -> Option<serde_json::Value> {
        self.state_out.clone()
    }

    fn load_state(&mut self, v: &serde_json::Value) {
        self.loaded.lock().unwrap().push(v.clone());
    }
}

/// Shared cells + a factory building an [`RtQuoter`] for every resolve — the composition-root
/// closure `CoreConfig::strategy_factory` models. Refuses a spec naming neither `name` nor `rhai`
/// so a refusal-path test has a real factory error to surface.
#[allow(clippy::type_complexity)]
fn quoter_factory(
    label: &'static str,
    step: &Arc<TestMutex<Step>>,
    fills: &Arc<TestMutex<Vec<(&'static str, f64)>>>,
    state_out: Option<serde_json::Value>,
    loaded: &Arc<TestMutex<Vec<serde_json::Value>>>,
) -> StrategyFactory {
    let (step, fills, loaded) = (Arc::clone(step), Arc::clone(fills), Arc::clone(loaded));
    Box::new(move |spec: &MountSpec| {
        if spec.name.is_none() && spec.rhai.is_none() {
            return Err("mount spec names neither `name` nor `rhai`".to_string());
        }
        Ok(Box::new(RtQuoter {
            label,
            step: Arc::clone(&step),
            fills: Arc::clone(&fills),
            state_out: state_out.clone(),
            loaded: Arc::clone(&loaded),
        }))
    })
}

/// A well-formed runtime mount spec on the test core's own engine/series.
fn spec(controller_id: &str) -> MountSpec {
    MountSpec {
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        interval: "1m".into(),
        account: None,
        controller_id: Some(controller_id.to_string()),
        name: Some("test_quoter".into()),
        rhai: None,
        params: serde_json::json!({}),
    }
}

fn mount_cmd(s: MountSpec) -> Ingest {
    Ingest::Command(Command::MountStrategy(Box::new(s)))
}

fn unmount_cmd(id: &str) -> Ingest {
    Ingest::Command(Command::UnmountStrategy { controller_id: id.to_string() })
}

/// A fill for `coid` on (sim, BTCUSDT) — the `multi_mount_tests::coid_fill` shape.
fn coid_fill(coid: &str, side: i32, qty: f64, px: f64) -> FillEvent {
    FillEvent {
        trade_id: TradeId::prefixed("t-", format_args!("{coid}-{side}-{px}")),
        client_order_id: coid.to_string(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side,
        last_qty: qty,
        last_px: px,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "taker".to_string().into(),
        ts: 0,
        mark_price: None,
        position_side: "BOTH".into(),
    }
}

/// The coid `mount_idx` owns (its first, in insertion order).
fn coid_of(core: &CoreThread<RecordingClient>, mount_idx: usize) -> String {
    core.coid_mount
        .iter()
        .find(|&(_, &m)| m == mount_idx)
        .map(|(c, _)| c.clone())
        .expect("mount submitted at least one order")
}

/// True when some recent-events line contains `needle`.
fn noted(core: &CoreThread<RecordingClient>, needle: &str) -> bool {
    core.recent.iter().any(|l| l.contains(needle))
}

fn state_dir() -> std::path::PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let d = std::env::temp_dir().join(format!("vrt-mount-{}-{}", std::process::id(), n));
    let _ = std::fs::remove_dir_all(&d);
    d
}

/// **The core B5 property.** A core spawned with NO mount at all gains one at runtime through the
/// real command path; the new mount trades, and its fill routes to ITS OWN attribution — ledger
/// (`mount_attr`) and callback (`on_fill`) both.
#[test]
fn runtime_mount_trades_and_attributes_fills() {
    let step = Arc::new(TestMutex::new(Step::Quote));
    let fills = Arc::new(TestMutex::new(Vec::new()));
    let loaded = Arc::new(TestMutex::new(Vec::new()));
    let mut core = core_with(CoreConfig {
        seed_cash: 10_000.0,
        strategy_factory: Some(quoter_factory("RT", &step, &fills, None, &loaded)),
        ..CoreConfig::default()
    });
    assert!(core.mounts.is_empty(), "spawned mount-less");
    assert!(!core.engine.collect_applied_fills, "no mount ⇒ capture disarmed at assembly");

    core.dispatch(mount_cmd(spec("rt-a")));

    assert_eq!(core.mounts.len(), 1, "one live slot appended");
    assert!(core.mounts[0].is_some());
    assert_eq!(core.mount_ids[0], "rt_a", "controller id IS the mount id, sanitized");
    assert!(noted(&core, "MOUNTED strategy `rt_a`"), "recent: {:?}", core.recent);
    assert!(
        core.engine.collect_applied_fills,
        "the mount arm arms applied-fill capture on the primary engine"
    );

    // The mounted strategy trades: one feed-status dispatch → a tagged resting quote.
    core.drive_strategy_feed_status("sim", "BTCUSDT", FeedStatus::Live);
    let coid = coid_of(&core, 0);
    assert_eq!(
        core.engine.client.submissions.len(),
        1,
        "the runtime mount's quote reached the client"
    );
    assert_eq!(core.strategy_tags.get("0|sim|BTCUSDT|bid"), Some(&coid));

    // ...and its fill folds into ITS ledger and reaches ITS on_fill.
    *step.lock().unwrap() = Step::Idle;
    core.bus.publish(Event::Fill(coid_fill(&coid, 1, 3.0, 100.0)), &mut core.engine);
    core.dispatch_applied_fills();
    assert_eq!(fills.lock().unwrap().clone(), vec![("RT", 3.0)], "on_fill routed to the mount");
    assert_eq!(core.mount_attr[0].size, 3.0, "the fill folded into the mount's own ledger");
}

/// **The resting-order decision + durable state.** Unmount cancels EXACTLY the mount's attributed
/// resting orders, saves its state sidecar, tombstones the slot (never reused), and the published
/// mount view drops the row. A later re-mount of the SAME id lands on a FRESH slot and loads the
/// state the unmount saved.
#[test]
fn unmount_cancels_resting_orders_and_saves_state() {
    let step = Arc::new(TestMutex::new(Step::Quote));
    let fills = Arc::new(TestMutex::new(Vec::new()));
    let loaded = Arc::new(TestMutex::new(Vec::new()));
    let dir = state_dir();
    let saved = serde_json::json!({"acc": 41});
    let mut core = core_with(CoreConfig {
        seed_cash: 10_000.0,
        state_dir: Some(dir.clone()),
        strategy_factory: Some(quoter_factory("RT", &step, &fills, Some(saved.clone()), &loaded)),
        ..CoreConfig::default()
    });
    core.dispatch(mount_cmd(spec("rt-a")));
    core.drive_strategy_feed_status("sim", "BTCUSDT", FeedStatus::Live);
    let coid = coid_of(&core, 0);
    assert!(core.engine.client.cancels.is_empty(), "quote resting, nothing canceled yet");

    core.dispatch(unmount_cmd("rt-a"));

    // (1) the mount's OWN resting order was canceled through the engine…
    assert!(
        core.engine.client.cancels.contains(&coid),
        "unmount must cancel the mount's attributed resting order: {:?}",
        core.engine.client.cancels
    );
    // (2) …its durable state was saved…
    let sidecar = crate::strategy_state::sidecar_path(&dir, "rt_a");
    assert_eq!(
        crate::strategy_state::read_json(&sidecar).as_ref(),
        Some(&saved),
        "unmount wrote the state sidecar"
    );
    // (3) …and the slot is a tombstone: gone from the published view, index not reused.
    assert!(core.mounts[0].is_none(), "slot tombstoned");
    assert!(core.mount_views().is_empty(), "no live mount rows published");
    assert!(noted(&core, "UNMOUNTED strategy `rt_a`"), "recent: {:?}", core.recent);

    // Re-mount of the SAME id: a fresh slot APPENDED (the tombstone's index is an attribution
    // key and is never reused), and the saved state loads into the new strategy.
    core.dispatch(mount_cmd(spec("rt-a")));
    assert_eq!(core.mounts.len(), 2, "re-mount appends; tombstone index never reused");
    assert!(core.mounts[0].is_none() && core.mounts[1].is_some());
    assert_eq!(core.mount_ids[1], "rt_a");
    assert_eq!(
        loaded.lock().unwrap().clone(),
        vec![saved],
        "the re-mounted strategy loaded the state the unmount saved"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A runtime mount whose id is already LIVE is a refusal EVENT — the runtime twin of the
/// spawn-time duplicate panic (which `multi_mount_tests` pins separately). Nothing panics, the
/// slot vector is unchanged, and the refusal names the id.
#[test]
fn duplicate_runtime_mount_is_refused_as_event() {
    let step = Arc::new(TestMutex::new(Step::Idle));
    let fills = Arc::new(TestMutex::new(Vec::new()));
    let loaded = Arc::new(TestMutex::new(Vec::new()));
    let mut core = core_with(CoreConfig {
        seed_cash: 10_000.0,
        strategy_factory: Some(quoter_factory("RT", &step, &fills, None, &loaded)),
        ..CoreConfig::default()
    });
    core.dispatch(mount_cmd(spec("rt-a")));
    assert_eq!(core.mounts.len(), 1);

    core.dispatch(mount_cmd(spec("rt-a")));

    assert_eq!(core.mounts.len(), 1, "duplicate refused — no second slot");
    assert!(
        noted(&core, "MOUNT REFUSED: duplicate strategy-mount id `rt_a`"),
        "recent: {:?}",
        core.recent
    );
}

/// The three remaining refusal arms, each a NOTE and never a panic: unknown unmount id,
/// factory-less core, unknown venue — plus the factory's own `Err` surfacing verbatim.
#[test]
fn refusal_arms_note_and_never_panic() {
    let step = Arc::new(TestMutex::new(Step::Idle));
    let fills = Arc::new(TestMutex::new(Vec::new()));
    let loaded = Arc::new(TestMutex::new(Vec::new()));

    // (a) unknown unmount id
    let mut core = core_with(CoreConfig {
        seed_cash: 10_000.0,
        strategy_factory: Some(quoter_factory("RT", &step, &fills, None, &loaded)),
        ..CoreConfig::default()
    });
    core.dispatch(unmount_cmd("nope"));
    assert!(
        noted(&core, "UNMOUNT REFUSED: no live strategy mount with id `nope`"),
        "recent: {:?}",
        core.recent
    );

    // (b) unknown venue — no engine runs it
    core.dispatch(mount_cmd(MountSpec { venue: "okx".into(), ..spec("rt-b") }));
    assert!(core.mounts.is_empty(), "unknown-venue mount refused");
    assert!(noted(&core, "no engine for it"), "recent: {:?}", core.recent);

    // (c) the factory's own refusal surfaces verbatim
    core.dispatch(mount_cmd(MountSpec { name: None, rhai: None, ..spec("rt-c") }));
    assert!(core.mounts.is_empty());
    assert!(noted(&core, "names neither `name` nor `rhai`"), "recent: {:?}", core.recent);

    // (d) a factory-less core refuses every mount
    let mut bare = core_with(CoreConfig { seed_cash: 10_000.0, ..CoreConfig::default() });
    bare.dispatch(mount_cmd(spec("rt-a")));
    assert!(bare.mounts.is_empty());
    assert!(noted(&bare, "no strategy factory is configured"), "recent: {:?}", bare.recent);
}

/// The `any_mount_*` gates re-derive on BOTH arms: a runtime mount whose id carries a configured
/// budget arms `any_mount_budget`, and its unmount disarms it again — the same formulas
/// `assemble_core` evaluates at spawn, so the fold's inert-when-empty reads stay truthful.
#[test]
fn runtime_mount_arms_and_unmount_disarms_the_budget_gate() {
    let step = Arc::new(TestMutex::new(Step::Idle));
    let fills = Arc::new(TestMutex::new(Vec::new()));
    let loaded = Arc::new(TestMutex::new(Vec::new()));
    let mut budgets = std::collections::HashMap::new();
    budgets.insert(
        "rt_a".to_string(),
        MountBudget { max_loss: Some(50.0), max_notional: None, flatten_on_breach: false },
    );
    let mut core = core_with(CoreConfig {
        seed_cash: 10_000.0,
        mount_budgets: budgets,
        strategy_factory: Some(quoter_factory("RT", &step, &fills, None, &loaded)),
        ..CoreConfig::default()
    });
    assert!(!core.any_mount_budget, "no mount ⇒ gate inert");

    core.dispatch(mount_cmd(spec("rt-a")));
    assert!(core.any_mount_budget, "the budget keyed by the new mount's id arms the gate");
    assert_eq!(core.mount_budget[0].map(|b| b.max_loss), Some(Some(50.0)));

    core.dispatch(unmount_cmd("rt-a"));
    assert!(!core.any_mount_budget, "unmount clears the slot's budget and disarms the gate");
}

/// A straggler fill for an UNMOUNTED mount's in-flight order must not panic, must not reach any
/// live strategy's `on_fill`, and still folds into the dead slot's ledger (the residual row's
/// accounting) — the tombstone-index contract.
#[test]
fn straggler_fill_after_unmount_folds_ledger_and_routes_nowhere() {
    let step = Arc::new(TestMutex::new(Step::Quote));
    let fills = Arc::new(TestMutex::new(Vec::new()));
    let loaded = Arc::new(TestMutex::new(Vec::new()));
    let mut core = core_with(CoreConfig {
        seed_cash: 10_000.0,
        strategy_factory: Some(quoter_factory("RT", &step, &fills, None, &loaded)),
        ..CoreConfig::default()
    });
    core.dispatch(mount_cmd(spec("rt-a")));
    core.drive_strategy_feed_status("sim", "BTCUSDT", FeedStatus::Live);
    let coid = coid_of(&core, 0);
    core.dispatch(unmount_cmd("rt-a"));

    // The venue's fill for the (cancel-raced) order arrives AFTER the unmount.
    core.bus.publish(Event::Fill(coid_fill(&coid, 1, 2.0, 100.0)), &mut core.engine);
    core.dispatch_applied_fills();

    assert!(fills.lock().unwrap().is_empty(), "no live strategy heard the dead mount's fill");
    assert_eq!(core.mount_attr[0].size, 2.0, "the tombstoned slot's ledger still folded the fill");
}

// ---------------------------------------------------------------------------------------------
// Mount TOPOLOGY as state (B5 residual closed): runtime mounts survive a restart
// ---------------------------------------------------------------------------------------------

/// The topology lifecycle at the arms: a successful mount RECORDS its spec in the sidecar, a
/// refused mount records NOTHING, and unmount is the one verb that FORGETS the record — while a
/// core drop without teardown (the kill shape) leaves it on disk, which is the whole point.
#[test]
fn mount_records_topology_and_only_unmount_forgets_it() {
    let step = Arc::new(TestMutex::new(Step::Idle));
    let fills = Arc::new(TestMutex::new(Vec::new()));
    let loaded = Arc::new(TestMutex::new(Vec::new()));
    let dir = state_dir();
    let mut core = core_with(CoreConfig {
        seed_cash: 10_000.0,
        state_dir: Some(dir.clone()),
        strategy_factory: Some(quoter_factory("RT", &step, &fills, None, &loaded)),
        ..CoreConfig::default()
    });

    // A REFUSED mount records nothing (unknown venue — the arm returns before step (7)).
    core.dispatch(mount_cmd(MountSpec { venue: "okx".into(), ..spec("rt-a") }));
    assert!(crate::mount_topology::read(&dir).is_empty(), "a refusal records no topology");

    core.dispatch(mount_cmd(spec("rt-a")));
    let records = crate::mount_topology::read(&dir);
    assert_eq!(records.len(), 1, "a successful mount records its spec");
    assert_eq!(records[0].mount_id(), "rt_a");
    assert_eq!(records[0].spec.name.as_deref(), Some("test_quoter"));

    core.dispatch(unmount_cmd("rt-a"));
    assert!(
        crate::mount_topology::read(&dir).is_empty(),
        "unmount removes the record (atomic rewrite)"
    );

    // Re-mount and DROP the core without teardown — the kill shape (`run()`'s teardown never ran
    // here at all; a white-box `CoreThread` drop runs no save/cancel/remove logic). The record
    // must survive the drop: only the explicit unmount verb forgets a runtime mount.
    core.dispatch(mount_cmd(spec("rt-a")));
    drop(core);
    assert_eq!(
        crate::mount_topology::read(&dir).len(),
        1,
        "a dropped-without-teardown core leaves the record for the restart to resurrect"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// **The residual, closed.** Mount at runtime → periodic state save → KILL (drop without
/// teardown) → a restart-shaped rebuild replays the topology sidecar through the SAME
/// `Command::MountStrategy` dispatch a wire mount takes — and the resurrected mount loads the
/// state sidecar through the mount arm's existing step (4), pinning the resurrect-then-load
/// ordering the daemon relies on.
#[test]
fn restart_shaped_rebuild_resurrects_the_mount_and_loads_its_state() {
    let step = Arc::new(TestMutex::new(Step::Idle));
    let fills = Arc::new(TestMutex::new(Vec::new()));
    let loaded = Arc::new(TestMutex::new(Vec::new()));
    let dir = state_dir();
    let saved = serde_json::json!({"acc": 7});
    let mut core = core_with(CoreConfig {
        seed_cash: 10_000.0,
        state_dir: Some(dir.clone()),
        strategy_factory: Some(quoter_factory("A", &step, &fills, Some(saved.clone()), &loaded)),
        ..CoreConfig::default()
    });
    core.dispatch(mount_cmd(spec("rt-a")));
    // The periodic save (`state_save`'s timer body — the same call `run()`'s teardown makes)
    // wrote the state sidecar sometime before the crash.
    core.save_all_strategy_state();
    assert!(
        crate::strategy_state::read_json(&crate::strategy_state::sidecar_path(&dir, "rt_a"))
            .is_some(),
        "the state sidecar exists before the kill"
    );
    drop(core); // the kill: no teardown, no unmount — the topology record stays.

    // The restart-shaped rebuild: a FRESH core (same state_dir, same factory), zero mounts —
    // then the daemon-side replay: every topology record re-sent through the SAME dispatch path.
    let loaded2 = Arc::new(TestMutex::new(Vec::new()));
    let mut rebuilt = core_with(CoreConfig {
        seed_cash: 10_000.0,
        state_dir: Some(dir.clone()),
        strategy_factory: Some(quoter_factory("A2", &step, &fills, None, &loaded2)),
        ..CoreConfig::default()
    });
    assert!(rebuilt.mounts.is_empty(), "spawned mount-less, exactly like the daemon's core");
    for record in crate::mount_topology::read(&dir) {
        rebuilt.dispatch(mount_cmd(record.spec));
    }

    assert_eq!(rebuilt.mounts.len(), 1, "the runtime mount survived the restart");
    assert!(rebuilt.mounts[0].is_some());
    assert_eq!(rebuilt.mount_ids[0], "rt_a", "same identity — sidecars and provenance line up");
    assert!(noted(&rebuilt, "MOUNTED strategy `rt_a`"), "recent: {:?}", rebuilt.recent);
    assert_eq!(
        loaded2.lock().unwrap().clone(),
        vec![saved],
        "the resurrected mount loaded its saved state through the mount arm's existing load path"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A STALE record — one whose spec the factory no longer resolves — refuses with a note and
/// skips, exactly like any other bad mount command; the good record beside it still mounts and
/// the core keeps working. Never a startup failure.
#[test]
fn a_stale_topology_record_refuses_and_the_rest_still_resurrect() {
    let step = Arc::new(TestMutex::new(Step::Idle));
    let fills = Arc::new(TestMutex::new(Vec::new()));
    let loaded = Arc::new(TestMutex::new(Vec::new()));
    let dir = state_dir();
    // Plant the sidecar by hand: a good record plus a stale one (the test factory refuses a spec
    // naming neither `name` nor `rhai` — the shape of "this strategy no longer exists").
    crate::mount_topology::upsert(
        &dir,
        crate::mount_topology::MountRecord::stamped(spec("rt-good"), 1_700_000_000_000),
    )
    .unwrap();
    crate::mount_topology::upsert(
        &dir,
        crate::mount_topology::MountRecord::stamped(
            MountSpec { name: None, rhai: None, ..spec("rt-stale") },
            1_700_000_000_000,
        ),
    )
    .unwrap();

    let mut core = core_with(CoreConfig {
        seed_cash: 10_000.0,
        state_dir: Some(dir.clone()),
        strategy_factory: Some(quoter_factory("RT", &step, &fills, None, &loaded)),
        ..CoreConfig::default()
    });
    for record in crate::mount_topology::read(&dir) {
        core.dispatch(mount_cmd(record.spec));
    }

    assert_eq!(core.mounts.len(), 1, "the good record mounted; the stale one skipped");
    assert_eq!(core.mount_ids[0], "rt_good");
    assert!(
        noted(&core, "MOUNT REFUSED: `rt_stale`"),
        "the stale record's refusal is a note, never a failure: {:?}",
        core.recent
    );
    // ...and the core is still fully operational: a fresh mount lands.
    core.dispatch(mount_cmd(spec("rt-b")));
    assert_eq!(core.mounts.len(), 2);
    let _ = std::fs::remove_dir_all(&dir);
}
