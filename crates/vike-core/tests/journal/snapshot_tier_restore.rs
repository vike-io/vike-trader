//! WHOSE BOOK a journal's snapshot is (`vike_exec::EngineSnapshot::tier`), through a REAL core and a
//! journal on disk: a core told its tier journals, stops, and the next boot restores (refolding the
//! journal through a replay core) into an engine of the same or another tier.
//!
//! The white-box twin is `src/runtime/tests/boot_restore.rs`; these gate what only the journal can
//! show: that the tier survives the replay core's re-snapshot (so the boot restore compares the
//! journal's tier and not a seed), that a core's next `Snap` names the tier it runs at, that a journal
//! written by an engine nobody told its tier (every journal written before the field) still restores,
//! and that the determinism fence does not see the tier.

use crate::scratch::Scratch;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use vike_core::replay::{RestoreOutcome, restore_for_boot};
use vike_core::{CoreConfig, JournalConfig, spawn_core_multi};
use vike_exec::CoreSnapshot;
use vike_journal::{CommandJournal, JournalFileConfig, JournalRecord};
use vike_model::{AccountTier, Clock, TradingState};

use crate::kit::engines::sim_engine;
use crate::kit::events::{sim_bare_fill, sim_quote};
use crate::kit::handle::wait_for_snapshot;

fn journal_at(dir: &Path) -> JournalConfig {
    JournalConfig {
        dir: dir.to_path_buf(),
        file: JournalFileConfig { segment_bytes: 1 << 20, flush_every: 8 },
        snapshot_every: 1_000,
    }
}

/// A self-advancing clock (message `k` dispatches at `now_ms == k`).
fn counter_clock() -> Box<dyn Clock + Send> {
    let t = Arc::new(AtomicI64::new(0));
    Box::new(move || t.fetch_add(1, Ordering::Relaxed))
}

fn config(dir: &Path) -> CoreConfig {
    CoreConfig {
        seed_cash: 1_000.0,
        clock: counter_clock(),
        coid_session: Some(("cafef00d".into(), 0)),
        journal: Some(journal_at(dir)),
        ..CoreConfig::default()
    }
}

fn position(s: &CoreSnapshot) -> f64 {
    s.portfolio.venues.iter().flat_map(|v| v.positions.iter()).map(|p| p.size).sum()
}

/// The engine of a core that was told its tier (`None`: nobody told it, as before the field).
fn engine_at(
    tier: Option<AccountTier>,
) -> vike_exec::ExecutionEngine<vike_exec::testing::RecordingClient> {
    let mut e = sim_engine();
    if let Some(tier) = tier {
        e.set_tier(tier);
    }
    e
}

/// Run 1: a core told `tier` takes a 1.0 fill on `sim`, journals it and stops (the exit `Snap`).
fn first_run(dir: &Path, tier: Option<AccountTier>) {
    let handle = spawn_core_multi(engine_at(tier), Vec::new(), config(dir));
    handle.event_sender().blocking_send(sim_bare_fill("t-1", 1.0, 100.0)).unwrap();
    wait_for_snapshot(&handle.snapshot_cell(), 10, "the position", |s| position(s) == 1.0);
    handle.shutdown_and_join();
}

/// What run 2 saw.
struct Second {
    /// The tier the boot restore handed the core for its engine.
    carried: Option<AccountTier>,
    position: f64,
    trading_state: TradingState,
    notes: Vec<String>,
    /// The tier the LAST `Snap` of the journal names after run 2 stopped.
    next_snap_tier: Option<AccountTier>,
    next_snap_json: String,
}

/// How run 2's boot restore reaches the journal.
#[derive(Clone, Copy)]
enum Via {
    /// The refold through the replay core (`RestoreOutcome::Full`).
    Refold,
    /// The latest `Snap` alone (decision 0127, verdict 5): the replay core cannot be staged.
    SnapOnly,
}

