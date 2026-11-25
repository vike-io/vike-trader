//! **A mount's closed trades across a restart, through the real core and the production boot restore**
//! (decision 0271 item 8, `crates/vike-core/src/runtime/closed_trades.rs`'s "A restart"): every `Snapshot` carries the
//! ledger's rows and lots, the fills after it are journaled as fills, and a restart resumes the ledger and books
//! those fills on it, so a restarted mount holds the rows and the open lots it held before, after a clean stop and
//! after a crash alike, and every fill after the restart books exactly as it would have without one.
//!
//! One strategy trades the same plan over one bar series: in one boot, and across restarts over one journal
//! directory (each restart is `restore_for_boot` + `RestoreReport::apply_to`, then the history the datahub hands a
//! mount at its start, as a seed, then the rest of the bars). They must read the same closed trades at every
//! decision after the restart, bar indices included. The position is held in TWO lots across the restart (100 and
//! 102), so a restart that kept only the position would book the first close against one lot at their average.
//!
//! The rows themselves live in the closed-trade ARCHIVE beside the journal: 10,001 trades across a restart read as
//! the whole list from it while the exit `Snapshot` stays the size of one after a single trade, and a boot that died in
//! the middle of an append loses no trade.

use std::collections::VecDeque;
use std::path::Path;
use std::sync::{Arc, Mutex};

use vike_core::replay::{RestoreReport, restore_for_boot};
use vike_core::testkit::mount_of;
use vike_core::{CoreConfig, CoreHandle, JournalConfig, LiveBroker, spawn_core};
use vike_execution::testing::{accept_submit, engine_on};
use vike_execution::{BarSeed, BarUpdate, ExecutionClient};
use vike_journal::JournalRecord;
use vike_marketdata::Bar;
use vike_model::events::{Event, FillEvent, OrderFilled, PositionSide, TradeId};
use vike_model::{OrderBroker, OrderPlacement, OrderRequest, Strategy, TradeClosedRow};

use crate::kit::journal::{journal_at, records, rewrite_journal};
use crate::scratch::Scratch;

const H: i64 = 3_600_000;
/// Every fill's commission.
const FEE: f64 = 0.5;
/// The core clock of every boot.
const NOW: i64 = 1_700_000_000_000;

/// `(bar ts, side, qty, limit price)`: what the strategy submits at the close of the bar opened at `ts`, filled
/// at once at its price by the venue.
type Plan = Vec<(i64, i32, f64, f64)>;

/// What the strategy read at each decision, `(bar ts, closed trades)`.
type Reads = Arc<Mutex<Vec<(i64, Vec<TradeClosedRow>)>>>;

struct Trader {
    plan: Plan,
    reads: Reads,
}

impl Strategy<LiveBroker> for Trader {
    fn on_bar(&mut self, b: &mut LiveBroker, bar: &Bar) {
        let rows = b.closed_trades().map(<[TradeClosedRow]>::to_vec).unwrap_or_default();
        self.reads.lock().unwrap().push((bar.timestamp, rows));
        for &(_, side, qty, px) in self.plan.iter().filter(|p| p.0 == bar.timestamp) {
            b.place("", "", side, qty, OrderPlacement::Limit { price: px }, false);
        }
    }
}

/// Two entries before the restart, and after it a close across both lots, a flip, and the flip's close.
fn plan() -> Plan {
    vec![
        (0, 1, 2.0, 100.0),
        (H, 1, 1.0, 102.0),
        (3 * H, -1, 2.5, 104.0),
        (4 * H, -1, 1.5, 103.0),
        (5 * H, 1, 1.0, 101.0),
    ]
}

fn bar(ts: i64) -> Bar {
    Bar::ohlcv(ts, 100.0, 100.0, 100.0, 100.0, 1.0)
}

/// A venue that fills every order at once, whole, at its limit price with [`FEE`], under a trade id made of the
/// order's own client id (unique across the restart, which resumes the order client id session; a counter of the client's
/// own would start again and the restored fill dedup would drop the fill).
#[derive(Default)]
struct Fills(VecDeque<Event>);

