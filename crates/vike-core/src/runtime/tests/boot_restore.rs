//! The ASSEMBLY half of the boot restore (`crate::runtime::boot_restore`, decision 0127), white-box:
//! a core assembled over what `crate::replay::BootRestore::apply_to` would put on its config. The
//! engine snapshots are built by hand from engines the test folds itself, so each case states the
//! journal it stands for.

use super::order_owners_tests::{Events, assembled, cell, config_on, noted, spy_mount};
use super::restore_tests::{heard, open, report, rest_one, run};
use super::*;
use crate::order_owners::ORDER_OWNERS_FILE;
use crate::replay::BootRestore;
use crate::runtime::test_support::{core_of, engine_on};
use crate::scratch::Scratch;
use vike_exec::testing::RecordingClient;

type Core = CoreThread<RecordingClient>;

/// An engine on `sim`/`BTCUSDT` routed as `route_key` (the venue for the default account).
fn engine(route_key: &str) -> ExecutionEngine<RecordingClient> {
    let mut e = engine_on("sim", "BTCUSDT", RecordingClient::default());
    e.route_key = route_key.to_string();
    e
}

/// The `Snap` entry of `route_key`'s engine after it folded a `size` position, one fill id and a
/// trading state — what a journal of that engine carries.
fn journaled(route_key: &str, size: f64, state: TradingState) -> vike_exec::EngineSnapshot {
    let mut e = engine(route_key);
    e.account.positions.insert(
        ("sim".into(), "BTCUSDT".into(), vike_model::events::PositionSide::Both),
        vike_exec::PositionEntry { size, avg_px: 100.0, ..Default::default() },
    );
    e.seed_seen_trade_ids([format!("t-{route_key}")]);
    e.trading_state = state;
    e.snapshot_state()
}

/// A two-account core (`sim` and `sim#ALT`) assembled over `restored`.
fn two_accounts(restored: Vec<vike_exec::EngineSnapshot>) -> Core {
    core_of(
        engine("sim"),
        vec![(1_000.0, engine("sim#ALT"))],
        CoreConfig { seed_cash: 1_000.0, restored_engines: restored, ..CoreConfig::default() },
    )
}

/// **Each journaled engine goes back onto the engine with ITS route key** — positions and fill
/// dedup together — whatever order the snapshots come in.
#[test]
fn a_multi_engine_journal_restores_each_engine_by_route_key() {
    let core = two_accounts(vec![
        journaled("sim#ALT", -2.0, TradingState::Active),
        journaled("sim", 3.0, TradingState::Active),
    ]);
    assert_eq!(
        core.engine.position_size(vike_model::events::PositionSide::Both),
        3.0,
        "the default account's position"
    );
    assert_eq!(
        core.extra_engines[0].1.position_size(vike_model::events::PositionSide::Both),
        -2.0,
        "ALT's, not the default's"
    );
    assert!(core.engine.seen_trade_ids().any(|t| t == "t-sim"), "with its own dedup");
    assert!(core.extra_engines[0].1.seen_trade_ids().any(|t| t == "t-sim#ALT"));
    assert!(noted(&core, "put back on 2 of 2"), "recent {:?}", core.recent);
}

/// **Two journaled engines of one venue without a route key (a v16 `Snap`) restore NEITHER**: which
/// is which is unknowable, and a guess books one account's position onto the other.
#[test]
fn a_snapshot_without_route_keys_on_a_two_account_venue_restores_neither_engine() {
    // Both snapshots read back `route_key: None`, the shape every pre-v17 `Snap` has.
    let mut a = journaled("sim", 3.0, TradingState::Active);
    let mut b = journaled("sim#ALT", -2.0, TradingState::Active);
    a.route_key = None;
    b.route_key = None;
    let core = two_accounts(vec![a, b]);
    assert_eq!(core.engine.position_size(vike_model::events::PositionSide::Both), 0.0);
    assert_eq!(core.extra_engines[0].1.position_size(vike_model::events::PositionSide::Both), 0.0);
    assert!(noted(&core, "RESTORE ENGINE SKIPPED sim: 2 journaled engines"), "{:?}", core.recent);
    assert!(noted(&core, "put back on 0 of 2"));
}

