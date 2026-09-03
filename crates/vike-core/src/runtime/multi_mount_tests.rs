//! Multi-mount correctness tests (bugs A/B + gap C) — the regression gates for the three ways two
//! strategies mounted over ONE account used to interfere with each other:
//!
//! - **bug A, cross-mount tag collision**: `strategy_tags` was keyed `{venue}|{symbol}|{tag}` while
//!   tags are strategy-LOCAL names (`vike-mm` emits the literals `"bid"`/`"ask"`), so mount B's
//!   tagged submit OVERWROTE mount A's coid and A's next `cancel_tagged("bid")` canceled B's
//!   resting order.
//! - **bug B, fill/order-event misrouting**: `on_fill`/`on_order_event` picked the mount by FIRST
//!   MATCH on `(venue, symbol)`, so with two mounts on one symbol only the first ever heard
//!   anything — including about its sibling's orders.
//! - **gap C, mount identity**: `mount_id_of` derives `{venue}__{symbol}__{interval}`, which two
//!   mounts on one triple SHARE (one state sidecar, one journal identity, one budget key).
//!
//! White-box, in-crate: these assert on the runtime's private `strategy_tags`/`coid_mount`/
//! `mount_ids` state, which is exactly where each bug lived. `use super::*` re-exports the runtime
//! module's items (the sibling-test-module idiom of `safe_state_tests.rs`).

use super::*;
use std::sync::Mutex as TestMutex;
use vike_exec::testing::RecordingClient;
use vike_exec::{Account, RiskGate, RiskLimits};
use vike_model::events::{FillEvent, TradeId};

/// Assemble a `CoreThread<RecordingClient>` from a caller-supplied config, the way `spawn_core`
/// does minus the OS thread (the `safe_state_tests::core_with` twin — duplicated rather than shared
/// because a private helper in a sibling test module is not reachable from here).
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

/// What one mount's strategy does on the next `on_feed_status` dispatch — the per-mount script that
/// lets ONE driver call (`drive_strategy_feed_status` fans out to EVERY mount on the pair, each
/// with its own index) exercise two mounts independently.
#[derive(Clone, Copy)]
enum Step {
    /// submit a tagged limit under the literal tag `"bid"` (the `vike-mm` maker's own tag)
    Quote,
    /// pull that quote by tag — the verb bug A corrupted
    Pull,
    /// do nothing (leave any resting quote alone)
    Idle,
}

/// A two-mount test strategy: quotes / pulls / idles per its OWN `step` cell, and records every
/// `on_fill` it receives under its own label so a test can prove WHICH mount heard a fill.
struct Quoter {
    label: &'static str,
    step: Arc<TestMutex<Step>>,
    fills: Arc<TestMutex<Vec<(&'static str, f64)>>>,
}

impl Strategy<LiveBroker> for Quoter {
    fn on_feed_status(&mut self, broker: &mut LiveBroker, _status: FeedStatus) {
        match *self.step.lock().unwrap() {
            Step::Quote => broker.submit_limit_tagged("bid", 1, 1.0, 100.0),
            Step::Pull => broker.cancel_tagged("bid"),
            Step::Idle => {}
        }
    }