impl ExecutionClient for Fills {
    fn submit(&mut self, r: &OrderRequest) {
        let fill = FillEvent {
            trade_id: TradeId::prefixed("t-", format_args!("{}", r.order_client_id)),
            order_client_id: r.order_client_id.clone(),
            venue: r.venue.as_str().into(),
            symbol: r.symbol.as_str().into(),
            side: r.side,
            last_qty: r.qty,
            last_price: r.price.unwrap_or(f64::NAN),
            commission: FEE,
            commission_asset: String::new().into(),
            liquidity_side: "taker".to_string().into(),
            timestamp: r.timestamp,
            mark_price: None,
            position_side: PositionSide::Both,
        };
        accept_submit(&mut self.0, r);
        self.0.push_back(Event::Fill(fill.clone()));
        let order_client_id = r.order_client_id.clone();
        self.0.push_back(Event::OrderFilled(OrderFilled {
            order_client_id,
            fill,
            timestamp: r.timestamp,
        }));
    }
    fn cancel(&mut self, _order_client_id: &str) {}
    fn poll_events(&mut self) -> Option<Event> {
        self.0.pop_front()
    }
}

/// One boot of the mount over the journal at `journal`, handed what `restore` restores.
fn boot(journal: &Path, reads: &Reads, restore: Option<RestoreReport>) -> CoreHandle {
    let trader = Trader { plan: plan(), reads: Arc::clone(reads) };
    boot_with(Some(journal_at(journal, 1 << 20, 1_000)), Box::new(trader), restore)
}

/// One boot of `strategy`'s mount over `journal` (none: no journal at all), handed what `restore` restores.
fn boot_with(
    journal: Option<JournalConfig>,
    strategy: Box<dyn Strategy<LiveBroker> + Send>,
    restore: Option<RestoreReport>,
) -> CoreHandle {
    let client = Fills::default();
    let mut config = CoreConfig {
        seed_cash: 1_000_000.0,
        clock: Box::new(|| NOW),
        order_client_id_session: Some(("cafef00d".into(), 0)),
        strategy: Some(mount_of("sim", "BTCUSDT", "1h", strategy)),
        journal,
        ..CoreConfig::default()
    };
    if let Some(restore) = restore {
        restore.apply_to(&mut config);
    }
    spawn_core(engine_on("sim", "BTCUSDT", client), config)
}

/// The production restart's first half over the journal at `journal`.
fn restore(journal: &Path) -> RestoreReport {
    restore_of(&journal_at(journal, 1 << 20, 1_000))
}

/// The production restart's first half over `journal`.
fn restore_of(journal: &JournalConfig) -> RestoreReport {
    let scratch = Scratch::created("closed-trades-restore");
    restore_for_boot(Some(journal), &scratch, NOW, &[]).expect("the journal reads")
}

/// The history the datahub hands a mount at its start, bars `0..n`, as the core's bar lane takes it.
fn seed(handle: &CoreHandle, n: i64) {
    let seed = BarSeed {
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        interval: "1h".into(),
        bars: (0..n).map(|k| bar(k * H)).collect(),
    };
    handle.bar_sender().seed(seed).unwrap();
}

/// What one boot over all seven bars reads, with no restart.
fn reads_once() -> Vec<(i64, Vec<TradeClosedRow>)> {
    let once = Scratch::reserved("closed-trades-once");
    let reads = Reads::default();
    let handle = boot(&once.join("journal"), &reads, None);
    (0..7).for_each(|k| close(&handle, k * H));
    handle.shutdown_and_join();
    reads.lock().unwrap().clone()
}

/// The exit `Snapshot`'s closed-trade ledgers of the journal at `journal`.
fn exit_ledgers(journal: &Path) -> Vec<vike_journal::JournalSnapshotClosedTrades> {
    let recs = records(journal);
    let exit = recs.iter().rev().find_map(|r| match r {
        JournalRecord::Snapshot { closed_trades, .. } => Some(closed_trades.clone()),
        _ => None,
    });
    exit.expect("an exit Snapshot")
}

fn close(handle: &CoreHandle, ts: i64) {
    let update = BarUpdate {
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        interval: "1h".into(),
        bar: bar(ts),
        sides: None,
    };
    handle.bar_sender().close(update).unwrap();
}

/// What one boot after the restart reads, over the journal at `journal`: the history of bars `0..3`, then the
/// four bars after it.
fn reads_after_restart(journal: &Path) -> Vec<(i64, Vec<TradeClosedRow>)> {
    let reads = Reads::default();
    let handle = boot(journal, &reads, Some(restore(journal)));
    seed(&handle, 3);
    (3..7).for_each(|k| close(&handle, k * H));
    handle.shutdown_and_join();
    reads.lock().unwrap().clone()
}

