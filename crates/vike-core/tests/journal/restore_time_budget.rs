//! **The boot restore of a large journal stays linear in it** (journal retention, point 4): `restore_for_boot`, end to
//! end, on a journal eight times larger takes at most [`MAX_RATIO`] times as long, and the larger one stays under
//! [`CEILING`]. A linear restore takes about 8 times as long and a quadratic one 64 times, so the ratio catches an
//! O(n²) on a shared box where an absolute bound alone would not: `crates/vike-core/src/replay/owed.rs`'s
//! `mounts_by_code` compared every strategy order of the WHOLE journal with every other (#3266), and the CI box's boot on a
//! 2.1 GB journal (4 M records, 833,260 orders of one mount) spun in it for about an hour before #3491.
//!
//! Why eight and not four, and why orders the gate refuses: in a debug build the restore spends about 35 µs on each
//! record and that pairwise check about 20 ns on each pair of orders, so the quadratic term outweighs the linear one
//! only past some thousands of orders at a few records each. A journal of acknowledged, cancelled orders alone (11 records
//! an order) with a step of 4 let the check through (measured 2026-11-24 on a the CI box lane, #3491's fix reverted: 5.0,
//! under a bound of 6). With [`OVER_CEILING`]'s refused orders (5 records an order) and a step of 8, the same lanes
//! measured 7.2 with the fix (3,096 and 24,750 orders, 0.66 s and 4.7 s) and 20.6 without it (14.5 s).
//!
//! The journal is a real core's, through the real journal writer: three mounts re-quote on every quote tick on two
//! accounts of one venue (`sim`, `sim#ALT`), a cancel and a new order three ticks in four and a re-price on the
//! fourth, plus two orders the gate refuses, under a v2 order client id session, against a venue that acknowledges,
//! re-prices, cancels and fills every [`FILL_EVERY`]-th order, each fold with its execution reports. Cadence
//! `Snapshot`s fall at the same fractions of both journals, so their tails and the late-retry memory their
//! `Snapshot`s carry scale with them too (what a restore that reads only the latest `Snapshot` and its tail still
//! reads), and the run ends in a crash: the exit `Snapshot` is cut, so the restore refolds a tail and reads what the
//! execution stream is owed. Each journal is restored [`ROUNDS`] times from a fresh copy (a full restore prunes the
//! segments its base supersedes), the two sizes interleaved and the fastest of each kept, so a busy moment of the box
//! does not land on one size only.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{Duration, Instant};

use vike_core::replay::{RestoreOutcome, restore_for_boot};
use vike_core::{CoreConfig, LiveBroker, Mount, spawn_core_multi};
use vike_execution::testing::{accept_submit, canceled};
use vike_execution::{Account, BalanceMode, Engine, ExecutionClient, MarkSource, RiskGate};
use vike_journal::{Journal, JournalRecord};
use vike_marketdata::TickQuote;
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::events::{Event, FillEvent, OrderModified, PositionSide, TradeId};
use vike_model::orders::client_id::OrderClientIdStrategyCode;
use vike_model::{MS_PER_DAY, OrderRequest, RiskLimits, Strategy};

use crate::kit::events::sim_quote;
use crate::kit::journal::{copy_journal_files, journal_at};
use crate::scratch::Scratch;