    fn on_fill(&mut self, _broker: &mut LiveBroker, fill: &Fill) {
        self.fills.lock().unwrap().push((self.label, fill.size));
    }
}

/// One mount on `(sim, <symbol>, 1m)` carrying its OWN `controller_id` — most tests here pass the
/// SAME triple for both mounts, which is what makes `controller_id` load-bearing (gap C) and what
/// reproduces bugs A and B.
fn quoter_mount(
    label: &'static str,
    symbol: &str,
    controller_id: Option<&str>,
    step: &Arc<TestMutex<Step>>,
    fills: &Arc<TestMutex<Vec<(&'static str, f64)>>>,
) -> StrategyMount {
    StrategyMount {
        account: None,
        symbols: Vec::new(),
        controller_id: controller_id.map(str::to_string),
        underlying_symbol: None,
        venue: "sim".into(),
        symbol: symbol.into(),
        interval: "1m".into(),
        strategy: Box::new(Quoter { label, step: Arc::clone(step), fills: Arc::clone(fills) }),
    }
}

/// Two mounts on ONE `(venue, symbol, interval)`, distinguished by each mount's own
/// `controller_id`, each driven by its own [`Step`] cell and both quoting under the literal tag
/// `"bid"`.
fn two_quoter_mounts(
    step_a: &Arc<TestMutex<Step>>,
    step_b: &Arc<TestMutex<Step>>,
    fills: &Arc<TestMutex<Vec<(&'static str, f64)>>>,
) -> CoreConfig {
    CoreConfig {
        seed_cash: 10_000.0,
        strategy: Some(quoter_mount("A", "BTCUSDT", Some("maker-a"), step_a, fills)),
        extra_mounts: vec![quoter_mount("B", "BTCUSDT", Some("maker-b"), step_b, fills)],
        ..CoreConfig::default()
    }
}

/// A fill for `coid` on (sim, BTCUSDT). Distinct `trade_id` per coid/side/px so the account's
/// per-trade dedup never collapses two injected fills.
fn coid_fill(coid: &str, side: i32, qty: f64, px: f64) -> FillEvent {
    coid_fill_fee(coid, side, qty, px, 0.0)
}

/// [`coid_fill`] with an explicit commission — the residual invariant is stated NET of fees, so it
/// needs a fill that actually pays some.
fn coid_fill_fee(coid: &str, side: i32, qty: f64, px: f64, commission: f64) -> FillEvent {
    FillEvent {
        // minted by this helper — same `t-<coid>-<side>-<px>` bytes as the `format!` it replaced
        trade_id: TradeId::prefixed("t-", format_args!("{coid}-{side}-{px}")),
        client_order_id: coid.to_string(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side,
        last_qty: qty,
        last_px: px,
        commission,
        commission_asset: String::new().into(),
        liquidity_side: "taker".to_string().into(),
        ts: 0,
        mark_price: None,
        position_side: "BOTH".into(),
    }
}

/// The coid `mount_idx` owns (its first, in insertion order) — the strategy-submit path tags it.
fn coid_of(core: &CoreThread<RecordingClient>, mount_idx: usize) -> String {
    core.coid_mount
        .iter()
        .find(|(_, &m)| m == mount_idx)
        .map(|(c, _)| c.clone())
        .expect("mount submitted at least one order")
}

/// **BUG A regression.** Two mounts on one (venue, symbol) both rest a quote under the tag `"bid"`;
/// mount A then pulls `"bid"` while B idles. A's OWN order must be canceled and B's must be left
/// RESTING.
///
/// Pre-fix, the shared `sim|BTCUSDT|bid` key meant B's submit overwrote A's coid, so A's pull
/// canceled B's resting order — one strategy cancelling another strategy's live quote.
#[test]
fn two_mounts_same_symbol_tags_do_not_collide() {
    let step_a = Arc::new(TestMutex::new(Step::Quote));
    let step_b = Arc::new(TestMutex::new(Step::Quote));
    let fills = Arc::new(TestMutex::new(Vec::new()));
    let mut core = core_with(two_quoter_mounts(&step_a, &step_b, &fills));

    // both mounts quote (one driver call fans out to every mount on the pair)
    core.drive_strategy_feed_status("sim", "BTCUSDT", FeedStatus::Live);
    let coid_a = coid_of(&core, 0);
    let coid_b = coid_of(&core, 1);
    assert_ne!(coid_a, coid_b, "each mount minted its own resting order");
    assert_eq!(
        core.strategy_tags.len(),
        2,
        "one tag entry PER MOUNT, not one shared entry: {:?}",
        core.strategy_tags
    );
    assert_eq!(core.strategy_tags.get("0|sim|BTCUSDT|bid"), Some(&coid_a));
    assert_eq!(core.strategy_tags.get("1|sim|BTCUSDT|bid"), Some(&coid_b));

    // A pulls its quote; B idles (its quote must stay resting).
    *step_a.lock().unwrap() = Step::Pull;
    *step_b.lock().unwrap() = Step::Idle;
    core.drive_strategy_feed_status("sim", "BTCUSDT", FeedStatus::Disconnected);

    assert!(
        core.engine.client.cancels.contains(&coid_a),
        "mount A's pull must cancel A's OWN order: {:?}",
        core.engine.client.cancels
    );
    assert!(
        !core.engine.client.cancels.contains(&coid_b),
        "mount B's resting quote must be untouched by A's pull: {:?}",
        core.engine.client.cancels
    );
}

/// **BUG B regression.** A fill for each mount's own coid reaches THAT mount's `on_fill`. Pre-fix
/// the first-match-on-(venue, symbol) routing delivered BOTH fills to mount A, and mount B never
/// heard about its own execution.
#[test]
fn two_mounts_same_symbol_both_receive_fills() {
    let step_a = Arc::new(TestMutex::new(Step::Quote));
    let step_b = Arc::new(TestMutex::new(Step::Quote));
    let fills = Arc::new(TestMutex::new(Vec::new()));
    let mut core = core_with(two_quoter_mounts(&step_a, &step_b, &fills));
    core.engine.collect_applied_fills = true;

    core.drive_strategy_feed_status("sim", "BTCUSDT", FeedStatus::Live);
    let coid_a = coid_of(&core, 0);
    let coid_b = coid_of(&core, 1);
    // no re-quoting from the on_fill drain — keep the assertion about deliveries only
    *step_a.lock().unwrap() = Step::Idle;
    *step_b.lock().unwrap() = Step::Idle;

    core.bus.publish(Event::Fill(coid_fill(&coid_a, 1, 3.0, 100.0)), &mut core.engine);
    core.bus.publish(Event::Fill(coid_fill(&coid_b, 1, 7.0, 100.0)), &mut core.engine);
    core.dispatch_applied_fills();

    let got = fills.lock().unwrap().clone();
    assert_eq!(
        got,
        vec![("A", 3.0), ("B", 7.0)],
        "each mount heard exactly its OWN fill, in fold order: {got:?}"
    );
    // ...and the attribution ledger agrees with the callback routing (they share the coid key)
    assert_eq!(core.mount_attr[0].size.to_bits(), 3.0_f64.to_bits());
    assert_eq!(core.mount_attr[1].size.to_bits(), 7.0_f64.to_bits());
}

/// A fill whose coid NO mount minted (an operator ticket, a liquidation, a reconcile-adopted venue
/// order) keeps the historical behavior: it falls back to the FIRST mount on (venue, symbol) and is
/// attributed to no ledger at all.
#[test]
fn unminted_coid_falls_back_to_triple_match() {
    let step_a = Arc::new(TestMutex::new(Step::Idle));
    let step_b = Arc::new(TestMutex::new(Step::Idle));
    let fills = Arc::new(TestMutex::new(Vec::new()));
    let mut core = core_with(two_quoter_mounts(&step_a, &step_b, &fills));
    core.engine.collect_applied_fills = true;

    core.bus.publish(Event::Fill(coid_fill("manual-ticket", 1, 2.0, 100.0)), &mut core.engine);
    core.dispatch_applied_fills();

    let got = fills.lock().unwrap().clone();
    assert_eq!(got, vec![("A", 2.0)], "an unminted coid falls back to the first mount: {got:?}");
    assert_eq!(core.mount_attr[0].size.to_bits(), 0.0_f64.to_bits(), "attributed to NO ledger");
    assert_eq!(core.mount_attr[1].size.to_bits(), 0.0_f64.to_bits());
}

/// **GAP C.** Two mounts that would derive the SAME `mount_id` are a configuration fault: assembly
/// panics at mount time rather than silently sharing one state sidecar / journal identity / budget
/// key. Distinct `controller_id`s are the cure (every other test in this file relies on that).
#[test]
#[should_panic(expected = "duplicate strategy-mount id")]
fn duplicate_controller_id_fails_mount_assembly() {
    let step = Arc::new(TestMutex::new(Step::Idle));
    let fills = Arc::new(TestMutex::new(Vec::new()));
    let config = CoreConfig {
        seed_cash: 10_000.0,
        // the SAME id on both mounts — exactly what the assert exists to catch (an ABSENT id would
        // collide the same way here, since both mounts sit on one (venue, symbol, interval))
        strategy: Some(quoter_mount("A", "BTCUSDT", Some("maker"), &step, &fills)),
        extra_mounts: vec![quoter_mount("B", "BTCUSDT", Some("maker"), &step, &fills)],
        ..CoreConfig::default()
    };
    let _ = core_with(config);
}

/// The byte-identical guarantee: a mount with NO controller id derives exactly today's
/// `{venue}__{symbol}__{interval}` id — the id its state sidecar, its journal `mount_id` provenance
/// and its budget/schedule keys have always been written under.
#[test]
fn absent_controller_id_derives_legacy_mount_id() {
    let step = Arc::new(TestMutex::new(Step::Idle));
    let fills = Arc::new(TestMutex::new(Vec::new()));
    let config = CoreConfig {
        seed_cash: 10_000.0,
        strategy: Some(quoter_mount("A", "BTCUSDT", None, &step, &fills)),
        ..CoreConfig::default()
    };
    let core = core_with(config);
    assert_eq!(
        core.mount_ids,
        vec![crate::strategy_state::mount_id_of("sim", "BTCUSDT", "1m")],
        "no controller id ⇒ the legacy derivation, unchanged"
    );

    // ...and naming only SOME mounts leaves every un-named one derived (the mixed config).
    let config = CoreConfig {
        seed_cash: 10_000.0,
        strategy: Some(quoter_mount("A", "BTCUSDT", Some("maker-a"), &step, &fills)),
        extra_mounts: vec![quoter_mount("B", "ETHUSDT", None, &step, &fills)],
        ..CoreConfig::default()
    };
    let core = core_with(config);
    assert_eq!(
        core.mount_ids,
        vec!["maker_a".to_string(), crate::strategy_state::mount_id_of("sim", "ETHUSDT", "1m")],
        "an un-named mount keeps the derived id"
    );
}

/// **THE REASON `controller_id` LIVES ON THE MOUNT.** A mount id is BOTH a state-sidecar FILENAME
/// and the journal attribution key, so it must travel WITH the mount. Assemble three named mounts,
/// then assemble the same three with `extra_mounts` in the OPPOSITE order: each mount must keep
/// ITS OWN id, i.e. the (symbol -> mount_id) pairing is invariant under reordering.
///
/// The predecessor shape — a POSITIONAL `CoreConfig::controller_ids` vec indexed by assembly order —
/// fails this: run 2 would hand the `SOLUSDT` mount `maker_b` and the `ETHUSDT` mount `maker_c`, so
/// on the next start each strategy would load its SIBLING's durable state out of the other's
/// sidecar file. Silently.
#[test]
fn reordering_extra_mounts_preserves_each_mount_id() {
    let step = Arc::new(TestMutex::new(Step::Idle));
    let fills = Arc::new(TestMutex::new(Vec::new()));

    // (symbol, mount_id) for every assembled mount — the pairing that must not drift.
    let pairs = |core: &CoreThread<RecordingClient>| -> Vec<(String, String)> {
        core.mounts
            .iter()
            .flatten()
            .map(|m| m.symbol.clone())
            .zip(core.mount_ids.iter().cloned())
            .collect()
    };

    let core = core_with(CoreConfig {
        seed_cash: 10_000.0,
        strategy: Some(quoter_mount("A", "BTCUSDT", Some("maker-a"), &step, &fills)),
        extra_mounts: vec![
            quoter_mount("B", "ETHUSDT", Some("maker-b"), &step, &fills),
            quoter_mount("C", "SOLUSDT", Some("maker-c"), &step, &fills),
        ],
        ..CoreConfig::default()
    });
    assert_eq!(
        pairs(&core),
        vec![
            ("BTCUSDT".to_string(), "maker_a".to_string()),
            ("ETHUSDT".to_string(), "maker_b".to_string()),
            ("SOLUSDT".to_string(), "maker_c".to_string()),
        ],
    );

    // the SAME three mounts, `extra_mounts` reversed
    let core = core_with(CoreConfig {
        seed_cash: 10_000.0,
        strategy: Some(quoter_mount("A", "BTCUSDT", Some("maker-a"), &step, &fills)),
        extra_mounts: vec![
            quoter_mount("C", "SOLUSDT", Some("maker-c"), &step, &fills),
            quoter_mount("B", "ETHUSDT", Some("maker-b"), &step, &fills),
        ],
        ..CoreConfig::default()
    });
    assert_eq!(
        pairs(&core),
        vec![
            ("BTCUSDT".to_string(), "maker_a".to_string()),
            ("SOLUSDT".to_string(), "maker_c".to_string()),
            ("ETHUSDT".to_string(), "maker_b".to_string()),
        ],
        "each mount kept ITS OWN id — the id travels with the mount, not with the slot index"
    );
}

/// **GAP E.** The published ledger CLOSES: `Σ (mount rows) + residual == account realized, net of
/// fees` — including a deliberately UNATTRIBUTED round trip (an operator ticket / liquidation /
/// adopted order), which is the entire reason the residual row exists.
///
/// Pre-fix, that unattributed PnL was simply absent from every mount row with nothing to catch it,
/// so `Σ MountView.realized_pnl != Account.realized_pnl` systematically and silently.
#[test]
fn residual_equals_account_minus_mounts() {
    let step_a = Arc::new(TestMutex::new(Step::Quote));
    let step_b = Arc::new(TestMutex::new(Step::Quote));
    let fills = Arc::new(TestMutex::new(Vec::new()));
    let mut core = core_with(two_quoter_mounts(&step_a, &step_b, &fills));
    core.engine.collect_applied_fills = true;

    core.drive_strategy_feed_status("sim", "BTCUSDT", FeedStatus::Live);
    let coid_a = coid_of(&core, 0);
    *step_a.lock().unwrap() = Step::Idle;
    *step_b.lock().unwrap() = Step::Idle;

    // mount A round-trips 3 @ 100 -> 110 (+30 realized, attributed)...
    core.coid_mount.insert("cA-close".to_string(), 0);
    core.bus.publish(Event::Fill(coid_fill(&coid_a, 1, 3.0, 100.0)), &mut core.engine);
    core.bus.publish(Event::Fill(coid_fill("cA-close", -1, 3.0, 110.0)), &mut core.engine);
    // ...and an OPERATOR ticket round-trips 1 @ 100 -> 120 (+20 realized, 2.0 of fees), attributed
    // to no mount at all: its coids were never minted by one.
    core.bus.publish(Event::Fill(coid_fill_fee("manual-1", 1, 1.0, 100.0, 1.0)), &mut core.engine);
    core.bus.publish(Event::Fill(coid_fill_fee("manual-2", -1, 1.0, 120.0, 1.0)), &mut core.engine);
    core.dispatch_applied_fills();

    let views = core.mount_views();
    let residual = views.last().expect("a mount exists, so the residual row is published");
    assert_eq!(
        residual.kind,
        crate::snapshot::MountRowKind::Residual,
        "the residual row is LAST: {views:?}"
    );
    let mount_rows: Vec<_> =
        views.iter().filter(|v| v.kind == crate::snapshot::MountRowKind::Mount).collect();
    assert_eq!(mount_rows.len(), 2, "one row per mount, then the residual");

    let account_net = core.engine.account.realized_pnl - core.engine.account.fees_paid;
    let attributed: f64 = mount_rows.iter().map(|v| v.realized_pnl).sum();
    assert!(
        (attributed + residual.realized_pnl - account_net).abs() < 1e-9,
        "THE INVARIANT: mounts {attributed} + residual {} != account {account_net}",
        residual.realized_pnl
    );
    // the concrete numbers, so a sign/fee slip cannot hide behind the invariant
    assert!((attributed - 30.0).abs() < 1e-9, "mount A's +30 is the only attributed PnL");
    assert!(
        (residual.realized_pnl - 18.0).abs() < 1e-9,
        "the unattributed +20 net of its 2.0 fees: {}",
        residual.realized_pnl
    );
    // the residual is account-wide, not a series, and carries no position/budget
    assert!(residual.venue.is_empty() && residual.symbol.is_empty());
    assert_eq!(residual.position.to_bits(), 0.0_f64.to_bits());
    assert!(residual.budget.is_none() && !residual.latched);
}

/// A mount-FREE core publishes an EMPTY `mounts` vec, exactly as before — the residual row appears
/// only where there are mounts to reconcile against.
#[test]
fn no_mounts_publishes_no_residual_row() {
    let core = core_with(CoreConfig { seed_cash: 10_000.0, ..CoreConfig::default() });
    assert!(core.mount_views().is_empty(), "no mounts ⇒ no rows at all, residual included");
}

// ---- coid_mount is BOUNDED: a terminal order's entry retires after the linger ----

/// Drive `coid` to a TERMINAL FSM state through a real venue event. `OrderRejected` is the one
/// terminal the FSM accepts straight from `Initialized` — `RecordingClient` never emits the
/// `OrderSubmitted`/`OrderAccepted` a live adapter would, so a cancel would be an invalid
/// transition here. It reaches `dispatch_order_events` as `OrderEventKind::Rejected`, which is what
/// the prune queue keys off.
fn terminalize(core: &mut CoreThread<RecordingClient>, coid: &str) {
    core.bus.publish(
        Event::OrderRejected(vike_model::events::OrderRejected {
            client_order_id: coid.to_string(),
            reason: "venue said no".into(),
            ts: 0,
        }),
        &mut core.engine,
    );
}

/// **THE BOUND.** `coid_mount` is written once per mount-minted order and used to be erased NEVER,
/// so a maker re-quoting a few times a second grew it for the life of the process — and a restart
/// made it worse, since `replay::fold_coid_mounts` rebuilds the map across the WHOLE readable record
/// set rather than just the tail.
///
/// A terminal order's entry now RETIRES — but only after [`COID_PRUNE_LINGER_MS`], never on the
/// terminal itself (see `note_terminal_coid`). A sibling mount's still-live order is untouched.
#[test]
fn a_terminal_orders_attribution_entry_is_pruned_after_the_linger() {
    let step_a = Arc::new(TestMutex::new(Step::Quote));
    let step_b = Arc::new(TestMutex::new(Step::Quote));
    let fills = Arc::new(TestMutex::new(Vec::new()));
    let mut core = core_with(two_quoter_mounts(&step_a, &step_b, &fills));
    core.engine.collect_applied_fills = true;

    core.drive_strategy_feed_status("sim", "BTCUSDT", FeedStatus::Live);
    let coid_a = coid_of(&core, 0);
    let coid_b = coid_of(&core, 1);
    *step_a.lock().unwrap() = Step::Idle;
    *step_b.lock().unwrap() = Step::Idle;

    // A's order goes terminal. The entry must SURVIVE its own terminal — a late fill is still
    // possible, and an unattributed fill silently books into the residual row.
    terminalize(&mut core, &coid_a);
    core.dispatch_applied_fills();
    assert!(
        core.coid_mount.contains_key(&coid_a),
        "the entry must outlive the terminal itself — that window is what protects a late fill"
    );
    assert_eq!(core.coid_terminal.len(), 1, "queued for retirement, not erased");

    // ...still there just INSIDE the window (non-vacuity: the window really is a window)...
    core.engine.now_ms = COID_PRUNE_LINGER_MS - 1;
    core.dispatch_applied_fills();
    assert!(core.coid_mount.contains_key(&coid_a), "one millisecond short must not retire it");

    // ...and gone once it has aged out.
    core.engine.now_ms = COID_PRUNE_LINGER_MS;
    core.dispatch_applied_fills();
    assert!(!core.coid_mount.contains_key(&coid_a), "the aged-out entry is retired");
    assert!(core.coid_terminal.is_empty(), "and its queue slot with it");
    assert!(
        core.coid_mount.contains_key(&coid_b),
        "a SIBLING mount's still-live order keeps its attribution: {:?}",
        core.coid_mount
    );
}

/// **WHY THE LINGER EXISTS.** `Event::Fill` and the FSM terminal that accompanies it are separate
/// events with no ordering contract, and `Account` folds a fill regardless of FSM state — so a
/// partial fill racing its own cancel ack, or a reconnect re-delivering executions, arrives AFTER
/// the order is terminal. Pruning eagerly would make that fill ownerless, and an ownerless fill
/// books into the RESIDUAL row instead of the mount that traded it.
///
/// Terminalize, let the clock run to one millisecond INSIDE the window, then fill: it still books to
/// the mount.
#[test]
fn a_fill_arriving_after_the_terminal_is_still_attributed() {
    let step_a = Arc::new(TestMutex::new(Step::Quote));
    let step_b = Arc::new(TestMutex::new(Step::Idle));
    let fills = Arc::new(TestMutex::new(Vec::new()));
    let mut core = core_with(two_quoter_mounts(&step_a, &step_b, &fills));
    core.engine.collect_applied_fills = true;

    core.drive_strategy_feed_status("sim", "BTCUSDT", FeedStatus::Live);
    let coid_a = coid_of(&core, 0);
    *step_a.lock().unwrap() = Step::Idle;

    terminalize(&mut core, &coid_a);
    core.dispatch_applied_fills();
    core.engine.now_ms = COID_PRUNE_LINGER_MS - 1; // still inside the window

    core.bus.publish(Event::Fill(coid_fill(&coid_a, 1, 4.0, 100.0)), &mut core.engine);
    core.dispatch_applied_fills();

    assert_eq!(
        core.mount_attr[0].size.to_bits(),
        4.0_f64.to_bits(),
        "a fill landing after its order's terminal still folds into the ORIGINATING mount's ledger"
    );
    assert_eq!(
        fills.lock().unwrap().clone(),
        vec![("A", 4.0)],
        "...and still reaches that mount's on_fill"
    );
}

// ---- budgets and schedules key on the MOUNT ID, so a same-triple pair can be given two ----

/// **THE RE-KEY.** Two mounts on ONE `(venue, symbol, interval)` — the exact configuration
/// `controller_id` exists to legitimize — get INDEPENDENT budgets.
///
/// Keyed on the triple (as `mount_budgets` used to be), both mounts resolved the SAME entry: each
/// latched on its own attributed loss, but against a cap neither could be given independently, and
/// giving one mount a budget silently gave it to the other. There was no spelling of "mount A caps
/// at 100, mount B is unbudgeted" at all.
#[test]
fn two_mounts_on_one_triple_get_independent_budgets() {
    let step = Arc::new(TestMutex::new(Step::Idle));
    let fills = Arc::new(TestMutex::new(Vec::new()));
    let mut budgets = std::collections::HashMap::new();
    // NB the SANITIZED ids — `mount_id_with` turns `maker-a` into `maker_a`.
    budgets.insert(
        "maker_a".to_string(),
        MountBudget { max_loss: Some(100.0), ..MountBudget::default() },
    );
    budgets.insert(
        "maker_b".to_string(),
        MountBudget { max_notional: Some(7.0), flatten_on_breach: true, ..MountBudget::default() },
    );
    let core = core_with(CoreConfig {
        seed_cash: 10_000.0,
        strategy: Some(quoter_mount("A", "BTCUSDT", Some("maker-a"), &step, &fills)),
        extra_mounts: vec![quoter_mount("B", "BTCUSDT", Some("maker-b"), &step, &fills)],
        mount_budgets: budgets,
        ..CoreConfig::default()
    });

    assert_eq!(
        core.mount_budget[0],
        Some(MountBudget { max_loss: Some(100.0), max_notional: None, flatten_on_breach: false }),
        "mount A resolved ITS OWN budget"
    );
    assert_eq!(
        core.mount_budget[1],
        Some(MountBudget { max_loss: None, max_notional: Some(7.0), flatten_on_breach: true }),
        "mount B resolved a DIFFERENT budget — under the old triple key both read the same entry"
    );
}

/// The half of the re-key that a same-entry map cannot even express: budget ONE of two same-triple
/// mounts. The other must come back unbudgeted, not silently inherit its sibling's cap.
#[test]
fn budgeting_one_of_two_same_triple_mounts_leaves_the_other_free() {
    let step = Arc::new(TestMutex::new(Step::Idle));
    let fills = Arc::new(TestMutex::new(Vec::new()));
    let mut budgets = std::collections::HashMap::new();
    budgets
        .insert("maker_a".to_string(), MountBudget { max_loss: Some(50.0), ..Default::default() });
    let core = core_with(CoreConfig {
        seed_cash: 10_000.0,
        strategy: Some(quoter_mount("A", "BTCUSDT", Some("maker-a"), &step, &fills)),
        extra_mounts: vec![quoter_mount("B", "BTCUSDT", Some("maker-b"), &step, &fills)],
        mount_budgets: budgets,
        ..CoreConfig::default()
    });
    assert!(core.mount_budget[0].is_some(), "the budgeted mount");
    assert_eq!(core.mount_budget[1], None, "its same-triple sibling must NOT inherit the cap");
}

/// The SCHEDULE twin of the budget re-key: two mounts on one triple get INDEPENDENT wall-clock
/// schedules. Under the old triple key the map held one entry, so the first mount consumed it
/// (`remove`) and the second silently got an empty schedule — its rules could never fire.
#[test]
fn two_mounts_on_one_triple_get_independent_schedules() {
    let step = Arc::new(TestMutex::new(Step::Idle));
    let fills = Arc::new(TestMutex::new(Vec::new()));
    let mut a = LiveSchedule::new();
    a.on(crate::schedule::TimeRule::daily_at_utc(0, 0), "midnight");
    let mut b = LiveSchedule::new();
    b.on(crate::schedule::TimeRule::daily_at_utc(12, 0), "noon");
    b.on(crate::schedule::TimeRule::daily_at_utc(18, 0), "close");
    let mut schedules = std::collections::HashMap::new();
    schedules.insert("maker_a".to_string(), a);
    schedules.insert("maker_b".to_string(), b);

    let core = core_with(CoreConfig {
        seed_cash: 10_000.0,
        strategy: Some(quoter_mount("A", "BTCUSDT", Some("maker-a"), &step, &fills)),
        extra_mounts: vec![quoter_mount("B", "BTCUSDT", Some("maker-b"), &step, &fills)],
        mount_schedules: schedules,
        ..CoreConfig::default()
    });

    assert_eq!(core.mount_schedule.len(), 2);
    assert_eq!(core.mount_schedule[0].len(), 1, "mount A kept its one rule");
    assert_eq!(
        core.mount_schedule[1].len(),
        2,
        "mount B kept ITS OWN two rules — under the old triple key it got an empty schedule, \
         because the first mount had already consumed the single entry"
    );
    assert!(core.any_mount_schedule, "both schedules are live");
}