/// **A journaled engine no engine of this core matches is skipped and said**, and the rest apply.
#[test]
fn an_unmatched_snapshot_is_skipped_and_the_others_apply() {
    let core = two_accounts(vec![
        journaled("sim", 1.0, TradingState::Active),
        journaled("sim#GONE", 5.0, TradingState::Active),
    ]);
    assert_eq!(core.engine.position_size(vike_model::events::PositionSide::Both), 1.0);
    assert_eq!(
        core.extra_engines[0].1.position_size(vike_model::events::PositionSide::Both),
        0.0,
        "ALT had no snapshot"
    );
    assert!(noted(&core, "RESTORE ENGINE SKIPPED sim#GONE"), "{:?}", core.recent);
}

/// **The settings' limits stay**: a journaled `RiskLimits` is never applied (decisions 0086, 0111).
#[test]
fn restored_limits_never_override_the_settings() {
    let mut journal_side = engine("sim");
    journal_side.gate.limits.max_notional_per_order = Some(1.0);
    let snap = journal_side.snapshot_state();
    let mut live = engine("sim");
    live.gate.limits.max_notional_per_order = Some(50_000.0);
    let core = core_of(
        live,
        Vec::new(),
        CoreConfig { restored_engines: vec![snap], ..CoreConfig::default() },
    );
    assert_eq!(core.engine.gate.limits.max_notional_per_order, Some(50_000.0));
}

/// **A journaled trading state applies only when STRICTER** than the boot's: a latched `Reducing`
/// survives the restart, a journaled `Active` does not loosen a boot `Halted`.
#[test]
fn a_stricter_restored_trading_state_is_kept_and_a_looser_one_is_not() {
    let core = core_of(
        engine("sim"),
        Vec::new(),
        CoreConfig {
            restored_engines: vec![journaled("sim", 0.0, TradingState::Reducing)],
            ..CoreConfig::default()
        },
    );
    assert_eq!(core.engine.trading_state, TradingState::Reducing, "the latch survives");
    assert!(noted(&core, "RESTORE TRADING STATE sim: Reducing"), "{:?}", core.recent);

    let mut halted = engine("sim");
    halted.trading_state = TradingState::Halted;
    let core = core_of(
        halted,
        Vec::new(),
        CoreConfig {
            restored_engines: vec![journaled("sim", 0.0, TradingState::Active)],
            ..CoreConfig::default()
        },
    );
    assert_eq!(core.engine.trading_state, TradingState::Halted, "never loosened");
    assert!(!noted(&core, "RESTORE TRADING STATE"));
}

/// **The journal off is today's core**: `BootRestore::off()` writes nothing onto a config, and a
/// core assembled over it carries no restore line.
#[test]
fn a_journal_off_core_is_byte_identical() {
    let mut config = CoreConfig::default();
    BootRestore::off().apply_to(&mut config);
    assert!(config.coid_session.is_none());
    assert_eq!(config.arm_seq, 0);
    assert!(config.conditionals.is_empty() && config.contingencies.is_empty());
    assert!(config.restored_engines.is_empty() && config.journal_orders.is_empty());
    assert!(config.mount_attr.is_empty() && config.conditional_owners.is_empty());
    assert!(config.lost_held_exits.is_empty() && config.restore_notes.is_empty());
    assert!(!config.restored_paper_orders_gone, "the paper rule is the ROOT's statement");
    let core = core_of(engine("sim"), Vec::new(), config);
    assert!(core.recent.is_empty(), "no line: {:?}", core.recent);
}
// --- the restore at the PASS and at boot (decision 0127): the journal's price at adoption, a paper
// --- book, and a held exit a broken journal lost. Driven like `restore_tests`, over its helpers.

