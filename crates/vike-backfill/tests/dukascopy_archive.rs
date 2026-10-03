//! Offline gate for the archive import lane's STORE half and its one-owner-per-UTC-day rule against
//! the HTTP lane (docs/decisions/0100; `docs/superpowers/specs/2026-09-30-archive-import-lane-design.md`
//! §4 and the store rows of §8). Every test drives a `tempdir` `DataFusionHist` through
//! `vike_backfill::venues::dukascopy`'s two writers — `ArchiveImport::import_day`, handed an
//! already-decoded day the way the datahub hands it one, and `backfill_quotes_then_bars`, handed a
//! fake fetch that honours its bounds — so both lanes run with no network and no file.
//!
//! The property every test ends on is the one the rule exists for: the store dedups by commit key and
//! never by row, so a tick both lanes stored is stored TWICE and inflates `volume` in every bar
//! resampled over it. So the assertions count stored ticks against the tape, and compare stored bars
//! with `vike_model::consolidate_quotes` of the tape bit for bit.

use std::sync::Barrier;

use vike_backfill::CollectError;
use vike_backfill::venues::dukascopy::{
    ArchiveBars, ArchiveDayClass, ArchiveDayRefusal, ArchiveDayResult, ArchiveImport, DayOwners,
    VENUE, archive_quote_commit_key, backfill_quotes_then_bars, provisional_quote_commit_key,
    provisional_resample_key, quote_commit_key, quote_series_facts,
};
use vike_data::{DataFusionHist, HistStore, TsRange};
use vike_model::{MS_PER_DAY, QuoteTick, consolidate_quotes};

const SYMBOL: &str = "EURUSD";
/// 2024-01-15 00:00 UTC — long settled, so every lane takes its canonical keys for it.
const D: i64 = 19_737 * MS_PER_DAY;
/// The last millisecond of [`D`].
const END: i64 = D + MS_PER_DAY - 1;
const MINUTE: i64 = 60_000;
/// `7m` does not divide a day: its chunk is 205 bars, `86_100_000` ms, so some chunks cross midnight.
const SEVEN_MINUTES: i64 = 7 * MINUTE;
const SEVEN_MINUTE_CHUNK: i64 = 205 * SEVEN_MINUTES;

/// Where each UTC day's ticks fall — its first two minutes, an hour in, noon, and its last minute —
/// so a day holds several 1m and 5m bars, and a `7m` chunk crossing midnight holds ticks on both
/// sides of it.
const OFFSETS: [i64; 6] =
    [10_000, 70_000, 3_600_000 + 5_000, 43_200_000, 43_260_000, MS_PER_DAY - 30_000];

/// The one tape both lanes see: six ticks every UTC day, cut to `[from, to]`, prices varying with
/// the instant so every bar is distinct. The import gets a day of it as its decoded file; the HTTP
/// lane's fake fetch gets the same cut of it, as `vike_dukascopy::fetch_quotes_range` would.
fn tape(from: i64, to: i64) -> Vec<QuoteTick> {
    (from.div_euclid(MS_PER_DAY)..=to.div_euclid(MS_PER_DAY))
        .flat_map(|day| OFFSETS.map(|at| day * MS_PER_DAY + at))
        .filter(|ts| (from..=to).contains(ts))
        .map(|ts| {
            let wiggle = (ts.rem_euclid(1_000_003) as f64) * 1e-9;
            QuoteTick {
                ts,
                local_ts: 0,
                bid: 1.1 + wiggle,
                ask: 1.1002 + wiggle,
                bid_size: 1.0 + (ts.rem_euclid(7) as f64),
                ask_size: 2.0,
                symbol: SYMBOL.to_string(),
            }
        })
        .collect()
}

fn open() -> (tempfile::TempDir, DataFusionHist) {
    let dir = tempfile::tempdir().unwrap();
    let hist = DataFusionHist::open(dir.path()).unwrap();
    (dir, hist)
}

fn now() -> i64 {
    vike_model::now_ms()
}