/// Quote ticks of the smaller journal; the larger one has [`SIZE_STEP`] times as many.
const TICKS: u64 = 375;
const SIZE_STEP: u64 = 8;
/// The venue fills every this-many-th order it is sent.
const FILL_EVERY: u64 = 5;
/// The tags of the orders a mount asks for on every tick at 1,000 units, 100,000 of notional, ten times the engine's
/// ceiling: each is minted, refused before it leaves and journaled as its three records (`StrategySubmit`,
/// `MintedSubmit`, the denied `ExecutionReport`). They make the orders many and their records few (about five records
/// an order, as the CI box's journal had), so the pairwise term shows at a size a test affords.
const OVER_CEILING: [&str; 2] = ["over-a", "over-b"];
/// A cadence `Snapshot` every this many records per quote tick. A tick journals about 44 records (three mounts' orders,
/// the venue's answers and their reports), so each journal takes six and a tail of about 4%; with repeated denials
/// counted in one record (journal format 35) a tick journals fewer and each takes fewer, with a longer tail. The tail
/// is kept short because its refold is the dearest linear work of a restore.
const CADENCE_PER_TICK: u64 = 7;
/// The core's clock moves this much on every read, so an ended order leaves a `Snapshot`'s registry within a minute
/// of messages, as it does live.
const STEP_MS: i64 = 1_000;
/// A `Snapshot` must fit a segment, and the late-retry memory it carries (every id that ended within a day, up to
/// 65,536) outgrows the 1 MiB the small suites take.
const SEGMENT_BYTES: u64 = 8 << 20;
/// Day 20,000 of the epoch: the first record.
const T0: i64 = 20_000 * MS_PER_DAY;
const SESSION: &str = "W00000000abcd";
const ALT: &str = "sim#ALT";
const ROUNDS: usize = 2;
/// Linear allows 8, quadratic gives 64.
const MAX_RATIO: f64 = 12.0;
/// About twelve times the larger restore on a busy lane.
const CEILING: Duration = Duration::from_secs(60);

/// The venue of the session: it acknowledges every order, re-prices one in place, cancels one at once, and fills
/// every [`FILL_EVERY`]-th order it is sent, whole, at its limit.
#[derive(Default)]
struct QuotingVenue {
    events: VecDeque<Event>,
    sent: u64,
}

impl ExecutionClient for QuotingVenue {
    fn submit(&mut self, request: &OrderRequest) {
        accept_submit(&mut self.events, request);
        self.sent += 1;
        if self.sent.is_multiple_of(FILL_EVERY) {
            self.events.push_back(Event::Fill(FillEvent {
                trade_id: TradeId::new(format!("{}-fill", request.order_client_id))
                    .expect("non-empty"),
                order_client_id: request.order_client_id.clone(),
                venue: "sim".into(),
                symbol: "BTCUSDT".into(),
                side: request.side,
                last_qty: request.qty,
                last_price: request.price.unwrap_or(100.0),
                commission: 0.01,
                commission_asset: "USDT".into(),
                liquidity_side: "maker".into(),
                timestamp: request.timestamp,
                mark_price: None,
                position_side: PositionSide::Both,
            }));
        }
    }
    fn cancel(&mut self, order_client_id: &str) {
        self.events.push_back(canceled(order_client_id, "requote", 0));
    }
    fn modify(&mut self, order: &OrderRequest, new_qty: Option<f64>, new_price: Option<f64>) {
        self.events.push_back(Event::OrderModified(OrderModified {
            order_client_id: order.order_client_id.clone(),
            venue_order_id: None,
            new_qty,
            new_price,
            new_trigger_price: None,
            timestamp: 0,
        }));
    }
    fn poll_events(&mut self) -> Option<Event> {
        self.events.pop_front()
    }
}

/// A market maker's quote loop: on every quote tick it cancels the order it rests and rests a new one on the other
/// side, and every fourth tick it re-prices the one it rests instead. On every tick it also asks for
/// [`OVER_CEILING`]'s orders, which the engine's per-order notional ceiling refuses.
#[derive(Default)]
struct Requoter {
    ticks: u64,
    resting_b: bool,
}

impl Strategy<LiveBroker> for Requoter {
    fn on_quote_tick(&mut self, broker: &mut LiveBroker, quote: &TickQuote) {
        self.ticks += 1;
        for tag in OVER_CEILING {
            broker.submit_limit_tagged(tag, 1, 1_000.0, quote.bid);
        }
        let resting = if self.resting_b { "b" } else { "a" };
        if self.ticks.is_multiple_of(4) {
            broker.modify(resting, None, Some(quote.bid - 0.5));
            return;
        }
        broker.cancel_tagged(resting);
        self.resting_b = !self.resting_b;
        let (tag, side) = if self.resting_b { ("b", 1) } else { ("a", -1) };
        broker.submit_limit_tagged(tag, side, 1.0, quote.bid);
    }
}