/// Run 2: the boot restore over `dir`, then a core told `tier` assembled over it, on the same journal.
fn second_run(dir: &Path, tier: AccountTier) -> Second {
    second_run_via(dir, tier, Via::Refold)
}

fn second_run_via(dir: &Path, tier: AccountTier, via: Via) -> Second {
    let tmp = Scratch::created("snapshot-tier-restore-tmp");
    // A scratch root that is a FILE cannot hold the replay core's journal, so the restore falls back
    // to the latest `Snap` (the same recipe `tests/journal/boot_restore.rs` uses).
    let root = match via {
        Via::Refold => tmp.to_path_buf(),
        Via::SnapOnly => {
            let file = tmp.join("not-a-dir");
            std::fs::write(&file, b"a file").unwrap();
            file
        }
    };
    let restore = restore_for_boot(Some(&journal_at(dir)), &root, 42).expect("readable");
    match via {
        Via::Refold => {
            assert_eq!(restore.outcome(), &RestoreOutcome::Full, "{:?}", restore.notes())
        }
        Via::SnapOnly => assert!(
            matches!(restore.outcome(), RestoreOutcome::SnapOnly { .. }),
            "{:?}",
            restore.outcome()
        ),
    }
    let carried = restore.restored_engines()[0].tier;
    let mut cfg = config(dir);
    restore.apply_to(&mut cfg);
    let handle = spawn_core_multi(engine_at(Some(tier)), Vec::new(), cfg);
    handle.tick_sender().quote(sim_quote(100.0, 2)).unwrap(); // a publish
    let cell = handle.snapshot_cell();
    wait_for_snapshot(&cell, 10, "the restore's summary on the ring", |s| {
        s.recent_events.iter().any(|l| l.contains("put back on"))
    });
    let snap = cell.load_full();
    let out = (
        position(&snap),
        snap.trading_state,
        snap.recent_events.iter().map(|l| l.to_string()).collect::<Vec<_>>(),
    );
    handle.shutdown_and_join();
    let last = CommandJournal::read_all(dir)
        .unwrap()
        .into_iter()
        .rev()
        .find_map(|r| match r {
            JournalRecord::Snap { engines, .. } => Some(engines),
            _ => None,
        })
        .expect("run 2 wrote its exit Snap");
    Second {
        carried,
        position: out.0,
        trading_state: out.1,
        notes: out.2,
        next_snap_tier: last[0].tier,
        next_snap_json: serde_json::to_string(&last[0]).unwrap(),
    }
}

fn said(second: &Second, needle: &str) -> bool {
    second.notes.iter().any(|l| l.contains(needle))
}

/// **THE COLD FLIP, every way round**: whichever tier the journal's engine ran at, an engine of
/// another tier that takes over its route key restores nothing of it, says so, and comes up halted
/// (the book held a position); its own next `Snap` then names ITS tier.
#[test]
fn a_cold_tier_flip_restores_nothing_of_the_other_tiers_book() {
    use AccountTier::{Demo, Live, Paper};
    for (before, after) in
        [(Demo, Live), (Live, Demo), (Paper, Demo), (Paper, Live), (Demo, Paper), (Live, Paper)]
    {
        let dir = Scratch::reserved("snapshot-tier-flip");
        first_run(&dir, Some(before));
        let second = second_run(&dir, after);
        let case = format!("{before:?} journal, {after:?} engine");
        assert_eq!(second.carried, Some(before), "{case}: the replay core lost the tier");
        assert_eq!(second.position, 0.0, "{case}: the position came across");
        assert_eq!(second.trading_state, TradingState::Halted, "{case}");
        assert!(said(&second, "TIER MISMATCH"), "{case}: {:?}", second.notes);
        assert_eq!(second.next_snap_tier, Some(after), "{case}: the next Snap names its tier");
    }
}

