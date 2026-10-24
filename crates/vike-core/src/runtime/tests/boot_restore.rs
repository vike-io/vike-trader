//! The ASSEMBLY half of the boot restore (`crate::runtime::boot_restore`, decision 0127), white-box:
//! a core assembled over what `crate::replay::BootRestore::apply_to` would put on its config. The
//! engine snapshots are built by hand from engines the test folds itself, so each case states the
//! journal it stands for.

use super::order_owners_tests::{Events, NOW, assembled, cell, config_on, noted, spy_mount};
use super::restore_tests::{
    fill_of, heard, open, pass_for, report, rest_one, rest_one_stop, restart, run,
    run_claiming_trigger_orders,
};
use super::*;
use crate::order_owners::ORDER_OWNERS_FILE;
use crate::replay::BootRestore;
use crate::runtime::test_support::{core_of, engine_on};
use crate::scratch::Scratch;
use tracing_test::traced_test;
use vike_exec::recon::{BalanceTol, ReconPolicy};
use vike_exec::testing::RecordingClient;
use vike_model::events::{OrderAccepted, OrderFilled, OrderPartiallyFilled, OrderSubmitted};
use vike_model::{FillReport, OrderStatus, OrderStatusReport};

type Core = CoreThread<RecordingClient>;

/// An engine on `sim`/`BTCUSDT` routed as `route_key` (the venue for the default account).
fn engine(route_key: &str) -> ExecutionEngine<RecordingClient> {
    let mut e = engine_on("sim", "BTCUSDT", RecordingClient::default());
    e.route_key = route_key.to_string();
    e
}

/// The `Snap` entry of `route_key`'s engine after it folded a `size` position, one fill id and a
/// trading state — what a journal of that engine carries. Its engine was never told its tier, so the
/// snapshot states none: the shape of every journal written before the tier field.
fn journaled(route_key: &str, size: f64, state: TradingState) -> vike_exec::EngineSnapshot {
    journaled_at(route_key, size, state, None)
}

/// [`journaled`] from an engine that was told it ran at `tier` (`None`: nobody told it).
fn journaled_at(
    route_key: &str,
    size: f64,
    state: TradingState,
    tier: Option<vike_model::AccountTier>,
) -> vike_exec::EngineSnapshot {
    let mut e = engine(route_key);
    if let Some(tier) = tier {
        e.set_tier(tier);
    }
    e.account.positions.insert(
        ("sim".into(), "BTCUSDT".into(), vike_model::events::PositionSide::Both),
        vike_exec::PositionEntry { size, avg_px: 100.0, ..Default::default() },
    );
    e.account.realized_pnl = 5.0;
    e.account.fees_paid = 0.5;
    e.seed_seen_trade_ids([format!("t-{route_key}")]);
    e.trading_state = state;
    e.snapshot_state()
}

