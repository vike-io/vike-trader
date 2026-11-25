//! The witnesses of a run's reads (`super`'s module doc): what is kept, what is refused, and what a later check sees.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use vike_data::{HistoryStore, HistoryStoreError, HistoryStoreMemory, TimestampRange};
use vike_hub_protocol::child_pipe;
use vike_marketdata::Bar;

use super::super::wire::{ReadAnswerFrame, StoreRead};
use super::{Before, HASHED_MAX_BYTES, ReadWitnesses, Seen, Witness, still_holds};
use crate::test_support::{DayStamps, stamping, stamping_restamped_by};

const DAY: i64 = 86_400_000;
/// A UTC midnight, where the stored bars open.
const S: i64 = 20_000 * DAY;

/// Two days of binance BTCUSDT hours from [`S`].
fn bars() -> Arc<dyn HistoryStore + Send + Sync> {
    let store = HistoryStoreMemory::new();
    let hours: Vec<Bar> = (0..48)
        .map(|i| Bar::ohlcv(S + i * 3_600_000, 100.0, 101.0, 99.0, 100.0 + i as f64, 1.0))
        .collect();
    store.append_bars("binance", "BTCUSDT", "1h", &hours, None).expect("planted");
    Arc::new(store)
}

fn hours(end: Option<i64>) -> StoreRead {
    StoreRead::LoadBars {
        venue: "binance".to_string(),
        symbol: "BTCUSDT".to_string(),
        interval: "1h".to_string(),
        range: TimestampRange { start: Some(S), end },
    }
}

/// `read` witnessed as the relay witnesses it: asked before, answered through the relay's own `answer`, recorded.
fn witness(witnesses: &ReadWitnesses, store: &(dyn HistoryStore + Send + Sync), read: StoreRead) {
    let before = witnesses.before(store, &read);
    let mut seen = witnesses.seen(&before);
    super::super::relay::answer(&mut seen.over(&mut Vec::new()), store, read.clone())
        .expect("answered");
    witnesses.record(0, read, before, &seen, false);
}

#[test]
fn a_refused_answer_is_seen_and_every_other_frame_is_not() {
    let frames = [
        (true, ReadAnswerFrame::<()>::Failed(HistoryStoreError::Io("no".to_string()))),
        (false, ReadAnswerFrame::<()>::Count(3)),
        (false, ReadAnswerFrame::<()>::Done),
        (false, ReadAnswerFrame::<()>::Rows(())),
    ];
    for (failed, frame) in frames {
        let mut seen = Seen::new(false);
        child_pipe::write_message(&mut seen.over(&mut Vec::new()), &frame).expect("written");
        assert_eq!(seen.failed(), failed, "{frame:?}");
    }
}

/// A bar read is witnessed by its day stamps, any other read by the hash of its answer; both hold until the store
/// moves, and a RESTAMPED day (a re-imported day) is a change, though not a row of it moved.
#[test]
fn a_read_is_witnessed_by_its_day_stamps_or_by_its_answer_and_a_restamp_breaks_it() {
    let restamp = Arc::new(AtomicU64::new(0));
    let store = stamping_restamped_by(bars(), DayStamps::Settled, Arc::clone(&restamp));
    let witnesses = ReadWitnesses::new(S + 10 * DAY);
    witness(&witnesses, store.as_ref(), hours(Some(S + 2 * DAY - 1)));
    witness(&witnesses, store.as_ref(), StoreRead::ListSeries);
    let reads = witnesses.finish().expect("every read witnessed");
    assert!(matches!(&reads[0].witness, Witness::Days(days) if days.len() == 2), "{reads:?}");
    assert!(matches!(reads[1].witness, Witness::Answer(_)), "{reads:?}");
    assert_eq!(still_holds(&reads, store.as_ref()), Ok(()));
    restamp.fetch_add(1, Ordering::Relaxed);
    let moved = still_holds(&reads, store.as_ref()).expect_err("a restamped day is a change");
    assert!(moved.contains("the 1h bars of binance BTCUSDT"), "{moved}");
}