/// **UPGRADE**: a journal written by an engine nobody told its tier (the shape of every journal
/// before the field) restores into an engine of ANY tier exactly as before, loses no position, and
/// the first `Snap` after it names the tier. The old bytes have no tier key at all.
#[test]
fn a_journal_without_a_tier_still_restores_and_the_next_snap_names_one() {
    for tier in AccountTier::ALL {
        let dir = Scratch::reserved("snapshot-tier-upgrade");
        first_run(&dir, None);
        let before = CommandJournal::read_all(&dir).unwrap();
        for r in &before {
            if let JournalRecord::Snap { engines, .. } = r {
                assert!(
                    !serde_json::to_string(engines).unwrap().contains("\"tier\""),
                    "an untold engine writes the bytes it always did"
                );
            }
        }
        let second = second_run(&dir, tier);
        assert_eq!(second.carried, None, "{tier:?}: unknown stays unknown through the refold");
        assert_eq!(second.position, 1.0, "{tier:?}: the upgrade lost the position");
        assert_eq!(second.trading_state, TradingState::Active, "{tier:?}");
        assert!(!said(&second, "TIER MISMATCH"), "{tier:?}: {:?}", second.notes);
        assert_eq!(second.next_snap_tier, Some(tier), "{tier:?}: stamped from here on");
    }
}

/// The same tier restores, for every tier; and the next `Snap` repeats it.
#[test]
fn a_journal_of_the_same_tier_restores() {
    for tier in AccountTier::ALL {
        let dir = Scratch::reserved("snapshot-tier-same");
        first_run(&dir, Some(tier));
        let second = second_run(&dir, tier);
        assert_eq!(second.carried, Some(tier));
        assert_eq!(second.position, 1.0, "{tier:?}");
        assert_eq!(second.trading_state, TradingState::Active, "{tier:?}");
        assert_eq!(second.next_snap_tier, Some(tier), "{tier:?}");
        assert!(second.next_snap_json.contains(&format!("\"tier\":\"{}\"", tier.as_str())));
    }
}

/// **The determinism fence does not see the tier**: the offline replay of a journal written by a
/// `live` core reproduces the recorded final hash (the hash excludes the tier, so a build that
/// predates the field computes the same one), and the replayed engine still says `live`.
#[test]
fn the_replay_fence_holds_for_a_journal_that_names_its_tier() {
    let dir = Scratch::reserved("snapshot-tier-fence");
    first_run(&dir, Some(AccountTier::Live));
    let recs = CommandJournal::read_all(&dir).unwrap();
    let recorded = recs
        .iter()
        .rev()
        .find_map(|r| match r {
            JournalRecord::Snap { hash, .. } => Some(*hash),
            _ => None,
        })
        .expect("an exit Snap");
    let out = vike_core::replay::replay_offline(&dir).expect("replay verifies");
    assert_eq!(out.final_hash, recorded, "the fence is the recorded hash");
    assert_eq!(out.engines[0].tier, Some(AccountTier::Live));
}

/// **The latest `Snap` alone refuses too** (decision 0127, verdict 5): when the replay core cannot be
/// staged the restore hands back the `Snap`'s own engines, which state the tier they ran at.
#[test]
fn a_snap_only_restore_refuses_the_other_tiers_book_too() {
    let dir = Scratch::reserved("snapshot-tier-snap-only");
    first_run(&dir, Some(AccountTier::Demo));
    let second = second_run_via(&dir, AccountTier::Live, Via::SnapOnly);
    assert_eq!(second.carried, Some(AccountTier::Demo), "the Snap's own tier");
    assert_eq!(second.position, 0.0);
    assert_eq!(second.trading_state, TradingState::Halted);
    assert!(said(&second, "TIER MISMATCH"), "{:?}", second.notes);

    let same = Scratch::reserved("snapshot-tier-snap-only-same");
    first_run(&same, Some(AccountTier::Live));
    let restored = second_run_via(&same, AccountTier::Live, Via::SnapOnly);
    assert_eq!(restored.position, 1.0, "the same tier restores from the Snap alone");
}