/// **A journal candidate is adopted with its journaled price.** The venue's report carries no
/// price; the candidate the journal supplied does, so the registry entry the adoption rebuilds rests
/// at it (decision 0120's verdict 8, amended for a box with a journal).
#[test]
fn a_journal_candidate_is_adopted_with_its_journaled_price() {
    let dir = Scratch::reserved("vco-restore-journal-price");
    let (nb, events) = (cell(), Events::default());
    let mut config = open(config_on(&dir, vec![spy_mount(&nb, &events)]));
    config.journal_orders = vec![crate::replay::JournalOrder {
        coid: "j-px".into(),
        mount_id: "maker_b".into(),
        venue: Some("sim".into()),
        symbol: Some("BTCUSDT".into()),
        account: None,
        price: Some(101.25),
    }];
    let mut core = assembled(config);
    assert!(core.restored_orders.contains_key("j-px"), "precondition: a restored entry");

    run(&mut core, vec![report("j-px", vike_model::OrderStatus::Accepted)], Vec::new());

    let order = core.engine.registry.get("j-px").expect("adopted");
    assert_eq!(order.request.price, Some(101.25), "the journal's price, which no report carries");
    assert_eq!(core.restore_counters.restored_adopted, 1);
    assert_eq!(heard(&events), vec![("Accepted".to_string(), None)], "its mount is told");
}

/// **A restored order on a PAPER book is gone at boot** under
/// `CoreConfig::restored_paper_orders_gone`: the book lived in the dead process's memory and no
/// pass will ever judge it. The mount hears `Canceled` with the paper reason and its tag, the
/// entry is spent and counted, and the file forgets it.
#[test]
fn a_paper_engine_restored_order_is_gone_at_boot() {
    let dir = Scratch::reserved("vco-restore-paper-gone");
    let (coid, nb, events) = rest_one(&dir);
    let mut config = config_on(&dir, vec![spy_mount(&nb, &events)]);
    config.restored_paper_orders_gone = true;
    let mut core = assembled(config);
    assert_eq!(core.engine.mode, vike_exec::EngineMode::Paper, "precondition: a paper engine");
    assert!(core.restored_orders.is_empty(), "nothing waits for a pass");
    assert_eq!(core.restore_counters.restored_gone, 1);
    assert!(noted(&core, "RESTORE PAPER BOOK"), "recent {:?}", core.recent);

    core.dispatch_applied_fills();
    let canceled = vike_model::OrderEventKind::Canceled {
        reason: crate::replay::RESTORE_PAPER_BOOK_LOST_REASON.to_string(),
    };
    assert_eq!(heard(&events), vec![(format!("{canceled:?}"), Some("q".to_string()))]);
    drop(core);
    let text = std::fs::read_to_string(dir.join(ORDER_OWNERS_FILE)).unwrap();
    assert!(text.contains("\"forget\"") && text.contains(&coid), "the file forgets it: {text}");
}

/// **A held exit a broken journal lost is a terminal reject its mount hears**: nothing holds
/// or releases it any more, so it may not vanish in silence. The owner comes from the ownership
/// file; the entry is spent, so no pass can tell the mount a second time.
#[test]
fn a_lost_held_exit_tells_its_mount_with_a_terminal_reject() {
    let dir = Scratch::reserved("vco-restore-lost-held-exit");
    let (coid, nb, events) = rest_one(&dir);
    let mut config = open(config_on(&dir, vec![spy_mount(&nb, &events)]));
    config.lost_held_exits = vec![crate::replay::LostHeldExit {
        coid: coid.clone(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
    }];
    let mut core = assembled(config);
    assert!(!core.restored_orders.contains_key(&coid), "spent at boot");

    core.dispatch_applied_fills();
    let rejected = vike_model::OrderEventKind::Rejected {
        reason: crate::replay::RESTORE_LOST_REASON.to_string(),
    };
    assert_eq!(heard(&events), vec![(format!("{rejected:?}"), Some("q".to_string()))]);
    run(&mut core, Vec::new(), Vec::new());
    assert_eq!(heard(&events).len(), 1, "a later pass tells it nothing more");
}