/// A sim/BTCUSDT engine on the default account or `route_key`'s, holding a mark of 100.
fn engine(route_key: Option<&str>) -> Engine<QuotingVenue> {
    let limits = RiskLimits { max_notional_per_order: Some(10_000.0), ..RiskLimits::new() };
    let mut engine = Engine::new(
        Account::new(1.0, "sim", None, BalanceMode::Delta),
        RiskGate::new(limits),
        QuotingVenue::default(),
        "sim",
        "BTCUSDT",
    );
    engine.account.set_mark_from("sim", "BTCUSDT", 100.0, MarkSource::VenueMark, 0);
    if let Some(key) = route_key {
        engine.route_key = key.to_string();
    }
    engine
}

/// A [`Requoter`] mounted on sim/BTCUSDT's `account`, minting under strategy code `code`.
fn maker(code: &str, account: AccountLabel, id: &str) -> Mount {
    Mount {
        order_client_id_code: OrderClientIdStrategyCode::parse(code),
        account: Some(account),
        symbols: Vec::new(),
        controller_id: Some(id.into()),
        underlying_symbol: None,
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        interval: "1m".into(),
        strategy: Box::new(Requoter::default()),
    }
}

/// The session of `ticks` quote ticks, journaled at `dir` and stopped cleanly. Returns the core's clock at the end.
fn write_session(dir: &Path, ticks: u64) -> i64 {
    let now = Arc::new(AtomicI64::new(T0));
    let clock = Arc::clone(&now);
    let alt = AccountLabel::parse("ALT").expect("a label");
    let config = CoreConfig {
        order_client_id_session: Some((SESSION.into(), 0)),
        strategy: Some(maker("0001", AccountLabel::Default, "maker-a")),
        extra_mounts: vec![
            maker("0002", AccountLabel::Default, "maker-b"),
            maker("0003", alt, "maker-c"),
        ],
        clock: Box::new(move || clock.fetch_add(STEP_MS, Ordering::Relaxed)),
        seed_cash: 1_000_000.0,
        journal: Some(journal_at(dir, SEGMENT_BYTES, ticks * CADENCE_PER_TICK)),
        ..CoreConfig::default()
    };
    let handle = spawn_core_multi(engine(None), vec![(1_000_000.0, engine(Some(ALT)))], config);
    let quotes = handle.tick_sender();
    for i in 0..ticks {
        let px = 100.0 + (i % 5) as f64 * 0.1;
        quotes.quote(sim_quote(px, now.load(Ordering::Relaxed))).expect("the core runs");
    }
    handle.shutdown_and_join();
    now.load(Ordering::Relaxed)
}

/// The journal at `src` as a crash just before its exit `Snapshot` leaves it, at `dst`: the segments copied (not
/// their durable copies, which would mend the cut) and the last frame of the last one, the exit `Snapshot`, zeroed. A
/// kill mid-frame leaves no length, so the read ends clean there.
fn crash_copy(src: &Path, dst: &Path) {
    copy_journal_files(src, dst);
    let mut segments: Vec<PathBuf> = std::fs::read_dir(dst)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "vjl"))
        .collect();
    segments.sort();
    let last = segments.pop().expect("a segment");
    let mut bytes = std::fs::read(&last).unwrap();
    // `[length][check][payload]` frames after the 16-byte header, until a zero length
    let (mut at, mut start) = (16usize, 16usize);
    while let Some(len) = bytes.get(at..at + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap()))
        && len > 0
    {
        start = at;
        at += 8 + len as usize;
    }
    bytes[start..at].fill(0);
    std::fs::write(&last, bytes).unwrap();
}