/// The reads after a restart, against the one-boot run's.
fn assert_reads_as_once(after: &[(i64, Vec<TradeClosedRow>)], once: &[(i64, Vec<TradeClosedRow>)]) {
    assert_eq!(after.len(), 4, "the boot after the restart decided at four bars: {after:?}");
    assert_eq!(after, &once[3..], "every decision after the restart reads what one boot reads");
    let rows = &after[1].1;
    assert_eq!(rows.len(), 2, "the close of 2.5 reached both lots: {rows:?}");
    assert_eq!((rows[0].entry_price, rows[0].qty), (100.0, 2.0));
    assert_eq!((rows[1].entry_price, rows[1].qty), (102.0, 0.5));
    assert_eq!(after[3].1.len(), 4, "the flip's short closed too, against its own lot at 103");
    assert_eq!(after[3].1[3].entry_price, 103.0);
}

/// **A clean restart restores the rows and the open lots exactly**: the exit `Snapshot` carries the ledger (two open
/// lots, no row yet, the bar clock), every decision after the restart reads the closed trades the one-boot run
/// reads, and the first close after it books one row per lot, at 100 and at 102.
#[test]
fn a_restarted_mount_reads_the_closed_trades_a_run_without_the_restart_reads() {
    let once = reads_once();
    let twice = Scratch::reserved("closed-trades-twice");
    let journal = twice.join("journal");
    let handle = boot(&journal, &Reads::default(), None);
    (0..3).for_each(|k| close(&handle, k * H));
    handle.shutdown_and_join();

    let exit = exit_ledgers(&journal);
    assert_eq!(exit.len(), 1, "one mount's ledger: {exit:?}");
    let lots: Vec<(f64, f64, usize)> =
        exit[0].lots.iter().map(|l| (l.price, l.qty, l.entry_bar)).collect();
    assert_eq!(lots, vec![(100.0, 2.0, 0), (102.0, 1.0, 1)], "both lots, exactly");
    assert!(exit[0].rows.is_empty());
    assert_eq!(
        exit[0].bar_clock.map(|c| (c.timestamp, c.index)),
        Some((2 * H, 2)),
        "the bar clock"
    );

    assert_reads_as_once(&reads_after_restart(&journal), &once);
}

/// **A crash's tail is booked from the journaled fills**: boot 1 buys at bar 0 and stops (its exit `Snapshot` holds one
/// lot); boot 2 buys again at bar 1 and then CRASHES (its exit `Snapshot` is cut off the journal, so the fill is in the
/// tail only, as a fill: there is no record of the booking). Boot 3 resumes the ledger from the `Snapshot` and books the
/// tail's fill on it at the bar its clock counts, and reads what one boot reads.
#[test]
fn a_crash_tail_fill_is_booked_from_the_journaled_fill() {
    let once = reads_once();
    let dir = Scratch::reserved("closed-trades-crash");
    let journal = dir.join("journal");
    let handle = boot(&journal, &Reads::default(), None);
    close(&handle, 0);
    handle.shutdown_and_join();
    let handle = boot(&journal, &Reads::default(), Some(restore(&journal)));
    seed(&handle, 1);
    (1..3).for_each(|k| close(&handle, k * H));
    handle.shutdown_and_join();

    // The crash: every record but the last `Snapshot` (boot 2's exit), in a journal of its own.
    let mut recs = records(&journal);
    let exit = recs
        .iter()
        .rposition(|r| matches!(r, JournalRecord::Snapshot { .. }))
        .expect("an exit Snapshot");
    recs.remove(exit);
    let fills_after_base = {
        let base = recs
            .iter()
            .rposition(|r| matches!(r, JournalRecord::Snapshot { .. }))
            .expect("a base Snapshot");
        let fill = |r: &&JournalRecord| matches!(r, JournalRecord::Command { message: msg, .. } if matches!(msg.venue_event(), Some(Event::Fill(_))));
        recs[base + 1..].iter().filter(fill).count()
    };
    assert_eq!(fills_after_base, 1, "boot 2's fill is in the tail, as a fill and nothing else");
    let crashed = Scratch::reserved("closed-trades-crashed");
    let crashed_journal = crashed.join("journal");
    rewrite_journal(&crashed_journal, recs);

    assert_reads_as_once(&reads_after_restart(&crashed_journal), &once);
}

// ---------------------------------------------------------------------------------------------------------------
// The closed-trade ARCHIVE beside the journal (`crates/vike-journal/src/closed_archive.rs`): the whole list across a
// restart, a crash mid-append, and a `Snapshot` whose size does not grow with the trades.
// ---------------------------------------------------------------------------------------------------------------

