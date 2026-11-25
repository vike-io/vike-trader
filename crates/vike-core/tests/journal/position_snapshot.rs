//! **A boot journals every account's open positions, or why it cannot know them** (package 2's opening positions,
//! `vike_journal::JournalRecord::PositionSnapshot`): the broker's start-of-day statement a reader of the execution
//! stream opens an account's window from, so no fill recorded before the stream is ever needed. A paper book is
//! always stated (the core is its venue); a venue account's only when the restore folded the journal and put its
//! journaled book back; every other account, and one no engine runs, says why its positions are unknown.

use std::path::Path;

use vike_core::replay::{RestoreOutcome, RestoreReport, restore_for_boot};
use vike_core::spawn_core_multi;
use vike_execution::testing::RecordingClient;
use vike_execution::{ArmingNotRunningView, Engine, GatewayEvent};
use vike_journal::{JournalPositionSnapshotAccount, JournalRecord, record_sequence};
use vike_model::AccountTier;
use vike_model::EventRingCursor;
use vike_model::accounts::account_keys::{AccountLabel, AccountNamed, AccountRef};
use vike_model::events::{Event, FillEvent, PositionSide, TradeId};

use crate::kit::engines::{journaled_config, sim_engine};
use crate::kit::journal::{copy_journal_files_once, journal_at, records};
use crate::scratch::Scratch;

/// A sim/BTCUSDT engine of `route_key`'s account, told its `tier`.
fn engine(route_key: &str, tier: AccountTier) -> Engine<RecordingClient> {
    let mut engine = sim_engine();
    engine.route_key = route_key.into();
    engine.set_tier(tier);
    engine
}

/// A fill naming no order (it journals no report) of `qty` at `px` on `ring`'s account, as its gateway ring carries
/// it.
fn ringed_fill(ring: &str, trade_id: &str, side: i32, qty: f64, px: f64, at: u64) -> GatewayEvent {
    GatewayEvent {
        event: Event::Fill(FillEvent {
            trade_id: TradeId::new(trade_id).expect("non-empty"),
            order_client_id: String::new(),
            venue: "sim".into(),
            symbol: "BTCUSDT".into(),
            side,
            last_qty: qty,
            last_price: px,
            commission: 0.0,
            commission_asset: "USDT".into(),
            liquidity_side: "taker".into(),
            timestamp: at as i64,
            mark_price: None,
            position_side: PositionSide::Both,
        }),
        route_key: ring.into(),
        cursor: EventRingCursor { gateway_epoch: 1, sequence: at, timestamp_ms: at as i64 },
    }
}

/// The restore a boot over `dir` makes (`tmp` its scratch).
fn restore(dir: &Path, tmp: &Path) -> RestoreReport {
    restore_for_boot(Some(&journal_at(dir, 1 << 20, 1_000)), tmp, 1_000_000, &[]).expect("readable")
}

/// The accounts of the journal's position snapshots, in order.
fn position_snapshots(dir: &Path) -> Vec<Vec<JournalPositionSnapshotAccount>> {
    records(dir)
        .into_iter()
        .filter_map(|r| match r {
            JournalRecord::PositionSnapshot { accounts, .. } => Some(accounts),
            _ => None,
        })
        .collect()
}

/// `account`'s row of `accounts`.
fn row<'a>(
    accounts: &'a [JournalPositionSnapshotAccount],
    account: &str,
) -> &'a JournalPositionSnapshotAccount {
    accounts
        .iter()
        .find(|a| a.account == account)
        .unwrap_or_else(|| panic!("{account}: {accounts:#?}"))
}

/// `(symbol, size, average price)` of each position of `row`.
fn held(row: &JournalPositionSnapshotAccount) -> Vec<(String, f64, f64)> {
    row.positions.iter().map(|p| (p.symbol.clone(), p.size, p.average_price)).collect()
}

/// **A restored boot states the books it put back, and says why it cannot know the rest**: a LIVE account whose
/// journaled book the restore put back is stated, a PAPER one too, a live account added since (no journaled book)
/// is unknown, and so is an account no engine runs. A core no restore applied to journals none.
#[test]
fn a_boot_states_the_books_it_restored_and_says_why_it_cannot_know_the_rest() {
    let dir = Scratch::reserved("position-snapshot-boot");
    let tmp = Scratch::created("position-snapshot-boot-tmp");
    {
        let config = journaled_config(&dir, 1_000_000.0, 1_000);
        let handle = spawn_core_multi(
            engine("sim", AccountTier::Live),
            vec![(1_000_000.0, engine("sim#ALT", AccountTier::Paper))],
            config,
        );
        let events = handle.event_sender();
        events.blocking_send_gateway(ringed_fill("sim", "a1", 1, 1.0, 100.0, 1)).unwrap();
        events.blocking_send_gateway(ringed_fill("sim#ALT", "b1", -1, 2.0, 50.0, 2)).unwrap();
        handle.shutdown_and_join();
    }
    assert!(position_snapshots(&dir).is_empty(), "no restore applied: no snapshot");

    let restored = restore(&dir, &tmp);
    assert_eq!(*restored.outcome(), RestoreOutcome::Full, "{:?}", restored.notes());
    let mut config = journaled_config(&dir, 1_000_000.0, 1_000);
    restored.apply_to(&mut config);
    config.not_running = vec![ArmingNotRunningView {
        account: AccountNamed::Known(AccountRef { venue: "binance", label: AccountLabel::Default }),
        tier: Some(AccountTier::Live),
        reason: "its keys are absent".into(),
        block: None,
        refusal_id: None,
    }];
    let handle = spawn_core_multi(
        engine("sim", AccountTier::Live),
        vec![
            (1_000_000.0, engine("sim#ALT", AccountTier::Paper)),
            (1_000_000.0, engine("sim#NEW", AccountTier::Live)),
        ],
        config,
    );
    handle.shutdown_and_join();

    let snapshots = position_snapshots(&dir);
    let [accounts] = snapshots.as_slice() else { panic!("one boot, one snapshot: {snapshots:#?}") };
    let live = row(accounts, "DEFAULT");
    assert_eq!((live.venue.as_str(), live.unknown.as_deref()), ("sim", None));
    assert_eq!(held(live), vec![("BTCUSDT".to_string(), 1.0, 100.0)], "the restored live book");
    let paper = row(accounts, "ALT");
    assert_eq!(paper.unknown, None);
    assert_eq!(held(paper), vec![("BTCUSDT".to_string(), -2.0, 50.0)], "the restored paper book");
    let added = row(accounts, "NEW");
    assert!(added.positions.is_empty(), "{added:?}");
    assert_eq!(
        added.unknown.as_deref(),
        Some(
            "the journal holds no book of this account that the boot restored, so its venue positions are not known"
        )
    );
    let gone = accounts.iter().find(|a| a.venue == "binance").expect("the account no engine runs");
    assert_eq!(gone.unknown.as_deref(), Some("no engine runs the account (its keys are absent)"));
}