/// The counts the test says and the shape it checks before it believes a timing: the crash copy at `dir` ends in a
/// tail (the exit `Snapshot` is gone) after at least two cadence `Snapshot`s, and that tail holds a fiftieth of the
/// journal and every kind of record the session writes.
fn shape(dir: &Path) -> String {
    let records = Journal::read_all(dir).expect("the crash copy reads");
    let is_snapshot = |r: &JournalRecord| matches!(r, JournalRecord::Snapshot { .. });
    let snapshots = records.iter().filter(|r| is_snapshot(r)).count();
    let base = records.iter().rposition(is_snapshot).expect("a cadence Snapshot");
    let tail = &records[base + 1..];
    let has = |pick: fn(&JournalRecord) -> bool| tail.iter().any(pick);
    let event = |pick: fn(&Event) -> bool| {
        tail.iter().any(|r| {
            matches!(r, JournalRecord::Command { message, .. } if message.venue_event().is_some_and(pick))
        })
    };
    let minted = records.iter().filter(|r| matches!(r, JournalRecord::MintedSubmit { .. })).count();
    let said = format!(
        "{} records, {snapshots} Snapshots, a tail of {}, {minted} orders minted",
        records.len(),
        tail.len()
    );
    assert!(has(|r| matches!(r, JournalRecord::StrategySubmit { .. })), "no submit: {said}");
    assert!(has(|r| matches!(r, JournalRecord::MintedSubmit { .. })), "no mint: {said}");
    assert!(has(|r| matches!(r, JournalRecord::ExecutionReport { .. })), "no report: {said}");
    assert!(event(|e| matches!(e, Event::Fill(_))), "the tail fills: {said}");
    assert!(event(|e| matches!(e, Event::OrderModified(_))), "the tail re-prices: {said}");
    assert!(event(|e| matches!(e, Event::OrderCanceled(_))), "the tail cancels: {said}");
    assert!(snapshots >= 2, "two cadence Snapshots: {said}");
    assert!(tail.len() * 50 >= records.len(), "a fiftieth in the tail: {said}");
    said
}

/// How long `restore_for_boot` takes on a crash copy of the journal at `src`, the restart's clock at `now_ms`.
fn timed_restore(src: &Path, now_ms: i64) -> Duration {
    let dir = Scratch::reserved("restore-budget-copy");
    crash_copy(src, &dir);
    let scratch = Scratch::created("restore-budget-scratch");
    let journal = journal_at(&dir, SEGMENT_BYTES, 1_000);
    let started = Instant::now();
    let restored =
        restore_for_boot(Some(&journal), &scratch, now_ms, &[]).expect("the journal reads");
    let took = started.elapsed();
    assert_eq!(restored.outcome(), &RestoreOutcome::Full, "{:?}", restored.notes());
    took
}

#[test]
fn the_boot_restore_of_an_eight_times_larger_journal_takes_at_most_twelve_times_as_long() {
    let small = Scratch::reserved("restore-budget-small");
    let large = Scratch::reserved("restore-budget-large");
    let started = Instant::now();
    let small_end = write_session(&small, TICKS);
    let large_end = write_session(&large, SIZE_STEP * TICKS);
    let written = started.elapsed();
    // the larger journal is the same session for longer, at the same cadence: one read is enough
    let shape_dir = Scratch::reserved("restore-budget-shape");
    crash_copy(&small, &shape_dir);
    let small_shape = shape(&shape_dir);
    let (mut small_best, mut large_best) = (Duration::MAX, Duration::MAX);
    for _ in 0..ROUNDS {
        small_best = small_best.min(timed_restore(&small, small_end + 60_000));
        large_best = large_best.min(timed_restore(&large, large_end + 60_000));
    }
    let ratio = large_best.as_secs_f64() / small_best.as_secs_f64();
    let said = format!(
        "small: {small_shape}, restored in {small_best:?}; large: {SIZE_STEP} times the ticks, restored in \
         {large_best:?}; ratio {ratio:.2}; both written in {written:?}"
    );
    eprintln!("{said}");
    assert!(large_best < CEILING, "over the {CEILING:?} ceiling: {said}");
    assert!(ratio <= MAX_RATIO, "superlinear (linear is 8, quadratic 64): {said}");
}