/// Closes one round trip at every bar opened before `trade_before` (a buy at 100 and a sell at 101, each filled at
/// once by [`Fills`]: one closed row a bar), and keeps the whole closed list it reads at the bar opened at `read_at`.
struct RoundTrips {
    trade_before: i64,
    read_at: i64,
    read: Arc<Mutex<Vec<TradeClosedRow>>>,
}

impl Strategy<LiveBroker> for RoundTrips {
    fn on_bar(&mut self, b: &mut LiveBroker, bar: &Bar) {
        if bar.timestamp == self.read_at {
            *self.read.lock().unwrap() =
                b.closed_trades().map(<[TradeClosedRow]>::to_vec).unwrap_or_default();
        }
        if bar.timestamp < self.trade_before {
            b.place("", "", 1, 1.0, OrderPlacement::Limit { price: 100.0 }, false);
            b.place("", "", -1, 1.0, OrderPlacement::Limit { price: 101.0 }, false);
        }
    }
}

/// One boot of [`RoundTrips`] over `journal` (none: no journal), after `restore`: the history of bars `0..seeded`
/// (none on a first boot), then the closes of bars `from..to`. The list it read at `read_at`.
fn round_trips(
    journal: Option<JournalConfig>,
    restore: Option<RestoreReport>,
    (seeded, from, to): (i64, i64, i64),
    (trade_before, read_at): (i64, i64),
) -> Vec<TradeClosedRow> {
    let read = Arc::new(Mutex::new(Vec::new()));
    let strategy = RoundTrips {
        trade_before: trade_before * H,
        read_at: read_at * H,
        read: Arc::clone(&read),
    };
    let handle = boot_with(journal, Box::new(strategy), restore);
    if seeded > 0 {
        seed(&handle, seeded);
    }
    (from..to).for_each(|k| close(&handle, k * H));
    handle.shutdown_and_join();
    read.lock().unwrap().clone()
}

/// The exit `Snapshot`'s one ledger of `journal`, and its size as the `Snapshot` writes it.
fn exit_ledger(journal: &Path) -> (vike_journal::JournalSnapshotClosedTrades, usize) {
    let mut exit = exit_ledgers(journal);
    assert_eq!(exit.len(), 1, "one mount's ledger: {exit:?}");
    let one = exit.remove(0);
    let size = serde_json::to_vec(&one).unwrap().len();
    (one, size)
}

/// The journal the archive tests write at `dir`: small segments, a cadence `Snapshot` every 1,000 records.
fn small_journal(dir: &Path) -> Option<JournalConfig> {
    Some(journal_at(dir, 1 << 20, 1_000))
}

/// **10,001 closed trades across a restart**: the exit `Snapshot` says the archive holds all of them and carries no row,
/// at the size a `Snapshot` after ONE trade has (its numbers' digits aside), and the boot after the restart reads the
/// whole list, two more trades on it, exactly as a run without the restart reads it.
///
/// The long run writes no cadence `Snapshot` and takes production's 64 MiB segments: the ENGINE's half of a `Snapshot` (its
/// order registry and fill dedup, which keep every one of the 20,002 orders under this test's frozen clock) is
/// about as large as the ledger's half once was, and a cadence `Snapshot` of it every 1,000 records would make the run
/// quadratic. The ledger's half is what this test measures.
#[test]
fn a_restart_after_ten_thousand_trades_reads_the_whole_list_from_the_archive() {
    const N: i64 = 10_001;
    let once = round_trips(None, None, (0, 0, N + 3), (N + 2, N + 2));
    assert_eq!(once.len(), (N + 2) as usize);

    let small_dir = Scratch::reserved("closed-archive-one");
    let small = small_dir.join("journal");
    round_trips(small_journal(&small), None, (0, 0, 2), (1, 1));
    let (one, one_size) = exit_ledger(&small);
    assert_eq!((one.archived, one.rows.len(), one.lots.len()), (1, 0, 0));

    let dir = Scratch::reserved("closed-archive-restart");
    let journal = dir.join("journal");
    let long = || journal_at(&journal, 64 << 20, u64::MAX);
    round_trips(Some(long()), None, (0, 0, N), (N, N));
    let snap_bytes = records(&journal)
        .iter()
        .rev()
        .find(|r| matches!(r, JournalRecord::Snapshot { .. }))
        .map(|r| serde_json::to_vec(r).unwrap().len());
    let (exit, size) = exit_ledger(&journal);
    println!(
        "the exit Snapshot after {N} trades: its ledger {size} B (after one trade: {one_size} B), {snap_bytes:?} B in all"
    );
    assert_eq!(
        (exit.archived, exit.rows.len(), exit.lots.len()),
        (N as u64, 0, 0),
        "the archive holds every row; the Snapshot carries none"
    );
    assert!(
        size <= one_size + 16,
        "a Snapshot after {N} trades ({size} B) is a Snapshot after one ({one_size} B)"
    );

    let after = round_trips(Some(long()), Some(restore_of(&long())), (N, N, N + 3), (N + 2, N + 2));
    assert_eq!(after.len(), (N + 2) as usize, "the whole list, and two more");
    assert_eq!(after, once, "the list a run without the restart reads, bar indices included");
}