fn intervals(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

/// The stored ticks inside `[from, to]`, by `ts`.
fn stored_ts(hist: &DataFusionHist, from: i64, to: i64) -> Vec<i64> {
    hist.scan_quotes(VENUE, SYMBOL, TsRange::of(from, to)).unwrap().iter().map(|q| q.ts).collect()
}

fn ts_of(tape: &[QuoteTick]) -> Vec<i64> {
    tape.iter().map(|q| q.ts).collect()
}

/// The stored bars of `interval` inside `[from, to]`, each as its stored fields' bit patterns.
fn stored_bars(hist: &DataFusionHist, interval: &str, from: i64, to: i64) -> Vec<(i64, [u64; 5])> {
    let bars = hist.load_bars(VENUE, SYMBOL, interval, TsRange::of(from, to)).unwrap();
    bars.iter()
        .map(|b| (b.ts, [b.open, b.high, b.low, b.close, b.volume].map(f64::to_bits)))
        .collect()
}

/// What `consolidate_quotes` makes of `tape` at `step`, in [`stored_bars`]' form — what ONE copy of
/// the ticks resamples to.
fn bars_of(tape: &[QuoteTick], step: i64) -> Vec<(i64, [u64; 5])> {
    consolidate_quotes(tape, step)
        .iter()
        .map(|b| (b.ts, [b.open, b.high, b.low, b.close, b.volume].map(f64::to_bits)))
        .collect()
}

fn quote_keys(hist: &DataFusionHist) -> Vec<String> {
    quote_series_facts(hist, SYMBOL).unwrap().1
}

fn bars_written(interval: &str, rows: usize) -> ArchiveBars {
    ArchiveBars { interval: interval.to_string(), rows }
}

/// A FREE day is decoded, stored under the archive day key — pre-spending its provisional twin, as
/// every settled commit does — and resampled at the requested interval under the canonical
/// `dukascopy-resample:` key. A repeat is HELD: it never decodes (the load panics if called) and
/// writes nothing.
#[test]
fn a_free_day_imports_its_ticks_and_bars_and_a_repeat_decodes_and_writes_nothing() {
    let (_dir, hist) = open();
    let day = tape(D, END);
    let one_minute_bars = bars_of(&day, MINUTE);

    let mut import = ArchiveImport::begin(&hist, SYMBOL, &intervals(&["1m"]), now()).unwrap();
    assert_eq!(import.classify(D), ArchiveDayClass::Free);
    let result = import.import_day(D, || Ok(day.clone())).unwrap();
    assert_eq!(
        result,
        ArchiveDayResult::Imported {
            ticks: day.len(),
            bars: vec![bars_written("1m", one_minute_bars.len())]
        }
    );
    assert_eq!(import.classify(D), ArchiveDayClass::HeldByArchive, "the session saw its own write");
    drop(import);

    assert_eq!(stored_ts(&hist, D, END), ts_of(&day));
    assert_eq!(stored_bars(&hist, "1m", D, END), one_minute_bars);
    let keys = quote_keys(&hist);
    assert!(keys.contains(&archive_quote_commit_key(SYMBOL, D, END)), "{keys:?}");
    assert!(
        keys.contains(&provisional_quote_commit_key(SYMBOL, D, END)),
        "the day's provisional twin is pre-spent, so an early HTTP fetch landing later writes \
         nothing: {keys:?}"
    );

    let mut again = ArchiveImport::begin(&hist, SYMBOL, &intervals(&["1m"]), now()).unwrap();
    assert_eq!(again.classify(D), ArchiveDayClass::HeldByArchive);
    let repeat = again.import_day(D, || panic!("a HELD day must never be decoded again")).unwrap();
    assert_eq!(repeat, ArchiveDayResult::ToppedUp { bars: vec![bars_written("1m", 0)] });
    drop(again);
    assert_eq!(stored_ts(&hist, D, END), ts_of(&day), "the repeat stored nothing");
    assert_eq!(stored_bars(&hist, "1m", D, END), one_minute_bars, "nor any bar");
}

/// IMPORT, THEN HTTP — the design's change A. Over a day the archive holds, the HTTP lane never
/// downloads (its fetch PANICS if called) and still writes the requested interval's bars, derived
/// from the archive's stored ticks: at the interval the import never resampled, and at another one.
/// The day's ticks stay one copy.
#[test]
fn the_http_lane_never_fetches_an_imported_day_and_derives_its_bars_from_the_store() {
    let (_dir, hist) = open();
    let day = tape(D, END);
    let mut import = ArchiveImport::begin(&hist, SYMBOL, &[], now()).unwrap();
    let imported = import.import_day(D, || Ok(day.clone())).unwrap();
    assert_eq!(imported, ArchiveDayResult::Imported { ticks: day.len(), bars: vec![] });
    drop(import);

    for (interval, step) in [("1m", MINUTE), ("5m", 5 * MINUTE)] {
        let bars =
            backfill_quotes_then_bars(&hist, SYMBOL, interval, D, END, &|| false, |_, from, to| {
                panic!("the archive holds this day; the HTTP lane fetched [{from}, {to}]")
            })
            .unwrap();
        let expected = bars_of(&day, step);
        assert_eq!(bars, expected.len(), "{interval}: every bar derived from the stored ticks");
        assert_eq!(stored_bars(&hist, interval, D, END), expected, "{interval}");
    }
    assert_eq!(stored_ts(&hist, D, END), ts_of(&day), "one copy of the day's ticks");
    assert!(
        !quote_keys(&hist).contains(&quote_commit_key(SYMBOL, D, END)),
        "the HTTP lane spent no key of its own over the archive's day"
    );
}

/// HTTP, THEN IMPORT. A day the HTTP lane fetched under its canonical day key is HELD by it: the
/// import never decodes it and only tops up an interval the HTTP lane did not write. One copy.
#[test]
fn a_day_the_http_lane_fetched_is_held_by_it_and_stored_once() {
    let (_dir, hist) = open();
    let day = tape(D, END);
    backfill_quotes_then_bars(&hist, SYMBOL, "1m", D, END, &|| false, |_, from, to| {
        Ok(tape(from, to))
    })
    .unwrap();

    let mut import = ArchiveImport::begin(&hist, SYMBOL, &intervals(&["1m", "5m"]), now()).unwrap();
    assert_eq!(import.classify(D), ArchiveDayClass::HeldByHttp);
    let result =
        import.import_day(D, || panic!("a day the HTTP lane holds is never decoded")).unwrap();
    let five_minute_bars = bars_of(&day, 5 * MINUTE);
    assert_eq!(
        result,
        ArchiveDayResult::ToppedUp {
            bars: vec![bars_written("1m", 0), bars_written("5m", five_minute_bars.len())]
        },
        "1m was the HTTP lane's own resample; 5m is topped up from its ticks"
    );
    drop(import);
    assert_eq!(stored_ts(&hist, D, END), ts_of(&day), "one copy of the day's ticks");
    assert_eq!(stored_bars(&hist, "1m", D, END), bars_of(&day, MINUTE));
    assert_eq!(stored_bars(&hist, "5m", D, END), five_minute_bars);
    assert!(!quote_keys(&hist).contains(&archive_quote_commit_key(SYMBOL, D, END)));
}

/// OVERLAP. A ragged HTTP request edge — the last 19 hours of the day, keyed by its own cut — meets
/// the day under other bounds, so the day is REFUSED and listed with the keys, never decoded, and
/// nothing is written: importing it would store those 19 hours twice. The days either side, which no
/// key meets, stay FREE.
#[test]
fn a_ragged_http_edge_refuses_the_day_and_the_import_writes_nothing() {
    let (_dir, hist) = open();
    let edge = D + 5 * 3_600_000;
    backfill_quotes_then_bars(&hist, SYMBOL, "1m", edge, END, &|| false, |_, from, to| {
        Ok(tape(from, to))
    })
    .unwrap();
    let before = stored_ts(&hist, D - MS_PER_DAY, END + MS_PER_DAY);

    let mut import = ArchiveImport::begin(&hist, SYMBOL, &intervals(&["1m"]), now()).unwrap();
    let ArchiveDayClass::Overlapped { keys } = import.classify(D) else {
        panic!("a ragged edge must overlap the day: {:?}", import.classify(D))
    };
    assert!(keys.contains(&quote_commit_key(SYMBOL, edge, END)), "{keys:?}");
    assert_eq!(import.classify(D - MS_PER_DAY), ArchiveDayClass::Free);
    assert_eq!(import.classify(D + MS_PER_DAY), ArchiveDayClass::Free);
    let ArchiveDayResult::Refused(refusal) =
        import.import_day(D, || panic!("an overlapped day is never decoded")).unwrap()
    else {
        panic!("an overlapped day must be refused")
    };
    assert_eq!(refusal.class, "Overlapped");
    assert!(refusal.detail.contains(&quote_commit_key(SYMBOL, edge, END)), "{}", refusal.detail);
    drop(import);
    assert_eq!(stored_ts(&hist, D - MS_PER_DAY, END + MS_PER_DAY), before, "nothing written");
    assert!(!quote_keys(&hist).contains(&archive_quote_commit_key(SYMBOL, D, END)));
}

/// SUPERSEDE. The only key meeting the day is a PROVISIONAL one over exactly the day — an early HTTP
/// fetch that came back short, with its provisional bars — so the import stores the whole day and
/// removes the early part in the same publish, quotes and bars alike: one copy of every tick, and
/// bars built from the full day only.
#[test]
fn an_exact_day_provisional_window_is_superseded_and_the_day_is_stored_once() {
    let (_dir, hist) = open();
    let day = tape(D, END);
    let early: Vec<QuoteTick> = day.iter().take(3).cloned().collect();
    let provisional = provisional_quote_commit_key(SYMBOL, D, END);
    hist.append_quotes(VENUE, SYMBOL, &early, Some(&provisional)).unwrap();
    let range = TsRange::of(D, END);
    let early_bars_key = provisional_resample_key(SYMBOL, "1m", range);
    hist.resample_quotes_to_bars(VENUE, SYMBOL, "1m", range, Some(&early_bars_key)).unwrap();
    assert_eq!(stored_bars(&hist, "1m", D, END), bars_of(&early, MINUTE), "the early state");

    let mut import = ArchiveImport::begin(&hist, SYMBOL, &intervals(&["1m"]), now()).unwrap();
    assert_eq!(import.classify(D), ArchiveDayClass::Supersede { key: provisional.clone() });
    let result = import.import_day(D, || Ok(day.clone())).unwrap();
    let full_bars = bars_of(&day, MINUTE);
    assert_eq!(
        result,
        ArchiveDayResult::Imported {
            ticks: day.len(),
            bars: vec![bars_written("1m", full_bars.len())]
        }
    );
    drop(import);
    assert_eq!(stored_ts(&hist, D, END), ts_of(&day), "the early ticks are gone: one copy");
    assert_eq!(stored_bars(&hist, "1m", D, END), full_bars, "the early bars are gone too");
}

/// CLIP — the design's change B. A `7m` chunk crosses midnight into a day the archive holds, so the
/// HTTP lane fetches ONLY the other day's part of it (the fetch asserts its bounds), keys that part
/// by its own bounds, and resamples the chunk's whole buckets from the store: the archive's ticks for
/// the imported day, its own for the rest. Every tick of the chunk is stored once, and a repeat is
/// held by the piece's own key and fetches nothing.
#[test]
fn a_7m_chunk_crossing_into_an_imported_day_fetches_only_the_other_days_part() {
    let (_dir, hist) = open();
    // The `7m` grid cell holding D's midnight: it starts on the day before and ends inside D.
    let cell = D.div_euclid(SEVEN_MINUTE_CHUNK) * SEVEN_MINUTE_CHUNK;
    let cell_end = cell + SEVEN_MINUTE_CHUNK - 1;
    assert!(cell < D && D < cell_end, "the cell must cross midnight: [{cell}, {cell_end}]");

    let mut import = ArchiveImport::begin(&hist, SYMBOL, &[], now()).unwrap();
    import.import_day(D, || Ok(tape(D, END))).unwrap();
    drop(import);

    let mut calls = Vec::new();
    let bars =
        backfill_quotes_then_bars(&hist, SYMBOL, "7m", cell, cell_end, &|| false, |_, from, to| {
            calls.push((from, to));
            Ok(tape(from, to))
        })
        .unwrap();

    assert_eq!(calls, vec![(cell, D - 1)], "only the day before's part of the chunk is fetched");
    assert!(quote_keys(&hist).contains(&quote_commit_key(SYMBOL, cell, D - 1)), "keyed by its cut");
    let chunk_tape = tape(cell, cell_end);
    assert!(
        chunk_tape.iter().any(|q| q.ts < D) && chunk_tape.iter().any(|q| q.ts >= D),
        "the fixture must put ticks on both sides of midnight"
    );
    assert_eq!(stored_ts(&hist, cell, cell_end), ts_of(&chunk_tape), "every tick once");
    let expected = bars_of(&chunk_tape, SEVEN_MINUTES);
    assert_eq!(bars, expected.len());
    assert_eq!(stored_bars(&hist, "7m", cell, cell_end), expected);

    let repeat =
        backfill_quotes_then_bars(&hist, SYMBOL, "7m", cell, cell_end, &|| false, |_, f, t| {
            panic!("a repeat of a clipped chunk fetched [{f}, {t}]")
        })
        .unwrap();
    assert_eq!(repeat, 0, "its piece's key and its resample key are both spent");
}

/// RACE — the design's change C. The import and the HTTP lane go for the same day at once, 100
/// rounds, each on a fresh day, the HTTP request alternating between a RAGGED window (the day from
/// 01:00) and the whole day.
///
/// Half the rounds put a BARRIER inside both lanes' I/O closures, forcing the one interleaving that
/// is a real race: each lane has asked "is this day free?" and been told yes BEFORE either has
/// written. Only the day-owner lock — and the HTTP lane's re-check under it — keeps that from
/// writing the day twice. MEASURED without them, the two windows fail differently, and both are
/// asserted: the ragged window's ticks are stored TWICE (its key and the archive's are different
/// keys, so the store keeps both), and the whole-day window ends in a spurious supersede REFUSAL
/// (the store finds the day's provisional key already stamped on the archive's part), whose
/// message tells the operator to delete the series. The other rounds run free, sampling the
/// orderings in which one lane simply finishes first.
///
/// Every round must end with both lanes answering `Ok`, no tick stored twice (the tape's
/// timestamps are unique, so a repeated one IS a second copy), and the stored bars exactly what ONE
/// copy of the stored ticks resamples to. Which lane owns the day is the race's to decide: the
/// import's whole day, or — when the ragged HTTP window lands first — that window alone, the day
/// then being refused to the import as overlapped.
#[test]
fn the_import_and_the_http_lane_racing_for_one_day_store_it_once() {
    let (_dir, hist) = open();
    for round in 0..100_i64 {
        let day = D + round * MS_PER_DAY;
        let end = day + MS_PER_DAY - 1;
        let (rendezvous, ragged) = (round % 4 < 2, round % 2 == 0);
        let start = if ragged { day + 3_600_000 } else { day };
        let barrier = Barrier::new(2);
        let (import, http) = std::thread::scope(|s| {
            let import = s.spawn(|| {
                let mut session = ArchiveImport::begin(&hist, SYMBOL, &intervals(&["1m"]), now())?;
                session.import_day(day, || {
                    if rendezvous {
                        barrier.wait();
                    }
                    Ok(tape(day, end))
                })
            });
            let http = s.spawn(|| {
                backfill_quotes_then_bars(&hist, SYMBOL, "1m", start, end, &|| false, |_, f, t| {
                    if rendezvous {
                        barrier.wait();
                    }
                    Ok(tape(f, t))
                })
            });
            (import.join().unwrap(), http.join().unwrap())
        });
        let at = format!("round {round} (barrier: {rendezvous}, ragged: {ragged})");
        assert!(import.is_ok(), "{at}: the import must answer Ok: {import:?}");
        assert!(http.is_ok(), "{at}: the HTTP lane must answer Ok: {http:?}");
        let stored = stored_ts(&hist, day, end);
        let mut once = stored.clone();
        once.dedup();
        assert_eq!(stored, once, "{at}: a tick was stored TWICE");
        let (whole, window) = (ts_of(&tape(day, end)), ts_of(&tape(start, end)));
        assert!(stored == whole || stored == window, "{at}: stored {stored:?}");
        let one_copy: Vec<QuoteTick> =
            tape(day, end).into_iter().filter(|q| stored.contains(&q.ts)).collect();
        assert_eq!(
            stored_bars(&hist, "1m", day, end),
            bars_of(&one_copy, MINUTE),
            "{at}: the bars must be built from one copy"
        );
    }
}

/// A RAGGED HTTP request over a day the archive holds — the shape of every `--days N` window's first
/// day — fetches nothing and does not write that day's bars a second time: it resamples the whole day,
/// landing on the import's own bar key (spent at `1m`, so nothing is written) or writing the whole
/// day's bars once at an interval the import did not derive, which a later whole-day request then
/// finds spent.
#[test]
fn a_ragged_request_over_an_imported_day_writes_its_bars_once() {
    let (_dir, hist) = open();
    let day = tape(D, END);
    let mut import = ArchiveImport::begin(&hist, SYMBOL, &intervals(&["1m"]), now()).unwrap();
    import.import_day(D, || Ok(day.clone())).unwrap();
    drop(import);

    let ragged = D + 3_600_000 + 30_000;
    let never = |_: &str, f: i64, t: i64| -> Result<Vec<QuoteTick>, String> {
        panic!("the archive holds this day; the HTTP lane fetched [{f}, {t}]")
    };
    let one_minute =
        backfill_quotes_then_bars(&hist, SYMBOL, "1m", ragged, END, &|| false, never).unwrap();
    assert_eq!(one_minute, 0, "the import's own 1m day key is spent");
    assert_eq!(stored_bars(&hist, "1m", D, END), bars_of(&day, MINUTE), "1m: one copy");

    let five = bars_of(&day, 5 * MINUTE);
    let first = backfill_quotes_then_bars(&hist, SYMBOL, "5m", ragged, END, &|| false, never);
    assert_eq!(first.unwrap(), five.len(), "the whole day's 5m bars, once");
    let whole = backfill_quotes_then_bars(&hist, SYMBOL, "5m", D, END, &|| false, never).unwrap();
    assert_eq!(whole, 0, "a whole-day request finds the same key spent");
    assert_eq!(stored_bars(&hist, "5m", D, END), five, "5m: one copy");
    assert_eq!(stored_ts(&hist, D, END), ts_of(&day), "and one copy of the ticks");
}

/// RESUME. A day whose ticks landed while its bars did not — here an import asked for no bars, the
/// state a request killed between the two leaves — is HELD on the next run, which derives the bars
/// from the stored ticks without decoding anything.
#[test]
fn a_day_whose_ticks_landed_without_its_bars_is_topped_up_without_decoding() {
    let (_dir, hist) = open();
    let day = tape(D, END);
    let mut first = ArchiveImport::begin(&hist, SYMBOL, &[], now()).unwrap();
    first.import_day(D, || Ok(day.clone())).unwrap();
    drop(first);
    assert!(stored_bars(&hist, "1m", D, END).is_empty(), "no bars yet");

    let mut rerun = ArchiveImport::begin(&hist, SYMBOL, &intervals(&["1m", "1h"]), now()).unwrap();
    let result = rerun.import_day(D, || panic!("a held day is never decoded")).unwrap();
    let (minute, hour) = (bars_of(&day, MINUTE), bars_of(&day, 60 * MINUTE));
    assert_eq!(
        result,
        ArchiveDayResult::ToppedUp {
            bars: vec![bars_written("1m", minute.len()), bars_written("1h", hour.len())]
        }
    );
    drop(rerun);
    assert_eq!(stored_bars(&hist, "1m", D, END), minute);
    assert_eq!(stored_bars(&hist, "1h", D, END), hour);
    assert_eq!(stored_ts(&hist, D, END), ts_of(&day), "and the ticks are untouched");
}

/// TOO RECENT. Today's day ends inside the publication margin, so it is refused — never decoded,
/// nothing written — whatever else is true of it. The archive never writes a provisional key.
#[test]
fn a_day_inside_the_publication_margin_is_refused_and_never_decoded() {
    let (_dir, hist) = open();
    let now = now();
    let today = now.div_euclid(MS_PER_DAY) * MS_PER_DAY;
    let mut import = ArchiveImport::begin(&hist, SYMBOL, &intervals(&["1m"]), now).unwrap();
    assert_eq!(import.classify(today), ArchiveDayClass::TooRecent);
    let ArchiveDayResult::Refused(refusal) =
        import.import_day(today, || panic!("a recent day is never decoded")).unwrap()
    else {
        panic!("a recent day must be refused")
    };
    assert_eq!(refusal.class, "TooRecent");
    drop(import);
    assert!(quote_keys(&hist).is_empty(), "nothing written, no key spent");
}

/// An EMPTY payload — no ticks recorded that day — stores nothing and spends no key, so the day is
/// still FREE for a re-published file.
#[test]
fn an_empty_payload_stores_nothing_and_spends_no_key() {
    let (_dir, hist) = open();
    let mut import = ArchiveImport::begin(&hist, SYMBOL, &intervals(&["1m"]), now()).unwrap();
    assert_eq!(import.import_day(D, || Ok(Vec::new())).unwrap(), ArchiveDayResult::Empty);
    assert_eq!(import.classify(D), ArchiveDayClass::Free);
    drop(import);
    assert!(quote_keys(&hist).is_empty());
}

/// The decoder's own refusal costs that one day, is passed through untouched, and spends no key; the
/// request carries on.
#[test]
fn a_decode_refusal_costs_the_day_and_spends_no_key() {
    let (_dir, hist) = open();
    let refusal = ArchiveDayRefusal {
        class: "NotMonotonic".to_string(),
        detail: "record 12 goes back in time".to_string(),
    };
    let mut import = ArchiveImport::begin(&hist, SYMBOL, &intervals(&["1m"]), now()).unwrap();
    let result = import.import_day(D, || Err(refusal.clone())).unwrap();
    assert_eq!(result, ArchiveDayResult::Refused(refusal));
    let next = import.import_day(D + MS_PER_DAY, || Ok(tape(D + MS_PER_DAY, END + MS_PER_DAY)));
    assert!(matches!(next.unwrap(), ArchiveDayResult::Imported { .. }), "the request carries on");
    drop(import);
    let keys = quote_keys(&hist);
    assert!(!keys.contains(&archive_quote_commit_key(SYMBOL, D, END)), "{keys:?}");
    assert!(stored_ts(&hist, D, END).is_empty());
}

/// A tick outside the day the file names is refused before anything is stored: it would sit on a
/// day this key does not name — and that day may be the other lane's.
#[test]
fn a_tick_outside_the_day_is_refused_and_spends_no_key() {
    let (_dir, hist) = open();
    let mut stray = tape(D, END);
    stray.extend(tape(END + 1, END + 20_000));
    assert!(stray.iter().any(|q| q.ts > END), "the fixture must hold a stray tick");
    let mut import = ArchiveImport::begin(&hist, SYMBOL, &intervals(&["1m"]), now()).unwrap();
    let ArchiveDayResult::Refused(refusal) = import.import_day(D, || Ok(stray)).unwrap() else {
        panic!("a stray tick must refuse the day")
    };
    assert_eq!(refusal.class, "OutsideDay");
    drop(import);
    assert!(quote_keys(&hist).is_empty(), "nothing written, no key spent");
}

/// An interval that does not divide a UTC day — its buckets straddle midnight — and one with no
/// width are refused at `begin`, before the lock is taken or anything is read.
#[test]
fn an_interval_that_does_not_divide_a_day_is_refused_before_anything_is_read() {
    let (_dir, hist) = open();
    for bad in ["7m", "0m", "nonsense", "2d"] {
        let Err(CollectError::Refused(why)) =
            ArchiveImport::begin(&hist, SYMBOL, &intervals(&["1m", bad]), now())
        else {
            panic!("{bad} must be refused")
        };
        assert!(why.contains(bad), "{why}");
    }
}

/// A `day` that is not the start of a UTC day at or after 1970 is the caller's mistake, refused
/// before anything is read.
#[test]
fn a_day_that_is_not_a_utc_midnight_is_refused() {
    let (_dir, hist) = open();
    let mut import = ArchiveImport::begin(&hist, SYMBOL, &[], now()).unwrap();
    for bad in [D + 1, D - 1, -MS_PER_DAY] {
        let result = import.import_day(bad, || panic!("never decoded"));
        assert!(matches!(result, Err(CollectError::Refused(_))), "{bad}: {result:?}");
    }
}

/// The archive's keys are a LANE of their own in the store's provenance vocabulary — the formatter's
/// real output classifies to `dukascopy-archive`, not to the HTTP lane — and `--produced-by
/// dukascopy:` (with the colon) does not match them, while the bare `dukascopy` does.
#[test]
fn an_imported_day_is_a_lane_of_its_own_in_the_stores_provenance() {
    use vike_data::store_kind::{key_matches_prefix, source_for_key};
    let key = archive_quote_commit_key(SYMBOL, D, END);
    assert_eq!(key, format!("dukascopy-archive:EURUSD:{D}-{END}"));
    assert_eq!(source_for_key(&key), Some("dukascopy-archive"));
    assert_eq!(source_for_key(&quote_commit_key(SYMBOL, D, END)), Some("dukascopy"));
    assert!(!key_matches_prefix(&key, "dukascopy:"));
    assert!(key_matches_prefix(&key, "dukascopy"));
}

/// The lock-free plan read sees what a session sees: `DayOwners::parse` over `quote_series_facts`
/// classifies the store's days exactly as an `ArchiveImport` does — what a dry run relies on.
#[test]
fn a_dry_run_plan_classifies_exactly_as_the_import_does() {
    let (_dir, hist) = open();
    let mut import = ArchiveImport::begin(&hist, SYMBOL, &[], now()).unwrap();
    import.import_day(D, || Ok(tape(D, END))).unwrap();
    drop(import);
    backfill_quotes_then_bars(
        &hist,
        SYMBOL,
        "1m",
        D + MS_PER_DAY,
        END + MS_PER_DAY,
        &|| false,
        |_, f, t| Ok(tape(f, t)),
    )
    .unwrap();

    let (coverage, keys) = quote_series_facts(&hist, SYMBOL).unwrap();
    assert_eq!(coverage.rows, 2 * OFFSETS.len() as u64);
    let owners = DayOwners::parse(SYMBOL, &keys);
    let session = ArchiveImport::begin(&hist, SYMBOL, &[], now()).unwrap();
    for day in [D - MS_PER_DAY, D, D + MS_PER_DAY, D + 2 * MS_PER_DAY] {
        assert_eq!(owners.classify(day, now()), session.classify(day), "{day}");
    }
    assert_eq!(owners.classify(D, now()), ArchiveDayClass::HeldByArchive);
    assert_eq!(owners.classify(D + MS_PER_DAY, now()), ArchiveDayClass::HeldByHttp);
    assert_eq!(session.coverage().rows, coverage.rows);
}