/// **A journal with no earlier book knows only its paper books**: a fresh journal's boot states the paper account
/// (flat: the core is its venue) and calls the live one unknown, in the restore's words, never flat.
#[test]
fn a_fresh_journal_knows_its_paper_books_and_not_its_venue_accounts() {
    let dir = Scratch::reserved("position-snapshot-fresh");
    let tmp = Scratch::created("position-snapshot-fresh-tmp");
    let restored = restore(&dir, &tmp);
    assert!(matches!(restored.outcome(), RestoreOutcome::Fresh { .. }), "{:?}", restored.outcome());
    let mut config = journaled_config(&dir, 1_000_000.0, 1_000);
    restored.apply_to(&mut config);
    let handle = spawn_core_multi(
        engine("sim", AccountTier::Live),
        vec![(1_000_000.0, engine("sim#ALT", AccountTier::Paper))],
        config,
    );
    handle.shutdown_and_join();
    let snapshots = position_snapshots(&dir);
    let [accounts] = snapshots.as_slice() else { panic!("{snapshots:#?}") };
    let live = row(accounts, "DEFAULT");
    assert!(
        live.unknown.as_deref().is_some_and(|w| w.starts_with("the journal holds no earlier book")),
        "{live:?}"
    );
    let paper = row(accounts, "ALT");
    assert_eq!((paper.unknown.as_deref(), paper.positions.len()), (None, 0), "flat, and known");
}

/// **A crash keeps the snapshot honest**: the next boot after a crash cut the journal states the book the restore
/// REFOLDED (every fill the crash left readable, the ones after the last checkpoint included), and journals it
/// after every record the dead process left, so a reader's "reports after the snapshot" are exactly the new
/// process's.
#[test]
fn after_a_crash_the_snapshot_is_the_refolded_book_and_follows_every_record_left() {
    let src = Scratch::reserved("position-snapshot-crash-src");
    let dir = Scratch::reserved("position-snapshot-crash");
    let tmp = Scratch::created("position-snapshot-crash-tmp");
    let config = journaled_config(&src, 1_000_000.0, 2);
    let handle = spawn_core_multi(engine("sim", AccountTier::Live), Vec::new(), config);
    let events = handle.event_sender();
    events.blocking_send_gateway(ringed_fill("sim", "c1", 1, 1.0, 100.0, 1)).unwrap();
    events.blocking_send_gateway(ringed_fill("sim", "c2", 1, 1.0, 110.0, 2)).unwrap();
    events.blocking_send_gateway(ringed_fill("sim", "c3", -1, 0.5, 120.0, 3)).unwrap();
    let third = |recs: &[JournalRecord]| {
        recs.iter().any(|r| {
            matches!(r, JournalRecord::Command { message, .. }
            if matches!(message.venue_event(), Some(Event::Fill(f)) if f.trade_id.as_str() == "c3"))
        })
    };
    copy_journal_files_once(&src, &dir, "the third fill", third);
    handle.shutdown_and_join();

    let left = records(&dir);
    let restored = restore(&dir, &tmp);
    assert!(
        matches!(
            restored.outcome(),
            RestoreOutcome::Full | RestoreOutcome::TornTail { refolded: true, .. }
        ),
        "{:?}",
        restored.outcome()
    );
    let mut config = journaled_config(&dir, 1_000_000.0, 1_000);
    restored.apply_to(&mut config);
    let handle = spawn_core_multi(engine("sim", AccountTier::Live), Vec::new(), config);
    handle.shutdown_and_join();

    let after = records(&dir);
    let (at, accounts) = after
        .iter()
        .find_map(|r| match r {
            JournalRecord::PositionSnapshot { sequence, accounts, .. } => {
                Some((*sequence, accounts))
            }
            _ => None,
        })
        .expect("the boot's snapshot");
    let live = row(accounts, "DEFAULT");
    assert_eq!(live.unknown, None);
    assert_eq!(held(live), vec![("BTCUSDT".to_string(), 1.5, 105.0)], "c1 + c2 - c3, refolded");
    assert!(left.iter().all(|r| record_sequence(r) < at), "after every record the crash left");
}