/// **A refusal is durable before any other `Snap`** (the review's kill -9): the core refuses the
/// other tier's book, the operator resumes it with `SetTradingState(Active)` and the account takes a
/// fill at its own tier, and the process dies before it writes anything else (the journal is copied
/// as it stands, no exit `Snap`). The next boot restores from the `Snap` the refusal wrote at once:
/// it states `live`, so it does NOT refuse again, the resume is kept (the command folds from the
/// tail) and the fill taken after it is on the book instead of vanishing with the discarded one. The
/// refused book was saved for a hand recovery under the state directory.
#[test]
fn a_refusal_survives_a_crash_before_any_other_snap() {
    let dir = Scratch::reserved("snapshot-tier-crash");
    let copy = Scratch::reserved("snapshot-tier-crash-copy");
    let state = Scratch::created("snapshot-tier-crash-state");
    first_run(&dir, Some(AccountTier::Demo));

    // Boot 2: a LIVE engine over the demo journal.
    let tmp = Scratch::created("snapshot-tier-crash-tmp");
    let restore = restore_for_boot(Some(&journal_at(&dir)), &tmp, 42).expect("readable");
    let mut cfg = config(&dir);
    cfg.state_dir = Some(state.to_path_buf());
    restore.apply_to(&mut cfg);
    let handle = spawn_core_multi(engine_at(Some(AccountTier::Live)), Vec::new(), cfg);
    handle.tick_sender().quote(sim_quote(100.0, 2)).unwrap(); // a publish
    let cell = handle.snapshot_cell();
    wait_for_snapshot(&cell, 10, "the refusal on the ring and the engine halted", |s| {
        s.trading_state == TradingState::Halted
            && s.recent_events.iter().any(|l| l.contains("TIER MISMATCH"))
    });
    handle.send_command(vike_exec::Command::SetTradingState(TradingState::Active));
    handle.event_sender().blocking_send(sim_bare_fill("t-live", 2.0, 100.0)).unwrap();
    wait_for_snapshot(&cell, 10, "the resume and the live fill", |s| {
        s.trading_state == TradingState::Active && position(s) == 2.0
    });
    // kill -9 HERE: the journal as it stands, with no exit Snap of this process.
    crate::kit::journal::copy_journal_files(&dir, &copy);
    handle.shutdown_and_join();

    let saved: Vec<_> = std::fs::read_dir(state.join("refused-books"))
        .expect("the refused book was saved")
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(saved.len(), 1, "{saved:?}");
    assert!(saved[0].starts_with("sim-") && saved[0].ends_with(".json"), "{saved:?}");

    // Boot 3: over the copy.
    let third = second_run(&copy, AccountTier::Live);
    assert_eq!(third.carried, Some(AccountTier::Live), "the base is the Snap the refusal wrote");
    assert!(!said(&third, "TIER MISMATCH"), "the refusal did not repeat: {:?}", third.notes);
    assert_eq!(third.position, 2.0, "the fill taken after the resume is on the book");
    assert_eq!(third.trading_state, TradingState::Active, "and so is the resume");
}

/// A fill on `sim`/`ETHUSDT`: the labelled twin's market.
fn eth_fill(tid: &str, qty: f64, px: f64) -> vike_model::events::Event {
    vike_model::events::Event::Fill(vike_model::events::FillEvent {
        trade_id: vike_model::events::TradeId::new(tid).expect("a non-empty trade id"),
        client_order_id: String::new(),
        venue: "sim".into(),
        symbol: "ETHUSDT".into(),
        side: 1,
        last_qty: qty,
        last_px: px,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "taker".into(),
        ts: 1,
        mark_price: Some(px),
        position_side: vike_model::events::PositionSide::Both,
    })
}