/// The answer a hashed read is witnessed by is the store's: another answer of the same read is a change.
#[test]
fn a_hashed_read_whose_answer_moved_is_a_change() {
    let memory = Arc::new(HistoryStoreMemory::new());
    let store: Arc<dyn HistoryStore + Send + Sync> = memory.clone();
    let witnesses = ReadWitnesses::new(S + 10 * DAY);
    witness(&witnesses, store.as_ref(), StoreRead::ListSeries);
    let reads = witnesses.finish().expect("witnessed");
    assert_eq!(still_holds(&reads, store.as_ref()), Ok(()));
    let one = [Bar::ohlcv(S, 1.0, 1.0, 1.0, 1.0, 1.0)];
    memory.append_bars("binance", "ETHUSDT", "1h", &one, None).expect("a new series");
    assert!(still_holds(&reads, store.as_ref()).is_err(), "the series list grew");
}

/// A run hashes no more than [`HASHED_MAX_BYTES`] of answers: the read that passes it stops being hashed, and the run
/// is not kept (a big depth or chain run pays no hash for a reuse).
#[test]
fn a_run_whose_hashed_answers_pass_the_bound_is_not_kept() {
    let store = bars();
    let witnesses = ReadWitnesses::new(S + 10 * DAY);
    witness(&witnesses, store.as_ref(), StoreRead::ListSeries);
    let under = witnesses.hashed.load(Ordering::Relaxed);
    assert!(under > 0, "a list is hashed");
    // As if the run had hashed all but a few bytes already.
    witnesses.hashed.store(HASHED_MAX_BYTES - 1, Ordering::Relaxed);
    witness(&witnesses, store.as_ref(), StoreRead::ListSeries);
    assert_eq!(
        witnesses.hashed.load(Ordering::Relaxed),
        HASHED_MAX_BYTES - 1,
        "nothing past the bound was hashed"
    );
    let why = witnesses.finish().expect_err("over the bound");
    assert!(why.contains("hashing stopped"), "{why}");
}

/// What keeps a run from being kept for reuse: a window that reaches today, a provisional day, a store that stamps
/// no day, a read the store refused. Each is a reason in words, and the first one wins.
#[test]
fn a_window_reaching_today_a_provisional_day_or_a_refused_read_is_not_kept() {
    let settled = stamping(bars(), DayStamps::Settled);
    // Today is the stored second day: a read through its end reaches it, an open one too.
    let witnesses = ReadWitnesses::new(S + DAY + 5);
    for end in [Some(S + 2 * DAY - 1), None] {
        assert!(matches!(witnesses.before(settled.as_ref(), &hours(end)), Before::Unkept(_)));
    }
    assert!(matches!(
        witnesses.before(settled.as_ref(), &hours(Some(S + DAY - 1))),
        Before::Days(_)
    ));
    let later = ReadWitnesses::new(S + 10 * DAY);
    let provisional = stamping(bars(), DayStamps::LastProvisional);
    assert!(matches!(
        later.before(provisional.as_ref(), &hours(Some(S + 2 * DAY - 1))),
        Before::Unkept(_)
    ));
    let unstamped = stamping(bars(), DayStamps::Unknown);
    assert!(matches!(
        later.before(unstamped.as_ref(), &hours(Some(S + 2 * DAY - 1))),
        Before::Unkept(_)
    ));
    // A read the relay refused in its own words leaves the run unkept, whatever its witness.
    later.record(0, StoreRead::ListSeries, Before::Answer, &Seen::new(true), true);
    witness(&later, settled.as_ref(), hours(Some(S + 2 * DAY - 1)));
    let why = later.finish().expect_err("a refused read is no witness");
    assert!(why.contains("refused the series list"), "{why}");
}