/// An engine on `route_key` that was told it runs at `tier` (what the mount does at assembly).
fn engine_at(route_key: &str, tier: vike_model::AccountTier) -> ExecutionEngine<RecordingClient> {
    let mut e = engine(route_key);
    e.set_tier(tier);
    e
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

/// **A stopped engine's TOMBSTONE is skipped at INFO, a key nobody carries stays a WARN.**
/// `CoreThread::write_snap` journals a stopped primary as `<route_key>#stopped` once a newer engine
/// of its account replaced it, so the first boot after an `account.active` off/on cycle skips that
/// snapshot by design. Both skips keep their ring note; only the tombstone's drops a level.
#[traced_test]
#[test]
fn a_stopped_engines_tombstone_is_skipped_at_info_and_an_unknown_key_at_warn() {
    let tombstone = format!("sim{RETIRED_ROUTE_KEY_SUFFIX}");
    let core = two_accounts(vec![
        journaled("sim", 1.0, TradingState::Active),
        journaled(&tombstone, 5.0, TradingState::Active),
        journaled("sim#GONE", 5.0, TradingState::Active),
    ]);
    assert_eq!(core.engine.position_size(vike_model::events::PositionSide::Both), 1.0);
    for key in [tombstone.as_str(), "sim#GONE"] {
        let note =
            format!("RESTORE ENGINE SKIPPED {key}: this core runs no engine with that route key");
        assert!(noted(&core, &note), "{key} left no ring note: {:?}", core.recent);
    }
    logs_assert(|lines: &[&str]| {
        let line_of = |needle: &str| {
            lines
                .iter()
                .find(|l| l.contains(needle))
                .copied()
                .ok_or_else(|| format!("no log line carries {needle:?}"))
        };
        let tomb = line_of("RESTORE ENGINE SKIPPED sim#stopped")?;
        if !tomb.contains("INFO") || tomb.contains("WARN") {
            return Err(format!("the tombstone's skip is not an INFO line: {tomb}"));
        }
        let unknown = line_of("RESTORE ENGINE SKIPPED sim#GONE")?;
        if !unknown.contains("WARN") || unknown.contains("INFO") {
            return Err(format!("an unknown key's skip is not a WARN line: {unknown}"));
        }
        Ok(())
    });
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
// --- WHOSE BOOK a snapshot is (`EngineSnapshot::tier`): a snapshot that states a tier other than the
// --- engine's is not restored; one that states none is restored as it always was.

use vike_model::AccountTier;

fn short(core: &Core) -> f64 {
    core.engine.position_size(vike_model::events::PositionSide::Both)
}

/// **THE HAZARD, both ways round: a cold tier flip must not carry a book across.** The Binance lane
/// twin `binance#PERP` has one route key at `demo` and at `live`; a `paper` engine's key is the
/// `demo` one's. Whichever way the tier changed while the tradehub was down, the snapshot states
/// the tier it ran at, and the engine of the OTHER tier restores NOTHING of it (no position, PnL,
/// fee, balance or fill dedup), says so in words that name both tiers and what was left out, and
/// comes up HALTED because it was holding a position.
#[test]
fn a_snapshot_of_another_tier_is_not_restored_and_its_engine_is_halted() {
    use AccountTier::{Demo, Live, Paper};
    for (journaled_tier, engine_tier) in
        [(Demo, Live), (Live, Demo), (Paper, Demo), (Paper, Live), (Demo, Paper), (Live, Paper)]
    {
        let core = core_of(
            engine_at("sim", engine_tier),
            Vec::new(),
            CoreConfig {
                restored_engines: vec![journaled_at(
                    "sim",
                    3.0,
                    TradingState::Active,
                    Some(journaled_tier),
                )],
                ..CoreConfig::default()
            },
        );
        let case = format!("{journaled_tier:?} snapshot into a {engine_tier:?} engine");
        assert_eq!(short(&core), 0.0, "{case}: a position came across");
        assert_eq!(core.engine.account.realized_pnl, 0.0, "{case}: PnL came across");
        assert_eq!(core.engine.account.fees_paid, 0.0, "{case}: fees came across");
        assert_eq!(core.engine.seen_trade_ids().count(), 0, "{case}: the fill dedup came across");
        assert_eq!(core.engine.trading_state, TradingState::Halted, "{case}: not halted");
        assert_eq!(core.engine.tier, engine_tier, "{case}: the engine keeps its own tier");
        let want = format!(
            "RESTORE ENGINE SKIPPED sim: TIER MISMATCH. The journaled engine ran at tier `{}` and \
             this engine runs at tier `{}`",
            journaled_tier.as_str(),
            engine_tier.as_str()
        );
        assert!(noted(&core, &want), "{case}: recent {:?}", core.recent);
        assert!(noted(&core, "1 open position(s) [BTCUSDT +3 @ 100]"), "{case}: {:?}", core.recent);
        assert!(noted(&core, "HALTED until an operator resumes it"), "{case}");
        assert!(noted(&core, "put back on 0 of 1"), "{case}: {:?}", core.recent);
    }
}

/// The same refusal as a LOG LINE: an ERROR when the book held a position (the operator changed the
/// tier with something open), a WARN when it was flat (nothing is lost), and the flat book's engine
/// keeps trading.
#[traced_test]
#[test]
fn the_refusal_is_an_error_over_a_position_and_a_warn_over_a_flat_book() {
    let loud = core_of(
        engine_at("sim", AccountTier::Live),
        Vec::new(),
        CoreConfig {
            restored_engines: vec![journaled_at(
                "sim",
                2.0,
                TradingState::Active,
                Some(AccountTier::Demo),
            )],
            ..CoreConfig::default()
        },
    );
    assert_eq!(loud.engine.trading_state, TradingState::Halted);
    let quiet = core_of(
        engine_at("sim#ALT", AccountTier::Live),
        Vec::new(),
        CoreConfig {
            restored_engines: vec![journaled_at(
                "sim#ALT",
                0.0,
                TradingState::Active,
                Some(AccountTier::Demo),
            )],
            ..CoreConfig::default()
        },
    );
    assert_eq!(quiet.engine.trading_state, TradingState::Active, "a flat book halts nothing");
    assert_eq!(quiet.engine.account.realized_pnl, 0.0, "but none of its state came across");
    assert!(noted(&quiet, "it held no open position"), "recent {:?}", quiet.recent);
    logs_assert(|lines: &[&str]| {
        let line_of = |needle: &str| {
            lines
                .iter()
                .find(|l| l.contains(needle))
                .copied()
                .ok_or_else(|| format!("no log line carries {needle:?}"))
        };
        let error = line_of("RESTORE ENGINE SKIPPED sim: TIER MISMATCH")?;
        if !error.contains("ERROR") {
            return Err(format!("a refused book with a position is not an ERROR line: {error}"));
        }
        for word in ["`demo`", "`live`", "BTCUSDT +2", "resting order(s)", "balance", "fill id(s)"]
        {
            if !error.contains(word) {
                return Err(format!("the ERROR line does not say {word}: {error}"));
            }
        }
        let warn = line_of("RESTORE ENGINE SKIPPED sim#ALT")?;
        if !warn.contains("WARN") || warn.contains("ERROR") {
            return Err(format!("a flat refused book is not a WARN line: {warn}"));
        }
        Ok(())
    });
}

/// **UPGRADE: a snapshot that states no tier is restored exactly as it always was**, whatever tier
/// the engine runs at (the journal on disk when this build first starts was written by one that
/// did not know the field), and the engine's NEXT snapshot states the tier it runs at.
#[test]
fn a_snapshot_that_states_no_tier_is_restored_and_the_next_one_names_the_tier() {
    for tier in AccountTier::ALL {
        let core = core_of(
            engine_at("sim", tier),
            Vec::new(),
            CoreConfig {
                restored_engines: vec![journaled("sim", 3.0, TradingState::Active)],
                ..CoreConfig::default()
            },
        );
        assert_eq!(short(&core), 3.0, "{tier:?}: the unstated book is restored");
        assert_eq!(core.engine.account.realized_pnl, 5.0, "{tier:?}");
        assert_eq!(core.engine.trading_state, TradingState::Active, "{tier:?}: not halted");
        assert!(noted(&core, "put back on 1 of 1"), "{tier:?}: {:?}", core.recent);
        assert!(!noted(&core, "TIER MISMATCH"), "{tier:?}");
        assert_eq!(
            core.engine.snapshot_state().tier,
            Some(tier),
            "{tier:?}: the next Snap says so"
        );
    }
}

/// A snapshot of the SAME tier is restored, whichever tier it is.
#[test]
fn a_snapshot_of_the_same_tier_is_restored() {
    for tier in AccountTier::ALL {
        let core = core_of(
            engine_at("sim", tier),
            Vec::new(),
            CoreConfig {
                restored_engines: vec![journaled_at("sim", 3.0, TradingState::Active, Some(tier))],
                ..CoreConfig::default()
            },
        );
        assert_eq!(short(&core), 3.0, "{tier:?}");
        assert_eq!(core.engine.trading_state, TradingState::Active, "{tier:?}");
        assert!(noted(&core, "put back on 1 of 1"), "{tier:?}: {:?}", core.recent);
    }
}

/// One account's refusal touches no other: the default account (same tier) is restored and keeps
/// trading, the labelled one (its snapshot was another tier's) restores nothing and halts.
#[test]
fn a_refused_account_does_not_halt_or_empty_its_neighbour() {
    let core = core_of(
        engine_at("sim", AccountTier::Live),
        vec![(1_000.0, engine_at("sim#ALT", AccountTier::Live))],
        CoreConfig {
            seed_cash: 1_000.0,
            restored_engines: vec![
                journaled_at("sim", 3.0, TradingState::Active, Some(AccountTier::Live)),
                journaled_at("sim#ALT", -2.0, TradingState::Active, Some(AccountTier::Demo)),
            ],
            ..CoreConfig::default()
        },
    );
    assert_eq!(short(&core), 3.0, "the default account's own book is back");
    assert_eq!(core.engine.trading_state, TradingState::Active);
    let alt = &core.extra_engines[0].1;
    assert_eq!(alt.position_size(vike_model::events::PositionSide::Both), 0.0);
    assert_eq!(alt.trading_state, TradingState::Halted);
    assert!(noted(&core, "put back on 1 of 2"), "recent {:?}", core.recent);
}

/// **The hot-flip path is unchanged**: a stopped engine's tombstone (any tier) is skipped at INFO
/// because no engine carries its key, and the engine of that account that took over is judged on
/// its own snapshot.
#[traced_test]
#[test]
fn a_tombstone_of_another_tier_is_still_skipped_by_design() {
    let tombstone = format!("sim{RETIRED_ROUTE_KEY_SUFFIX}");
    let core = core_of(
        engine_at("sim", AccountTier::Live),
        Vec::new(),
        CoreConfig {
            restored_engines: vec![
                journaled_at("sim", 1.0, TradingState::Active, Some(AccountTier::Live)),
                journaled_at(&tombstone, 5.0, TradingState::Active, Some(AccountTier::Demo)),
            ],
            ..CoreConfig::default()
        },
    );
    assert_eq!(short(&core), 1.0, "the live engine got its own book");
    assert_eq!(core.engine.trading_state, TradingState::Active, "and the tombstone halted nothing");
    assert!(!noted(&core, "TIER MISMATCH"), "recent {:?}", core.recent);
    logs_assert(|lines: &[&str]| {
        let tomb = lines
            .iter()
            .find(|l| l.contains("RESTORE ENGINE SKIPPED sim#stopped"))
            .ok_or_else(|| "the tombstone's skip left no log line".to_string())?;
        if tomb.contains("INFO") && !tomb.contains("ERROR") && !tomb.contains("WARN") {
            Ok(())
        } else {
            Err(format!("the tombstone's skip is not an INFO line: {tomb}"))
        }
    });
}

/// **A refused book does not shed a journaled HALT** (decision 0127, verdict 7: only a
/// `SetTradingState` command loosens a restored halt). The operator halted the node, then changed an
/// account's tier: the other engines come back halted, and so does this one, whether its refused
/// book was flat (the snapshot's state is carried across the refusal) or held a position (always
/// halted). A snapshot that was `Active` and flat leaves the engine as it is.
#[test]
fn a_refused_book_keeps_the_journaled_halt() {
    use AccountTier::{Demo, Live};
    for (state, size, want) in [
        (TradingState::Halted, 0.0, TradingState::Halted),
        (TradingState::Reducing, 0.0, TradingState::Reducing),
        (TradingState::Active, 0.0, TradingState::Active),
        (TradingState::Active, 2.0, TradingState::Halted),
        (TradingState::Reducing, 2.0, TradingState::Halted),
        (TradingState::Halted, 2.0, TradingState::Halted),
    ] {
        let core = core_of(
            engine_at("sim", Live),
            Vec::new(),
            CoreConfig {
                restored_engines: vec![journaled_at("sim", size, state, Some(Demo))],
                ..CoreConfig::default()
            },
        );
        let case = format!("a {state:?} {size} snapshot of another tier");
        assert_eq!(core.engine.trading_state, want, "{case}");
        assert_eq!(short(&core), 0.0, "{case}: nothing else came across");
        assert_eq!(
            noted(&core, "RESTORE TRADING STATE sim"),
            size == 0.0 && state != TradingState::Active,
            "{case}: {:?}",
            core.recent
        );
    }
    // A stricter state the engine already boots in is never loosened by a refused snapshot.
    let mut halted = engine_at("sim", Live);
    halted.trading_state = TradingState::Halted;
    let core = core_of(
        halted,
        Vec::new(),
        CoreConfig {
            restored_engines: vec![journaled_at("sim", 0.0, TradingState::Active, Some(Demo))],
            ..CoreConfig::default()
        },
    );
    assert_eq!(core.engine.trading_state, TradingState::Halted);
}

/// **The refused book is SAVED where an operator can recover it by hand**: `refused-books/<route
/// key>-<snap seq>.json` under the state directory, with the average price of every position and the
/// ids of the resting orders, and the ERROR note names the file, the `Snap` and the average price. A
/// flat book with nothing resting writes no file; an existing file is left alone (a repeated refusal
/// is harmless); no state directory is said, never a refused start.
#[test]
fn a_refused_book_is_saved_with_its_average_price_and_the_snap_it_came_from() {
    let dir = Scratch::reserved("vco-restore-refused-book");
    let config = |size: f64| CoreConfig {
        state_dir: Some(dir.to_path_buf()),
        restored_snap_seq: Some(42),
        restored_engines: vec![journaled_at(
            "sim#ALT",
            size,
            TradingState::Active,
            Some(AccountTier::Demo),
        )],
        ..CoreConfig::default()
    };
    let flat = core_of(engine_at("sim#ALT", AccountTier::Live), Vec::new(), config(0.0));
    assert!(!dir.join("refused-books").exists(), "a flat book with nothing resting saves nothing");
    assert!(noted(&flat, "journal snap seq 42"), "recent {:?}", flat.recent);

    let held = core_of(engine_at("sim#ALT", AccountTier::Live), Vec::new(), config(3.0));
    let file = dir.join("refused-books").join("sim_ALT-42.json");
    assert!(noted(&held, "BTCUSDT +3 @ 100"), "recent {:?}", held.recent);
    assert!(noted(&held, "journal snap seq 42"), "recent {:?}", held.recent);
    assert!(noted(&held, "sim_ALT-42.json"), "the note names the file: {:?}", held.recent);
    let saved: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&file).expect("the book was saved")).unwrap();
    assert_eq!(saved["route_key"], "sim#ALT");
    assert_eq!(saved["journaled_tier"], "demo");
    assert_eq!(saved["engine_tier"], "live");
    assert_eq!(saved["journal_snap_seq"], 42);
    let positions = saved["positions"].as_array().expect("positions");
    assert_eq!(positions.len(), 1);
    assert_eq!(positions[0][1]["size"], 3.0);
    assert_eq!(positions[0][1]["avg_px"], 100.0);
    assert!(saved["resting_orders"].is_array());

    // The same refusal again (a boot before any Snap) keeps the first file untouched.
    std::fs::write(&file, b"{\"kept\":true}").unwrap();
    let again = core_of(engine_at("sim#ALT", AccountTier::Live), Vec::new(), config(3.0));
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "{\"kept\":true}");
    assert!(noted(&again, "sim_ALT-42.json"), "recent {:?}", again.recent);

    let nowhere = core_of(
        engine_at("sim", AccountTier::Live),
        Vec::new(),
        CoreConfig {
            restored_engines: vec![journaled_at(
                "sim",
                3.0,
                TradingState::Active,
                Some(AccountTier::Demo),
            )],
            ..CoreConfig::default()
        },
    );
    assert_eq!(nowhere.engine.trading_state, TradingState::Halted, "still refused and halted");
    assert!(noted(&nowhere, "could NOT be saved (no state directory"), "{:?}", nowhere.recent);
}