/// The labelled twin engine of `sim` (the Binance `binance#PERP` shape: the same venue as the default
/// account, a route key of its own).
fn twin_engine(
    tier: AccountTier,
) -> vike_exec::ExecutionEngine<vike_exec::testing::RecordingClient> {
    let mut e = crate::kit::engines::engine_on(
        "sim",
        "ETHUSDT",
        vike_exec::testing::RecordingClient::default(),
    );
    e.route_key = "sim#PERP".to_string();
    e.set_tier(tier);
    e
}

/// What the published block of `route_key` held: `(position, trading state)`; `None` before the
/// core's first publish (the cell holds a placeholder with no venue block until then).
fn block_of(s: &CoreSnapshot, route_key: &str) -> Option<(f64, TradingState)> {
    let block = s.portfolio.venues.iter().find(|v| v.route_key == route_key)?;
    Some((block.positions.iter().map(|p| p.size).sum(), block.trading_state))
}

/// **THE TWIN SHAPE: two engines of one venue, only the twin changes tier.** The default account
/// (`sim`, `demo` both runs) gets its book back and keeps trading; the labelled twin (`sim#PERP`,
/// `demo` then `live`: one route key at both tiers, decision 0184) restores nothing of the demo
/// twin's book and is halted. Neither refusal touches the other account.
#[test]
fn a_twin_that_flips_tier_is_refused_and_the_default_account_beside_it_is_not() {
    let dir = Scratch::reserved("snapshot-tier-twin");
    let tmp = Scratch::created("snapshot-tier-twin-tmp");
    let engines =
        |twin: AccountTier| (engine_at(Some(AccountTier::Demo)), vec![(500.0, twin_engine(twin))]);

    let (primary, extras) = engines(AccountTier::Demo);
    let handle = spawn_core_multi(primary, extras, config(&dir));
    handle.event_sender().blocking_send(sim_bare_fill("t-1", 1.0, 100.0)).unwrap();
    handle.event_sender().routed("sim#PERP").blocking_send(eth_fill("t-2", 2.0, 50.0)).unwrap();
    wait_for_snapshot(&handle.snapshot_cell(), 10, "a position on each account", |s| {
        block_of(s, "sim").is_some_and(|b| b.0 == 1.0)
            && block_of(s, "sim#PERP").is_some_and(|b| b.0 == 2.0)
    });
    handle.shutdown_and_join();

    let restore = restore_for_boot(Some(&journal_at(&dir)), &tmp, 42).expect("readable");
    // Two engines of one venue refold like any other base: the tail's order writes carry the route
    // key of their engine (`crates/vike-core/CLAUDE.md`'s boot restore), so the twin shape is a Full
    // restore through a real core. (Its tail is empty here: a clean stop wrote the exit `Snap`.)
    assert_eq!(restore.outcome(), &RestoreOutcome::Full, "{:?}", restore.notes());
    assert_eq!(restore.restored_engines().len(), 2, "both accounts' snapshots");
    let mut cfg = config(&dir);
    restore.apply_to(&mut cfg);
    let (primary, extras) = engines(AccountTier::Live);
    let handle = spawn_core_multi(primary, extras, cfg);
    handle.tick_sender().quote(sim_quote(100.0, 2)).unwrap(); // a publish
    let cell = handle.snapshot_cell();
    wait_for_snapshot(&cell, 10, "the restore on the ring", |s| {
        s.recent_events.iter().any(|l| l.contains("put back on"))
    });
    let snap = cell.load_full();
    handle.shutdown_and_join();
    assert_eq!(
        block_of(&snap, "sim"),
        Some((1.0, TradingState::Active)),
        "the default account's book"
    );
    assert_eq!(
        block_of(&snap, "sim#PERP"),
        Some((0.0, TradingState::Halted)),
        "the twin's: refused"
    );
    assert!(
        snap.recent_events.iter().any(|l| l.contains("sim#PERP: TIER MISMATCH")),
        "{:?}",
        snap.recent_events
    );
    assert!(
        snap.recent_events.iter().any(|l| l.contains("put back on 1 of 2")),
        "{:?}",
        snap.recent_events
    );
}