/// **A crash mid-append loses nothing**: boot 1 closes 20 trades and stops (its exit `Snapshot`: archived through 20);
/// boot 2 closes 15 more and dies while its archive thread appends the 8th of them (the archive cut in the middle
/// of trade #27, and boot 2's exit `Snapshot` cut off the journal, so its fills are in the tail only). Boot 3 reads the
/// archive's 20, books the tail's 15 again and writes them again, closes 5 more, and reads the list a run without a
/// restart reads; the archive then holds all 40, the torn frame and the copies written twice dropped.
#[test]
fn a_crash_mid_append_loses_no_closed_trade() {
    let once = round_trips(None, None, (0, 0, 41), (40, 40));
    assert_eq!(once.len(), 40);

    let dir = Scratch::reserved("closed-archive-crash");
    let journal = dir.join("journal");
    round_trips(small_journal(&journal), None, (0, 0, 20), (20, 20));
    round_trips(small_journal(&journal), Some(restore(&journal)), (20, 20, 35), (35, 35));

    // The crash: boot 2's exit `Snapshot` cut off the journal, and its archive cut in the middle of trade #27.
    let mut recs = records(&journal);
    let exit = recs
        .iter()
        .rposition(|r| matches!(r, JournalRecord::Snapshot { .. }))
        .expect("an exit Snapshot");
    recs.remove(exit);
    let crashed = Scratch::reserved("closed-archive-crashed");
    let crashed_journal = crashed.join("journal");
    rewrite_journal(&crashed_journal, recs);
    let bytes = std::fs::read(journal.join(vike_journal::ARCHIVE_FILE)).unwrap();
    let cut = torn_at(&bytes, 27);
    std::fs::write(crashed_journal.join(vike_journal::ARCHIVE_FILE), &bytes[..cut]).unwrap();
    let base = exit_ledger(&crashed_journal).0;
    assert_eq!((base.archived, base.rows.len()), (20, 0), "the base Snapshot: archived through 20");

    let after = round_trips(
        small_journal(&crashed_journal),
        Some(restore(&crashed_journal)),
        (35, 35, 41),
        (40, 40),
    );
    assert_eq!(after, once, "every trade, the 15 of the crashed boot included");

    let archive = vike_journal::read_closed_archive(&crashed_journal).unwrap();
    assert_eq!(archive.trades.len(), 1);
    let by_n = archive.trades.values().next().unwrap();
    assert_eq!(by_n.keys().copied().collect::<Vec<_>>(), (0..40).collect::<Vec<u64>>());
    let rows: Vec<_> = by_n.values().cloned().collect();
    let want: Vec<_> = once.iter().map(|r| (r.entry_timestamp, r.exit_timestamp, r.net)).collect();
    assert_eq!(
        rows.iter().map(|r| (r.entry_timestamp, r.exit_timestamp, r.net)).collect::<Vec<_>>(),
        want
    );
    assert_eq!(
        (archive.torn, archive.duplicates),
        (1, 7),
        "the torn #27 and boot 2's #20..#26, dropped"
    );
}

/// The offset in the middle of archive frame number `n` (the journal's frame: length, check, payload).
fn torn_at(bytes: &[u8], n: u64) -> usize {
    let mut at = 0;
    while at + 8 <= bytes.len() {
        let len = u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
        let t: vike_journal::TradeClosedArchiveEntry =
            serde_json::from_slice(&bytes[at + 8..at + 8 + len]).unwrap();
        if t.n == n {
            return at + 8 + len / 2;
        }
        at += 8 + len;
    }
    panic!("no frame #{n} in the archive")
}