/// **A refusal is durable at once**: a core with a journal writes ONE `Snap` right after refusing,
/// stating the engine's own tier, flat, with the halt, so a boot that follows a crash restores from
/// THAT and not from the other tier's checkpoint. A core that refused nothing writes none.
#[test]
fn a_refusal_writes_one_snap_stating_the_engines_own_tier() {
    use vike_journal::{CommandJournal, JournalFileConfig, JournalRecord};
    fn journal(dir: &Scratch) -> JournalConfig {
        JournalConfig {
            dir: dir.to_path_buf(),
            file: JournalFileConfig { segment_bytes: 1 << 20, flush_every: 8 },
            snapshot_every: 1_000,
        }
    }
    let snaps = |dir: &Scratch| -> Vec<Vec<vike_exec::EngineSnapshot>> {
        CommandJournal::read_all(dir)
            .map(|records| {
                records
                    .into_iter()
                    .filter_map(|r| match r {
                        JournalRecord::Snap { engines, .. } => Some(engines),
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default()
    };

    let refused = Scratch::reserved("vco-restore-refusal-snap");
    let core = core_of(
        engine_at("sim", AccountTier::Live),
        Vec::new(),
        CoreConfig {
            journal: Some(journal(&refused)),
            restored_engines: vec![journaled_at(
                "sim",
                3.0,
                TradingState::Active,
                Some(AccountTier::Demo),
            )],
            ..CoreConfig::default()
        },
    );
    drop(core);
    let written = snaps(&refused);
    assert_eq!(written.len(), 1, "exactly one Snap, written at boot");
    let engine = &written[0][0];
    assert_eq!(engine.tier, Some(AccountTier::Live), "it states the engine's own tier");
    assert_eq!(engine.trading_state, TradingState::Halted, "with the halt");
    assert!(
        engine.account.positions.iter().all(|(_, p)| p.size == 0.0),
        "and none of the other tier's book"
    );

    let same = Scratch::reserved("vco-restore-no-refusal-no-snap");
    let core = core_of(
        engine_at("sim", AccountTier::Live),
        Vec::new(),
        CoreConfig {
            journal: Some(journal(&same)),
            restored_engines: vec![journaled_at(
                "sim",
                3.0,
                TradingState::Active,
                Some(AccountTier::Live),
            )],
            ..CoreConfig::default()
        },
    );
    drop(core);
    assert!(snaps(&same).is_empty(), "a restore that refused nothing writes no boot Snap");
}

/// An idle strategy, to give a mount a ledger to lose.
struct Idle;

impl Strategy<LiveBroker> for Idle {}

/// **The mount ledgers of a refused engine go with it** (the journal's rows AND the ownership
/// file's): a mount's ledger is its slice of the engine's book, and the book it was a slice of is
/// another tier's. The control: the same ledger over an engine of the SAME tier is restored.
#[test]
fn the_ledgers_of_a_mount_on_a_refused_engine_are_dropped_in_memory_and_in_the_file() {
    let dir = Scratch::reserved("vco-restore-tier-ledger");
    let mount_id = "idle";
    let config = |snapshot_tier: AccountTier| CoreConfig {
        seed_cash: 10_000.0,
        clock: Box::new(|| super::order_owners_tests::NOW),
        strategy: Some(StrategyMount {
            account: None,
            symbols: Vec::new(),
            controller_id: Some(mount_id.to_string()),
            underlying_symbol: None,
            venue: "sim".into(),
            symbol: "BTCUSDT".into(),
            interval: "1m".into(),
            strategy: Box::new(Idle),
        }),
        order_owners: Some(crate::order_owners::OrderOwnerLog::in_state_dir(&dir)),
        restored_engines: vec![journaled_at("sim", 3.0, TradingState::Active, Some(snapshot_tier))],
        mount_attr: vec![vike_journal::SnapMountAttr {
            mount_id: mount_id.into(),
            size: 3.0,
            avg_px: 100.0,
            realized_pnl: 7.0,
            fees_paid: 0.25,
        }],
        ..CoreConfig::default()
    };

    let same = core_of(engine_at("sim", AccountTier::Live), Vec::new(), config(AccountTier::Live));
    assert_eq!(same.mount_ids[0], mount_id, "precondition: the derived mount id");
    assert_eq!(same.mount_attr[0].size, 3.0, "control: the same tier's ledger is restored");
    assert_eq!(same.mount_attr[0].realized_pnl, 7.0);
    drop(same);

    let refused =
        core_of(engine_at("sim", AccountTier::Live), Vec::new(), config(AccountTier::Demo));
    assert_eq!(refused.mount_attr[0], MountAttribution::default(), "the other tier's ledger");
    assert!(noted(&refused, "RESTORE MOUNT LEDGER DROPPED"), "recent {:?}", refused.recent);
    drop(refused);
    let text = std::fs::read_to_string(dir.join(ORDER_OWNERS_FILE)).unwrap();
    let last = text
        .lines()
        .rfind(|l| l.contains("\"kind\":\"ledger\"") && l.contains(mount_id))
        .expect("the file was told");
    assert!(last.contains("\"size\":0.0") && last.contains("\"realized_pnl\":0.0"), "{last}");
}

/// **An armed conditional of a refused engine is not re-armed**: its fire would place an order on an
/// account it was never armed on. An arm of an engine that WAS restored is re-armed as ever.
#[test]
fn an_armed_conditional_on_a_refused_engine_is_not_rearmed() {
    let arm = |id: &str| vike_journal::SnapConditional {
        arm_id: id.into(),
        terms: vike_journal::ConditionalRecord {
            venue: "sim".into(),
            symbol: "BTCUSDT".into(),
            side: -1,
            qty: 1.0,
            price: Some(95.0),
            trail: None,
            extreme: None,
            trigger_by: None,
        },
        route_key: None,
    };
    let armed = |core: &Core| core.conditional_books.values().map(|b| b.len()).sum::<usize>();
    for (snapshot_tier, rearmed) in [(AccountTier::Live, 1), (AccountTier::Demo, 0)] {
        let core = core_of(
            engine_at("sim", AccountTier::Live),
            Vec::new(),
            CoreConfig {
                restored_engines: vec![journaled_at(
                    "sim",
                    1.0,
                    TradingState::Active,
                    Some(snapshot_tier),
                )],
                conditionals: vec![arm("arm-1")],
                ..CoreConfig::default()
            },
        );
        assert_eq!(armed(&core), rearmed, "a {snapshot_tier:?} snapshot: {:?}", core.recent);
        assert_eq!(
            noted(&core, "RESTORE ARMS DROPPED"),
            rearmed == 0,
            "a {snapshot_tier:?} snapshot: {:?}",
            core.recent
        );
    }
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
        ..crate::replay::JournalOrder::default()
    }];
    let mut core = assembled(config);
    assert!(core.restored_orders.contains_key("j-px"), "precondition: a restored entry");

    run(&mut core, vec![report("j-px", vike_model::OrderStatus::Accepted)], Vec::new());

    let order = core.engine.registry.get("j-px").expect("adopted");
    assert_eq!(order.request.price, Some(101.25), "the journal's price, which no report carries");
    assert_eq!(core.restore_counters.restored_adopted, 1);
    assert_eq!(heard(&events), vec![("Accepted".to_string(), None)], "its mount is told");
}

/// The journal's candidate for a resting reduce-only stop at `sim`/`BTCUSDT`, owned by `mount_id`.
fn stop_candidate(coid: &str, mount_id: &str) -> crate::replay::JournalOrder {
    crate::replay::JournalOrder {
        coid: coid.into(),
        mount_id: mount_id.into(),
        venue: Some("sim".into()),
        symbol: Some("BTCUSDT".into()),
        reduce_only: true,
        trigger_price: Some(90.0),
        tier: Some(vike_model::AccountTier::Demo),
        ..crate::replay::JournalOrder::default()
    }
}

/// The venue's report of a resting SELL stop: a bare row, with no reduce-only flag and no trigger
/// price, because a venue report carries neither.
fn stop_report(coid: &str) -> OrderStatusReport {
    OrderStatusReport {
        side: -1,
        order_type: vike_model::OrderType::Stop,
        ..report(coid, OrderStatus::Accepted)
    }
}

/// **The restart cycle of an operator's stop** (the node restarts and every resting order stays at
/// the venue). The operator's reduce-only stop (no mount minted it) comes back from the journal
/// under the reserved pseudo mount, the pass adopts it silently with the `reduce_only` flag and the
/// trigger price the journal knows (a venue report carries neither), it raises no `UnknownOrder`,
/// and it LEAVES the pending table at its adoption, so a settled book keeps the cheap early return
/// of `restore_orders_at_pass` (the pseudo mount is no permanent tenant).
#[test]
fn an_operators_stop_is_adopted_with_its_terms_and_leaves_the_pending_table() {
    let dir = Scratch::reserved("vco-restore-protective-stop");
    let mut config = open(config_on(&dir, Vec::new()));
    config.journal_orders =
        vec![stop_candidate("op-stop", crate::order_owners::PROTECTIVE_STOP_MOUNT)];
    let mut core = assembled(config);
    core.engine.tier = vike_model::AccountTier::Demo;
    assert!(core.restored_orders.is_empty(), "no mount owns it");
    assert!(
        core.pending_owners.contains_key(crate::order_owners::PROTECTIVE_STOP_MOUNT),
        "it waits under the pseudo mount, for a pass to adopt it silently"
    );
    assert!(
        noted(&core, "RESTORE OPERATOR STOPS: 1 resting protective stop(s)")
            && !noted(&core, "ORDER OWNERS HELD"),
        "the boot says what waits for what, not that a mount is missing: {:?}",
        core.recent
    );

    run_claiming_trigger_orders(&mut core, vec![stop_report("op-stop")], Vec::new());

    let adopted = core.engine.registry.get("op-stop").expect("adopted");
    assert!(adopted.request.reduce_only, "still a reduce-only stop");
    assert_eq!(adopted.request.trigger_price, Some(90.0), "with its level");
    assert_eq!(adopted.request.order_type, vike_model::OrderType::Stop);
    assert_eq!(core.restore_counters.restored_adopted, 1);
    assert!(core.recon_alerts.is_empty(), "adopted before the diff, so no UnknownOrder is held");
    assert!(
        core.pending_owners.values().all(|p| p.coids.is_empty()),
        "the adopted stop left the pending table: {:?}",
        core.pending_owners.keys().collect::<Vec<_>>()
    );
    assert!(core.engine.registry.contains_key("op-stop"), "and it is a known local order");
}

/// **An operator's stop is adopted on a `demo` book of an open venue, and nowhere else.** Adopting
/// it puts it where the link dead-man, `enter_safe_state` and a `MarketExit` reach it, and the owner
/// has not ruled on `live`; a venue whose order restore is closed will never run the pass that would
/// adopt it. Each such candidate is not filed (not in the pending table, not in the ownership file)
/// and ONE WARN note says so, instead of the boot promising an adoption nobody will make.
#[test]
fn an_operators_stop_is_filed_on_a_demo_book_of_an_open_venue_only() {
    use vike_model::AccountTier::{Demo, Live, Paper};
    for (why, tier, gate_open, filed) in [
        ("demo, gate open", Some(Demo), true, true),
        ("live: the owner has not ruled", Some(Live), true, false),
        ("paper", Some(Paper), true, false),
        ("no tier stated", None, true, false),
        ("demo, but the venue's restore gate is closed", Some(Demo), false, false),
    ] {
        let dir = Scratch::reserved("vco-restore-operator-stop-filing");
        let mut config = config_on(&dir, Vec::new());
        let gate: fn(&str) -> bool = if gate_open { |_| true } else { |_| false };
        config.restore_venue_gate = gate;
        config.journal_orders = vec![crate::replay::JournalOrder {
            tier,
            ..stop_candidate("op-stop", crate::order_owners::PROTECTIVE_STOP_MOUNT)
        }];
        let core = assembled(config);
        let waiting = core.pending_owners.contains_key(crate::order_owners::PROTECTIVE_STOP_MOUNT);
        assert_eq!(waiting, filed, "{why}: recent {:?}", core.recent);
        assert_eq!(
            noted(&core, "RESTORE OPERATOR STOPS NOT ADOPTED: 1"),
            !filed,
            "{why}: one note when declined: {:?}",
            core.recent
        );
        assert!(
            !(noted(&core, "ORDER RESTORE OFF") && noted(&core, "RESTORE OPERATOR STOPS: 1")),
            "{why}: never two contradicting notes about one venue: {:?}",
            core.recent
        );
        drop(core);
        let text = std::fs::read_to_string(dir.join(ORDER_OWNERS_FILE)).unwrap_or_default();
        assert_eq!(text.contains("op-stop"), filed, "{why}: the ownership file: {text}");
    }
}

/// **…and the pass asks for the client's claim.** A client that does not list trigger orders can
/// neither prove an operator's stop still rests nor be trusted to find it again, so on a `demo`
/// engine too the entry is released with one note and nothing is adopted, even when the report
/// happens to list the stop.
#[test]
fn an_operators_stop_is_released_when_the_client_lists_no_trigger_orders() {
    let dir = Scratch::reserved("vco-restore-operator-stop-no-claim");
    let mut config = open(config_on(&dir, Vec::new()));
    config.journal_orders =
        vec![stop_candidate("op-stop", crate::order_owners::PROTECTIVE_STOP_MOUNT)];
    let mut core = assembled(config);
    core.engine.tier = vike_model::AccountTier::Demo;
    assert!(core.pending_owners.contains_key(crate::order_owners::PROTECTIVE_STOP_MOUNT));

    run(&mut core, vec![stop_report("op-stop")], Vec::new());

    assert!(!core.engine.registry.contains_key("op-stop"), "not adopted without the claim");
    assert!(noted(&core, "does not list trigger orders"), "recent {:?}", core.recent);
    assert!(core.pending_owners.is_empty(), "released from memory: {:?}", core.pending_owners);
}

/// **The pass asks the tier again.** The ownership file outlives a tier change, so a pending
/// pseudo-mount entry whose engine is no longer `demo` is released at the pass with one note, and
/// the venue's listing of the stop adopts nothing (it stays an `UnknownOrder`).
#[test]
fn an_operators_stop_on_a_live_engine_is_released_at_the_pass_not_adopted() {
    let dir = Scratch::reserved("vco-restore-operator-stop-live");
    let mut first = open(config_on(&dir, Vec::new()));
    first.journal_orders =
        vec![stop_candidate("op-stop", crate::order_owners::PROTECTIVE_STOP_MOUNT)];
    drop(assembled(first)); // boot 1 files the pseudo entry in the ownership file
    let mut core = assembled(open(config_on(&dir, Vec::new())));
    assert!(
        core.pending_owners.contains_key(crate::order_owners::PROTECTIVE_STOP_MOUNT),
        "precondition: the file's pseudo entry waits: {:?}",
        core.recent
    );
    core.engine.tier = vike_model::AccountTier::Live;

    run(&mut core, vec![stop_report("op-stop")], Vec::new());

    assert!(!core.engine.registry.contains_key("op-stop"), "not adopted on a live engine");
    assert!(noted(&core, "operator stop(s) NOT adopted"), "recent {:?}", core.recent);
    assert!(core.pending_owners.is_empty(), "released from memory: {:?}", core.pending_owners);
    assert_eq!(
        core.recon_alerts.values().map(|a| a.kind).collect::<Vec<_>>(),
        vec![vike_exec::recon::DivergenceKind::UnknownOrder],
        "the stop stays what it was: an order the engine does not hold"
    );
}

/// A `bybit`/`BTCUSDT` account engine routed as `route_key` (`bybit#DEMO`, `bybit#LIVE`: how the node
/// mounts a labelled account), told it runs at `tier`.
fn bybit_account(
    route_key: &str,
    tier: vike_model::AccountTier,
) -> ExecutionEngine<RecordingClient> {
    let mut e = engine_on("bybit", "BTCUSDT", RecordingClient::default());
    e.route_key = route_key.to_string();
    e.set_tier(tier);
    e
}

/// The journal's candidate for the operator's `Stop` `coid` on bybit account `label`, recorded while
/// that account ran at `tier`.
fn bybit_stop_candidate(
    coid: &str,
    label: &str,
    tier: vike_model::AccountTier,
) -> crate::replay::JournalOrder {
    crate::replay::JournalOrder {
        venue: Some("bybit".into()),
        account: Some(label.into()),
        order_type: Some(vike_model::OrderType::Stop),
        tier: Some(tier),
        ..stop_candidate(coid, crate::order_owners::PROTECTIVE_STOP_MOUNT)
    }
}

/// One pass of bybit account `label`'s reconcile client, which lists `listed` and CLAIMS to list
/// trigger orders (`BybitReconClient::lists_trigger_orders`, stamped on the pass by the manager).
fn bybit_pass(core: &mut Core, label: &str, listed: Vec<OrderStatusReport>) {
    core.reconcile_reports(ReconcileReports {
        lists_trigger_orders: true,
        ..pass_for("bybit", Some(&format!("bybit#{label}")), listed, Vec::new())
    });
    core.dispatch_applied_fills();
}

/// The row `order/realtime` gives a resting operator stop, as bybit's parser reads it
/// (`crates/bridges/bybit/tests/offline/claim_prereqs.rs`): a live SELL `Stop` carrying the coid.
fn bybit_stop_report(coid: &str) -> OrderStatusReport {
    OrderStatusReport { venue: "bybit".into(), ..stop_report(coid) }
}

/// **0192's rule on a bybit book that has a DEMO and a LIVE account side by side** (the shape the CI box
/// runs; the claim is the client's, so it holds for both): an operator's stop on `bybit#DEMO` whose
/// report lists it is ADOPTED, into that account's engine and no other; the same on `bybit#LIVE` is
/// NEVER adopted. A candidate recorded at `live` is not even filed at boot; one filed while the account
/// still ran at `demo` (the ownership file outlives a tier change) is released at the LIVE pass with one
/// note, stays an `UnknownOrder` at the venue, and keeps its line in the file; nothing moves the DEMO
/// account's stop.
#[test]
fn a_bybit_operators_stop_is_adopted_on_the_demo_account_and_never_on_the_live_one() {
    use vike_model::AccountTier::{Demo, Live};
    let dir = Scratch::reserved("vco-restore-bybit-operator-stops");

    // boot 1: the journal names three operator stops
    let mut first = open(config_on(&dir, Vec::new()));
    first.journal_orders = vec![
        bybit_stop_candidate("demo-stop", "DEMO", Demo),
        bybit_stop_candidate("live-stop", "LIVE", Demo),
        bybit_stop_candidate("born-live-stop", "LIVE", Live),
    ];
    let boot1 = core_of(
        bybit_account("bybit#DEMO", Demo),
        vec![(1_000.0, bybit_account("bybit#LIVE", Demo))],
        first,
    );
    assert!(
        noted(&boot1, "RESTORE OPERATOR STOPS NOT ADOPTED: 1") && noted(&boot1, "born-live-stop"),
        "a stop recorded at live is not filed: {:?}",
        boot1.recent
    );
    drop(boot1);

    // boot 2: the file is the memory; the LIVE account now runs at its own tier
    let mut core = core_of(
        bybit_account("bybit#DEMO", Demo),
        vec![(1_000.0, bybit_account("bybit#LIVE", Live))],
        open(config_on(&dir, Vec::new())),
    );

    bybit_pass(&mut core, "DEMO", vec![bybit_stop_report("demo-stop")]);
    assert!(core.engine.registry.contains_key("demo-stop"), "adopted on the DEMO account");
    assert!(core.extra_engines[0].1.registry.is_empty(), "and not into the LIVE engine");
    assert_eq!(core.restore_counters.restored_adopted, 1);
    assert!(core.recon_alerts.is_empty(), "adopted before the diff, so no UnknownOrder is held");

    bybit_pass(&mut core, "LIVE", vec![bybit_stop_report("live-stop")]);
    for (name, engine) in [("DEMO", &core.engine), ("LIVE", &core.extra_engines[0].1)] {
        assert!(!engine.registry.contains_key("live-stop"), "never adopted ({name} engine)");
    }
    assert!(noted(&core, "operator stop(s) NOT adopted"), "recent {:?}", core.recent);
    assert_eq!(
        core.recon_alerts.values().map(|a| a.kind).collect::<Vec<_>>(),
        vec![vike_exec::recon::DivergenceKind::UnknownOrder],
        "the LIVE stop stays what it was: an order the engine does not hold"
    );
    assert!(core.engine.registry.contains_key("demo-stop"), "the DEMO stop is untouched");

    let mut filed = file_coids(core, &dir);
    filed.sort();
    assert_eq!(filed, ["demo-stop", "live-stop"], "born-live-stop was filed nowhere");
}

/// A stop of ANOTHER venue and account (`binance#PERP`) filed beside `op-stop`: an engine this
/// pass-and-event never serves, so nothing about `op-stop` may touch its entry.
fn other_route_stop(coid: &str) -> crate::replay::JournalOrder {
    crate::replay::JournalOrder {
        venue: Some("binance".into()),
        account: Some("PERP".into()),
        ..stop_candidate(coid, crate::order_owners::PROTECTIVE_STOP_MOUNT)
    }
}

/// Boot 1 files the operator's stop `op-stop` (and `other-key`, see [`other_route_stop`]) in the
/// ownership file; boot 2 adopts `op-stop` at its first pass and is returned.
///
/// ⚠ The engine has NO mount, as in production, where the engine that holds an operator's stop is
/// usually one nobody mounted a strategy on: `assembled` arms the fill capture "as `spawn_core_multi`
/// does for a core with mounts", and that is exactly what hid the defect (an engine with the capture
/// off buffers no lifecycle event), so it is turned off again here.
fn adopted_operators_stop(dir: &Scratch) -> Core {
    let mut first = open(config_on(dir, Vec::new()));
    first.journal_orders = vec![
        stop_candidate("op-stop", crate::order_owners::PROTECTIVE_STOP_MOUNT),
        other_route_stop("other-key"),
    ];
    drop(assembled(first)); // boot 1 files both pseudo entries in the ownership file
    let mut core = assembled(open(config_on(dir, Vec::new())));
    core.engine.collect_applied_fills = false;
    core.engine.tier = vike_model::AccountTier::Demo;
    run_claiming_trigger_orders(&mut core, vec![stop_report("op-stop")], Vec::new());
    assert!(core.engine.registry.contains_key("op-stop"), "precondition: adopted");
    assert!(core.operator_stops.contains("op-stop"), "and remembered as an operator's");
    core
}

/// The coids the ownership file would hand a NEW boot, after `core` is gone.
fn file_coids(core: Core, dir: &Scratch) -> Vec<String> {
    drop(core);
    crate::order_owners::OrderOwnerLog::in_state_dir(dir)
        .start(NOW)
        .coids
        .into_iter()
        .map(|o| o.coid)
        .collect()
}

/// The venue event that ends `coid` the way `how` says.
fn order_ends(how: &str, coid: &str) -> Event {
    use vike_model::events::{OrderCanceled, OrderExpired};
    match how {
        "canceled" => Event::OrderCanceled(OrderCanceled {
            client_order_id: coid.into(),
            reason: String::new().into(),
            ts: 9,
        }),
        "expired" => Event::OrderExpired(OrderExpired { client_order_id: coid.into(), ts: 9 }),
        "filled" => Event::OrderFilled(OrderFilled {
            client_order_id: coid.into(),
            fill: vike_model::events::FillEvent {
                side: -1,
                ..fill_event(coid, "t-stop", 1.0, 90.0)
            },
            ts: 9,
        }),
        other => unreachable!("no such ending: {other}"),
    }
}

/// **The end of an adopted operator's stop forgets its line in the ownership file.** No mount owns
/// the stop, so `coid_mount`'s terminal prune never sees it; without this the line sat in the file
/// until its time-to-live and was restored (and noted) at every boot. A cancel, an expiry and a FILL
/// all end it (a fill has no lifecycle event at all), on an engine with no mount (see
/// [`adopted_operators_stop`]: the first version of this test armed the capture and passed while the
/// real engine never buffered the event). The entry of the other route stays, and the control, where
/// nothing ends the stop, keeps both lines.
#[test]
fn an_adopted_operators_stop_is_forgotten_in_the_file_when_its_order_ends() {
    let file_after = |end: Option<&str>| -> Vec<String> {
        let dir = Scratch::reserved(&format!(
            "vco-restore-operator-stop-forget-{}",
            end.unwrap_or("none")
        ));
        let mut core = adopted_operators_stop(&dir);
        if let Some(how) = end {
            core.dispatch(Ingest::Event(order_ends(how, "op-stop")));
            assert!(
                core.engine.registry["op-stop"].status.is_terminal(),
                "{how}: precondition, the engine folded the event"
            );
            assert!(
                core.operator_stops.is_empty(),
                "{how}: the ended stop is no longer remembered"
            );
        }
        file_coids(core, &dir)
    };
    assert_eq!(file_after(None), ["op-stop", "other-key"], "control: nothing forgot it");
    for how in ["canceled", "expired", "filled"] {
        assert_eq!(
            file_after(Some(how)),
            ["other-key"],
            "{how}: its line goes, the other route's stays"
        );
    }
}

/// **The next boot says nothing of a stop that ended.** The stale line is the whole symptom: the
/// boot noted `RESTORE OPERATOR STOPS` and the pass noted `UNPROVEN` or `NOT adopted` for it at
/// every start. After the end only the entry of the other route is noted.
#[test]
fn the_next_boot_says_nothing_of_an_operators_stop_that_ended() {
    let dir = Scratch::reserved("vco-restore-operator-stop-next-boot");
    let mut core = adopted_operators_stop(&dir);
    core.dispatch(Ingest::Event(order_ends("canceled", "op-stop")));
    drop(core);

    let mut next = assembled(open(config_on(&dir, Vec::new())));
    next.engine.tier = vike_model::AccountTier::Demo;
    run_claiming_trigger_orders(&mut next, Vec::new(), Vec::new());
    run_claiming_trigger_orders(&mut next, Vec::new(), Vec::new());

    assert!(noted(&next, "other-key"), "precondition: the boot still names what waits");
    assert!(!noted(&next, "op-stop"), "nothing names the ended stop: {:?}", next.recent);
}

/// **A stop that still rests is not forgotten.** Another order of the engine ends, and a terminal
/// frame the engine REFUSES (a reject of a stop already accepted is not a legal transition) ends
/// nothing: the registry, not the event's shape, decides.
#[test]
fn an_operators_stop_that_still_rests_is_not_forgotten() {
    let dir = Scratch::reserved("vco-restore-operator-stop-still-rests");
    let mut core = adopted_operators_stop(&dir);
    let mut other = vike_model::ManagedOrder::new(vike_model::OrderRequest {
        client_order_id: "other-order".into(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 1.0,
        order_type: vike_model::OrderType::Limit,
        price: Some(100.0),
        ..Default::default()
    });
    other.status = OrderStatus::Accepted;
    core.engine.registry.insert("other-order".to_string(), other);

    core.dispatch(Ingest::Event(order_ends("canceled", "other-order")));
    assert!(core.engine.registry["other-order"].status.is_terminal(), "precondition: it ended");
    core.dispatch(Ingest::Event(Event::OrderRejected(vike_model::events::OrderRejected {
        client_order_id: "op-stop".into(),
        reason: String::new().into(),
        ts: 9,
    })));

    assert!(!core.engine.registry["op-stop"].status.is_terminal(), "the engine refused the reject");
    assert!(core.operator_stops.contains("op-stop"), "so the stop is still remembered");
    assert_eq!(file_coids(core, &dir), ["op-stop", "other-key"], "and its line stays");
}

/// **A restored operator's stop whose order the venue reports TERMINAL is released at the first
/// pass**, on both ways a pass can meet it: an engine whose client lists no trigger orders releases
/// the entry from memory (the file kept the line), and a claiming `demo` engine hands it to the
/// planner (a terminal report is proof). The line goes in either; a stop the venue still lists live
/// but the client cannot judge keeps its line, as before.
#[test]
fn an_operators_stop_the_venue_reports_ended_is_forgotten_at_the_first_pass() {
    let file_after = |claiming: bool, reported: OrderStatus| -> Vec<String> {
        let dir = Scratch::reserved(&format!(
            "vco-restore-operator-stop-reported-{claiming}-{}",
            reported.as_str()
        ));
        let mut first = open(config_on(&dir, Vec::new()));
        first.journal_orders = vec![
            stop_candidate("op-stop", crate::order_owners::PROTECTIVE_STOP_MOUNT),
            other_route_stop("other-key"),
        ];
        drop(assembled(first));
        let mut core = assembled(open(config_on(&dir, Vec::new())));
        core.engine.tier = vike_model::AccountTier::Demo;
        let row = OrderStatusReport { status: reported, ..stop_report("op-stop") };
        if claiming {
            run_claiming_trigger_orders(&mut core, vec![row], Vec::new());
        } else {
            run(&mut core, vec![row], Vec::new());
        }
        file_coids(core, &dir)
    };
    for claiming in [false, true] {
        assert_eq!(
            file_after(claiming, OrderStatus::Canceled),
            ["other-key"],
            "claiming {claiming}: the venue says it is over, so its line goes"
        );
    }
    assert_eq!(
        file_after(false, OrderStatus::Accepted),
        ["op-stop", "other-key"],
        "control: listed live on a client that cannot judge it, the line stays"
    );
}

/// **A mount's stop is adopted with the same terms**, and its mount is told as for any adopted
/// order. The journal's `reduce_only` and trigger price land on the registry entry whatever the
/// report says.
#[test]
fn a_mounts_stop_is_adopted_with_its_journaled_terms() {
    let dir = Scratch::reserved("vco-restore-mount-stop");
    let (nb, events) = (cell(), Events::default());
    let mut config = open(config_on(&dir, vec![spy_mount(&nb, &events)]));
    config.journal_orders = vec![stop_candidate("j-stop", "maker_b")];
    let mut core = assembled(config);
    assert!(core.restored_orders.contains_key("j-stop"), "precondition: a restored entry");

    run(&mut core, vec![stop_report("j-stop")], Vec::new());

    let order = core.engine.registry.get("j-stop").expect("adopted");
    assert!(order.request.reduce_only);
    assert_eq!(order.request.trigger_price, Some(90.0));
    assert_eq!(heard(&events), vec![("Accepted".to_string(), None)], "its mount is told");
}

/// **The ownership FILE alone rebuilds a stop** (the audit's G6 interim). `the CI box` runs with the
/// journal on, but the file path must carry the terms too: boot 1 mints a reduce-only stop through
/// the one writer (`own_coid`), boot 2 has NO journal candidate, and the adoption still puts the
/// kind, the trigger price and the `reduce_only` flag back on the registry entry, which a venue
/// report never carries.
#[test]
fn the_ownership_file_alone_restores_the_terms_of_a_stop() {
    let dir = Scratch::reserved("vco-restore-file-terms");
    let (nb, events) = (cell(), Events::default());
    let mut first = assembled(config_on(&dir, vec![spy_mount(&nb, &events)]));
    let mut resting = vike_model::ManagedOrder::new(vike_model::OrderRequest {
        client_order_id: "file-stop".into(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: -1,
        qty: 1.0,
        order_type: vike_model::OrderType::Stop,
        trigger_price: Some(90.0),
        reduce_only: true,
        ..Default::default()
    });
    resting.status = OrderStatus::Accepted;
    first.engine.registry.insert("file-stop".to_string(), resting);
    first.own_coid("file-stop".to_string(), 0, None);
    drop(first);

    let mut core = restart(&dir, &nb, &events);
    let entry = core.restored_orders.get("file-stop").expect("the file remembers it");
    assert_eq!(entry.order_type, Some(vike_model::OrderType::Stop), "the kind is on the line");
    assert!(entry.reduce_only && entry.trigger_price == Some(90.0), "so are the terms: {entry:?}");

    run(&mut core, vec![stop_report("file-stop")], Vec::new());

    let adopted = core.engine.registry.get("file-stop").expect("adopted");
    assert!(adopted.request.reduce_only, "still reduce-only, from the file");
    assert_eq!(adopted.request.trigger_price, Some(90.0), "at its level, from the file");
    assert_eq!(heard(&events), vec![("Accepted".to_string(), None)], "its mount is told");
}

/// The pseudo owner is a fallback GUESS, never a claim: when the ownership file already names a
/// real mount for the coid, the journal's pseudo candidate is no `RESTORE OWNER CONFLICT`, the
/// file's owner stands and the journal's facts still supplement the entry.
#[test]
fn the_pseudo_owner_never_conflicts_with_a_real_one() {
    let dir = Scratch::reserved("vco-restore-pseudo-no-conflict");
    let (coid, nb, events) = rest_one(&dir);
    let mut config = open(config_on(&dir, vec![spy_mount(&nb, &events)]));
    config.journal_orders = vec![stop_candidate(&coid, crate::order_owners::PROTECTIVE_STOP_MOUNT)];
    let core = assembled(config);

    assert!(!noted(&core, "RESTORE OWNER CONFLICT"), "recent {:?}", core.recent);
    let held = core.restored_orders.get(&coid).expect("the file's entry stands");
    assert_eq!(held.mount_id, "maker_b", "the file's owner");
    assert!(held.reduce_only, "and the journal's terms supplement it");
    assert_eq!(held.trigger_price, Some(90.0));
}

/// **The journal's price and trigger price win over the ownership line's** where it knows them: the
/// line was written at the mint, and a modify has moved the order since (the journal folds the
/// registry's CURRENT request). The line's terms are only the fallback for a journal that carries
/// none.
#[test]
fn the_journals_modified_terms_win_over_the_lines_mint_time_ones() {
    // a limit quoted at 100, modified to 101.5 while the process lived
    let dir = Scratch::reserved("vco-restore-journal-price-wins");
    let (coid, nb, events) = rest_one(&dir);
    let mut config = open(config_on(&dir, vec![spy_mount(&nb, &events)]));
    config.journal_orders = vec![crate::replay::JournalOrder {
        price: Some(101.5),
        trigger_price: None,
        reduce_only: false,
        ..stop_candidate(&coid, "maker_b")
    }];
    let core = assembled(config);
    let entry = core.restored_orders.get(&coid).expect("restored");
    assert_eq!(entry.price, Some(101.5), "the modified quote, not the mint-time 100: {entry:?}");

    // a stop minted at 90 and moved to 85
    let dir = Scratch::reserved("vco-restore-journal-trigger-wins");
    let (coid, nb, events) = rest_one_stop(&dir);
    let mut config = open(config_on(&dir, vec![spy_mount(&nb, &events)]));
    config.journal_orders = vec![crate::replay::JournalOrder {
        trigger_price: Some(85.0),
        ..stop_candidate(&coid, "maker_b")
    }];
    let core = assembled(config);
    let entry = core.restored_orders.get(&coid).expect("restored");
    assert_eq!(entry.trigger_price, Some(85.0), "the moved trigger: {entry:?}");

    // the control: a journal that carries no terms leaves the line's
    let dir = Scratch::reserved("vco-restore-journal-silent-keeps-line");
    let (coid, nb, events) = rest_one_stop(&dir);
    let mut config = open(config_on(&dir, vec![spy_mount(&nb, &events)]));
    config.journal_orders = vec![crate::replay::JournalOrder {
        trigger_price: None,
        ..stop_candidate(&coid, "maker_b")
    }];
    let core = assembled(config);
    let entry = core.restored_orders.get(&coid).expect("restored");
    assert_eq!(entry.trigger_price, Some(90.0), "the line's, as the fallback: {entry:?}");
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
    assert_eq!(core.engine.tier, vike_model::AccountTier::Paper, "precondition: a paper engine");
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

/// The note sample both restore notes print (the boot notes and the reconcile pass's RESTORE
/// note): the first eight coids joined by `, `, then ` (N more)`; nothing at all for an empty list.
#[test]
fn a_restore_note_lists_eight_coids_then_counts_the_rest() {
    let coids = |n: usize| (0..n).map(|i| format!("c{i}")).collect::<Vec<String>>();
    assert_eq!(super::boot_restore::sample(&coids(0)), "");
    assert_eq!(super::boot_restore::sample(&coids(2)), "c0, c1");
    assert_eq!(super::boot_restore::sample(&coids(8)), "c0, c1, c2, c3, c4, c5, c6, c7");
    assert_eq!(super::boot_restore::sample(&coids(10)), "c0, c1, c2, c3, c4, c5, c6, c7 (2 more)");
}

// --- the GATEWAY CURSOR (journal v18): `Ingest::GatewayEvent` folds as the event it carries, holds
// --- its ring's cursor for the next `Snap`, and what a replayed event can still do to a book the
// --- restore and a reconcile pass already settled (the interplay the ring replay is owed).

fn cursor(gw_epoch: u64, seq: u64) -> GatewayCursor {
    GatewayCursor { gw_epoch, seq, ts_ms: seq as i64 }
}

fn via_gateway(event: Event, route_key: &str, c: GatewayCursor) -> Ingest {
    Ingest::GatewayEvent(Box::new(GatewayEvent { event, route_key: route_key.into(), cursor: c }))
}

/// A `sim` buy fill of `coid`: `qty` at `px`, under the trade id `tid`.
fn fill_event(coid: &str, tid: &str, qty: f64, px: f64) -> vike_model::events::FillEvent {
    vike_model::events::FillEvent {
        trade_id: vike_model::events::TradeId::new(tid).expect("a non-empty trade id"),
        client_order_id: coid.to_string(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        last_qty: qty,
        last_px: px,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "taker".into(),
        ts: 5,
        mark_price: None,
        position_side: vike_model::events::PositionSide::Both,
    }
}

fn fill_ev(tid: &str) -> Event {
    Event::Fill(fill_event("", tid, 1.0, 100.0))
}

/// One `sim` pass under `hybrid`, so a `MissingFill` FOLDS (the other tests hold it under
/// `quarantine`), delivered as `handle` does after the command.
fn hybrid_pass(core: &mut Core, orders: Vec<OrderStatusReport>, fills: Vec<FillReport>) {
    core.reconcile_reports(ReconcileReports {
        venue: "sim".into(),
        since: 0,
        orders,
        fills,
        positions: Vec::new(),
        policy: ReconPolicy::hybrid(),
        balance: None,
        generate_missing_orders: false,
        reconcile_balance: false,
        balance_tol: BalanceTol::default(),
        route_key: None,
        admitted_symbols: None,
        lists_trigger_orders: false,
    });
    core.dispatch_applied_fills();
}

fn position(core: &Core) -> f64 {
    core.engine.position_size(vike_model::events::PositionSide::Both)
}

/// **A gateway event folds exactly as the plain event it carries.** The same fill through each door
/// leaves the same position, balance and dedup ledger: the `GatewayEvent` arm and the `Event` arm
/// share one fold, and the cursor is bookkeeping beside it.
#[test]
fn a_gateway_event_folds_as_the_event_it_carries() {
    let mut plain = core_of(engine("sim"), Vec::new(), CoreConfig::default());
    let mut gateway = core_of(engine("sim"), Vec::new(), CoreConfig::default());
    plain.dispatch(Ingest::Event(Event::Fill(fill_event("", "t-1", 2.0, 100.0))));
    gateway.dispatch(via_gateway(
        Event::Fill(fill_event("", "t-1", 2.0, 100.0)),
        "sim",
        cursor(7, 1),
    ));
    assert_eq!(position(&gateway), 2.0);
    assert_eq!(position(&gateway).to_bits(), position(&plain).to_bits());
    assert_eq!(
        gateway.engine.account.balance.to_bits(),
        plain.engine.account.balance.to_bits(),
        "the same money"
    );
    assert_eq!(
        gateway.engine.seen_trade_ids().collect::<Vec<_>>(),
        plain.engine.seen_trade_ids().collect::<Vec<_>>(),
        "and the same dedup ledger"
    );
    assert!(gateway.dirty, "and the fold marked the core for a publish, as an Event does");
}

/// **The core holds each ring's cursor by the one rule** (`GatewayCursor::after`): the larger `seq`
/// inside an epoch, the later arrival across epochs, one entry per account in first-seen order. A
/// straggler a replay re-delivers never rewinds the resume point.
#[test]
fn the_core_holds_each_rings_cursor_by_the_one_rule() {
    let mut core = core_of(engine("sim"), Vec::new(), CoreConfig::default());
    let held = |core: &Core| -> Vec<(String, GatewayCursor)> {
        core.gateway_cursors.iter().map(|(k, c)| (k.to_string(), *c)).collect()
    };
    assert!(held(&core).is_empty(), "a core that never heard a gateway holds no cursor");

    core.dispatch(via_gateway(fill_ev("a1"), "sim", cursor(7, 5)));
    core.dispatch(via_gateway(fill_ev("b1"), "sim#ALT", cursor(7, 2)));
    core.dispatch(via_gateway(fill_ev("a2"), "sim", cursor(7, 3))); // a straggler
    assert_eq!(
        held(&core),
        vec![("sim".to_string(), cursor(7, 5)), ("sim#ALT".to_string(), cursor(7, 2))]
    );
    core.dispatch(via_gateway(fill_ev("a3"), "sim", cursor(7, 6)));
    core.dispatch(via_gateway(fill_ev("b2"), "sim#ALT", cursor(9, 1))); // a new epoch
    assert_eq!(
        held(&core),
        vec![("sim".to_string(), cursor(7, 6)), ("sim#ALT".to_string(), cursor(9, 1))]
    );
}

/// A core assembled over the restore's cursors holds them (the next `Snap` will carry them), and a
/// plain `Ingest::Event` never touches them.
#[test]
fn the_restored_cursors_seed_the_core_and_a_plain_event_leaves_them_alone() {
    let config = CoreConfig {
        gateway_cursors: vec![("sim#ALT".to_string(), cursor(4, 40))],
        ..CoreConfig::default()
    };
    let mut core = core_of(engine("sim"), Vec::new(), config);
    assert_eq!(core.gateway_cursors.len(), 1);
    core.dispatch(Ingest::Event(fill_ev("p1")));
    assert_eq!(core.gateway_cursors.len(), 1);
    assert_eq!(core.gateway_cursors[0].0.as_str(), "sim#ALT");
    assert_eq!(core.gateway_cursors[0].1, cursor(4, 40));
}

/// **THE OWED INTERPLAY TEST, order one: the reconcile pass books the fill first, the ring then
/// replays it.** The boot restore left an order resting; while the tradehub was down it filled. The
/// first pass finds it `filled_while_down` and the `MissingFill` path (folding, under `hybrid`)
/// books the venue's fill under its REAL trade id. The resume then replays what the ring held --
/// the very same fill, its lifecycle wrap, and the order's earlier events -- over the same trade
/// link: one fill, one position change.
#[test]
fn a_fill_the_pass_booked_is_not_booked_again_by_the_ring_replay() {
    let dir = Scratch::reserved("gw-interplay-pass-then-replay");
    let (coid, nb, events) = rest_one(&dir);
    let mut core = restart(&dir, &nb, &events);

    hybrid_pass(&mut core, Vec::new(), vec![fill_of(&coid)]);
    assert_eq!(core.restore_counters.restored_filled_while_down, 1, "precondition: filled down");
    assert_eq!(position(&core), 1.0, "precondition: the MissingFill path booked the fill");
    let tid = fill_of(&coid).trade_id.to_string();
    let fill = || fill_event(&coid, &tid, 1.0, 100.0);
    let book_before = (position(&core).to_bits(), core.engine.account.balance.to_bits());

    // The replay: the order's events as the ring held them, `replayed:true` on the wire.
    let replay = [
        Event::OrderSubmitted(OrderSubmitted { client_order_id: coid.clone(), ts: 1 }),
        Event::OrderAccepted(OrderAccepted {
            client_order_id: coid.clone(),
            venue_order_id: None,
            ts: 2,
        }),
        Event::Fill(fill()),
        Event::OrderFilled(OrderFilled { client_order_id: coid.clone(), fill: fill(), ts: 5 }),
    ];
    for (i, event) in replay.into_iter().enumerate() {
        core.dispatch(via_gateway(event, "sim", cursor(7, 1 + i as u64)));
    }

    assert_eq!(position(&core), 1.0, "ONE position change, not two");
    assert_eq!(
        core.engine.account.duplicate_fills_refused, 1,
        "the replayed fill was offered and refused by the ledger, not folded"
    );
    assert_eq!(
        (position(&core).to_bits(), core.engine.account.balance.to_bits()),
        book_before,
        "the book is exactly what the pass left"
    );
    assert_eq!(core.engine.dropped_terminal_on_live, 0, "no terminal is read as lost");
    assert_eq!(core.engine.stranded_terminal_drops, 0, "and nothing is read as stranded");
    assert_eq!(core.gateway_cursors.len(), 1);
    assert_eq!(core.gateway_cursors[0].1, cursor(7, 4), "the cursor advanced");
}

/// **…and the other order: the ring replay delivers the fill first, the pass then finds it already
/// booked.** The pass still counts the order `filled_while_down` (the restore's own bookkeeping),
/// but its `MissingFill` check consults the engine's seen trade ids, so the fill the replay booked
/// is no divergence and nothing folds a second time.
#[test]
fn a_fill_the_ring_replay_booked_is_not_booked_again_by_the_pass() {
    let dir = Scratch::reserved("gw-interplay-replay-then-pass");
    let (coid, nb, events) = rest_one(&dir);
    let mut core = restart(&dir, &nb, &events);
    let tid = fill_of(&coid).trade_id.to_string();

    core.dispatch(via_gateway(
        Event::Fill(fill_event(&coid, &tid, 1.0, 100.0)),
        "sim",
        cursor(7, 1),
    ));
    assert_eq!(position(&core), 1.0, "precondition: the replay booked the fill");

    hybrid_pass(&mut core, Vec::new(), vec![fill_of(&coid)]);

    assert_eq!(position(&core), 1.0, "ONE position change, not two");
    assert_eq!(
        core.engine.account.duplicate_fills_refused, 0,
        "the pass never even OFFERED the booked fill: the diff saw it in the seen ids, not the fold's dedup"
    );
    assert_eq!(core.restore_counters.restored_filled_while_down, 1);
    assert!(
        core.recon_alerts.values().all(|a| a.kind != vike_exec::recon::DivergenceKind::MissingFill),
        "the booked fill is no divergence: {:?}",
        core.recon_alerts.values().map(|a| a.kind).collect::<Vec<_>>()
    );
}

/// **THE OWED TOLERANCE TEST: a re-delivered prefix of an order's events changes nothing.** An
/// adopted order takes its fills; then the whole venue story is delivered AGAIN from the top (the
/// shape a ring replay or a reconnect resync takes). The order, the position, the balance and the
/// seen ledgers are exactly what the first delivery left, and neither the lost-terminal counter
/// (`dropped_terminal_on_live`) nor the stranded-position counter moves: a benign replay is not
/// counted as a fault.
#[test]
fn a_redelivered_prefix_of_order_events_changes_nothing() {
    let dir = Scratch::reserved("gw-redelivered-prefix");
    let (coid, nb, events) = rest_one(&dir);
    let mut core = restart(&dir, &nb, &events);
    run(&mut core, vec![report(&coid, OrderStatus::Accepted)], Vec::new());
    assert!(core.engine.registry.contains_key(&coid), "precondition: the order is adopted");

    let story = |with_prefix: bool| -> Vec<Event> {
        let (f1, f2) = (fill_event(&coid, "r-1", 0.4, 100.0), fill_event(&coid, "r-2", 0.6, 110.0));
        let mut out = Vec::new();
        if with_prefix {
            out.push(Event::OrderSubmitted(OrderSubmitted {
                client_order_id: coid.clone(),
                ts: 1,
            }));
            out.push(Event::OrderAccepted(OrderAccepted {
                client_order_id: coid.clone(),
                venue_order_id: None,
                ts: 2,
            }));
        }
        out.push(Event::Fill(f1.clone()));
        out.push(Event::OrderPartiallyFilled(OrderPartiallyFilled {
            client_order_id: coid.clone(),
            fill: f1,
            ts: 6,
        }));
        out.push(Event::Fill(f2.clone()));
        out.push(Event::OrderFilled(OrderFilled {
            client_order_id: coid.clone(),
            fill: f2,
            ts: 7,
        }));
        out
    };
    let observe = |core: &Core| {
        let o = &core.engine.registry[&coid];
        (
            o.status,
            o.filled_qty.to_bits(),
            o.avg_fill_px.to_bits(),
            position(core).to_bits(),
            core.engine.account.balance.to_bits(),
            core.engine.account.realized_pnl.to_bits(),
        )
    };

    for (i, event) in story(false).into_iter().enumerate() {
        core.dispatch(via_gateway(event, "sim", cursor(3, 10 + i as u64)));
    }
    let first = observe(&core);
    assert_eq!(first.0, OrderStatus::Filled, "precondition: the story completed the order");
    assert_eq!(f64::from_bits(first.1), 1.0, "precondition: 0.4 + 0.6, once");

    for (i, event) in story(true).into_iter().enumerate() {
        core.dispatch(via_gateway(event, "sim", cursor(3, 20 + i as u64)));
    }
    assert_eq!(observe(&core), first, "the replay moved nothing");
    assert_eq!(
        core.engine.dropped_terminal_on_live, 0,
        "no benign replay is counted as a lost terminal"
    );
    assert_eq!(core.engine.stranded_terminal_drops, 0, "nor as a stranded position");
}

// --- the cursor's two guards: a REDELIVERY is dropped before the journal and the fold, and a late
// --- straggler of an OLD epoch is not (the epoch is random: nothing says it is the older one).

/// A funding payment on `sim`/`BTCUSDT`: no id, no dedup, `Account::apply_funding` is a bare `+=`.
fn funding_ev(amount: f64) -> Event {
    Event::Funding(vike_model::events::FundingEvent {
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        position_side: vike_model::events::PositionSide::Both,
        funding_rate: 0.0001,
        amount,
        mark_price: None,
        ts: 1,
        route_key: None,
        ccy: None,
    })
}

/// Everything a redelivery could double: position, money, funding, the dedup ledger and the fill
/// dedup's own counter.
fn book(core: &Core) -> (u64, u64, u64, usize, u64) {
    (
        position(core).to_bits(),
        core.engine.account.balance.to_bits(),
        core.engine.account.funding_paid.to_bits(),
        core.engine.seen_trade_ids().count(),
        core.engine.account.duplicate_fills_refused,
    )
}

/// **A redelivered prefix of a ring (same epoch, `seq` at or below the held cursor) changes
/// nothing**: not the position, the money, the funding (a bare `+=` no dedup could save), the dedup
/// ledger, the counter of refused duplicates, the held cursor or the publish flag. The event AFTER
/// the cursor still folds, and a different epoch is never a redelivery.
#[test]
fn a_redelivered_prefix_of_the_ring_is_dropped_before_the_fold() {
    let mut core = core_of(engine("sim"), Vec::new(), CoreConfig::default());
    let story = || [(fill_ev("r1"), 1), (funding_ev(-1.25), 2), (fill_ev("r2"), 3)];
    for (event, seq) in story() {
        core.dispatch(via_gateway(event, "sim", cursor(7, seq)));
    }
    let first = book(&core);
    assert_eq!(position(&core), 2.0, "precondition: both fills folded");
    assert_eq!(core.engine.account.funding_paid, -1.25, "precondition: the funding folded");

    core.dirty = false;
    for (event, seq) in story() {
        core.dispatch(via_gateway(event, "sim", cursor(7, seq)));
    }
    assert_eq!(
        book(&core),
        first,
        "the redelivered prefix, the held `seq` included, moved nothing"
    );
    assert_eq!(core.gateway_cursors[0].1, cursor(7, 3), "and the cursor stayed");
    assert!(!core.dirty, "nothing was folded, so nothing needs a publish");

    core.dispatch(via_gateway(funding_ev(-0.5), "sim", cursor(7, 4)));
    assert_eq!(core.engine.account.funding_paid, -1.75, "the event after the cursor folds");
    core.dispatch(via_gateway(funding_ev(-0.25), "sim", cursor(9, 1)));
    assert_eq!(core.engine.account.funding_paid, -2.0, "a new epoch is a new ring, not a replay");
}

/// **The OWED interplay, `PositionDrift` then ring replay: DOES NOT HOLD (`#[ignore]`d, reported).**
/// Under `hybrid`/`synthesize` a position the venue holds and the engine does not is rewritten by
/// `PositionDrift` through SYNTHESIZED legs (their own trade ids). The two fills that caused the
/// drift are then replayed from the ring under their REAL trade ids, which no ledger has seen, so
/// they fold on top: the position counts them twice. `MissingFill` is safe (its fill carries the
/// real id: see `a_fill_the_pass_booked_is_not_booked_again_by_the_ring_replay`); `PositionDrift`
/// has no id to dedup on, and the cursor cannot help (the replayed seqs are above the held one).
/// The root `CLAUDE.md` rollout rule keeps `hybrid`/`synthesize` off every gateway venue, which is
/// all that protects this today. The cure is a design decision (hold the ring replay until the pass
/// ends, or have the pass retire the ring's range it covered), not a line.
#[test]
#[ignore = "known double count: PositionDrift synthesizes ids, the ring replay brings the real ones (DOUBTS, w2_w3c)"]
fn a_position_the_pass_synthesized_is_not_booked_again_by_the_ring_replay() {
    let mut core = core_of(engine("sim"), Vec::new(), CoreConfig::default());
    core.dispatch(via_gateway(fill_ev("a1"), "sim", cursor(7, 1)));
    assert_eq!(position(&core), 1.0, "precondition: one fill folded live");

    // Two more fills happened while the link was down (the pass's fill window did not reach them);
    // the venue's position report says 3.
    let venue_position = vike_model::PositionStatusReport {
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        position_side: vike_model::events::PositionSide::Both,
        qty: 3.0,
        avg_px: 100.0,
        ts: 5,
        margin_mode: Default::default(),
        isolated_margin: None,
        delta: None,
    };
    core.reconcile_reports(ReconcileReports {
        venue: "sim".into(),
        since: 0,
        orders: Vec::new(),
        fills: Vec::new(),
        positions: vec![venue_position],
        policy: ReconPolicy::hybrid(),
        balance: None,
        generate_missing_orders: false,
        reconcile_balance: false,
        balance_tol: BalanceTol::default(),
        route_key: None,
        admitted_symbols: None,
        lists_trigger_orders: false,
    });
    core.dispatch_applied_fills();
    assert_eq!(position(&core), 3.0, "precondition: PositionDrift rewrote the size to the venue's");

    // The resume replays the two real fills from the ring.
    core.dispatch(via_gateway(fill_ev("a2"), "sim", cursor(7, 2)));
    core.dispatch(via_gateway(fill_ev("a3"), "sim", cursor(7, 3)));
    assert_eq!(position(&core), 3.0, "the replayed fills were already counted by the drift");
}

/// **A late straggler of the OLD epoch rewinds the held cursor to it -- pinned, not cured.** An
/// epoch is a random number, so nothing here can tell that `epoch 7` is the older one once `epoch 9`
/// has spoken: `GatewayCursor::after` takes the later arrival across epochs, and the redelivery
/// guard only judges the SAME epoch. The straggler is a real event and still folds (a fill is not
/// lost); only the resume point is wrong, and it errs to the safe side: the next resume presents an
/// epoch the gateway does not hold, which it must answer as "no cursor" (a reconcile then recovers
/// what the ring would have). The cure would be per-account memory of retired epochs that the
/// restore's journal fold shares, which is the gateway producer's contract to make unnecessary
/// (one ordered link per account), so it is left to the wave that writes the producer.
#[test]
fn a_late_straggler_of_the_old_epoch_rewinds_the_cursor_and_still_folds() {
    let mut core = core_of(engine("sim"), Vec::new(), CoreConfig::default());
    core.dispatch(via_gateway(fill_ev("old-1"), "sim", cursor(7, 5)));
    core.dispatch(via_gateway(fill_ev("new-1"), "sim", cursor(9, 1)));
    assert_eq!(core.gateway_cursors[0].1, cursor(9, 1), "precondition: the new epoch holds");

    core.dispatch(via_gateway(fill_ev("old-2"), "sim", cursor(7, 6)));
    assert_eq!(position(&core), 3.0, "the straggler is a real fill: folded, not lost");
    assert_eq!(
        core.gateway_cursors[0].1,
        cursor(7, 6),
        "and the cursor now names the old epoch (the next resume is answered `no cursor`)"
    );
}
