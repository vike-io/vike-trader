//! Coverage and inventory, the delete verbs, `series_gaps`, and every verb over a grouped series.

use vike_data::{HistoryStore, SeriesId};
use vike_marketdata::{Bar, TickQuote};
use vike_model::StoreKindId;
use vike_store::HistoryStoreDataFusion;

use crate::common::{DAY, bar, bars_series, qt};

// ---- SeriesCoverage / inventory --------------------------------------------------------------

#[test]
fn inventory_reports_series_coverage_from_manifests() {
    let dir = tempfile::tempdir().unwrap();
    let df = HistoryStoreDataFusion::open(dir.path()).unwrap();
    let bars = vec![bar(1_000, 100.0, None), bar(61_000, 101.0, None)];
    df.append_bars("binance", "BTCUSDT", "1m", &bars, None).unwrap();

    let inventory = df.inventory().unwrap();
    assert_eq!(inventory.len(), 1, "one series");
    let (id, cov) = &inventory[0];
    assert_eq!(id.kind, StoreKindId::Bar);
    assert_eq!(id.venue, "binance");
    assert_eq!(id.symbol, "BTCUSDT");
    assert_eq!(cov.rows, 2);
    assert!(cov.parts >= 1);
    assert!(cov.first_timestamp <= cov.last_timestamp);
    assert!(cov.bytes > 0, "part files have a size on disk");

    // series_coverage for the same id matches
    let direct = df.series_coverage(id).unwrap();
    assert_eq!(&direct, cov);
}

/// A second inventory answers an unchanged series WITHOUT reading its manifest (the full walk per
/// request was 3 to 5 s on the CI box). The base manifest is overwritten with garbage of the SAME length
/// and its modification time put back, so its stamp has not moved and only a re-read could notice:
/// a coverage read by a handle with no memo refuses the garbage, the inventory still answers. (The
/// writing handle's own coverage read would answer from the manifest its append kept.)
#[test]
fn an_unchanged_series_is_answered_without_reading_its_manifest() {
    let dir = tempfile::tempdir().unwrap();
    let df = HistoryStoreDataFusion::open(dir.path()).unwrap();
    let bars = vec![bar(1_000, 100.0, None), bar(61_000, 101.0, None)];
    df.append_bars("binance", "BTCUSDT", "1m", &bars, None).unwrap();
    let first = df.inventory().unwrap();

    let base = bars_series(dir.path()).join("_manifest.json");
    let meta = std::fs::metadata(&base).unwrap();
    let mtime = meta.modified().unwrap();
    std::fs::write(&base, vec![b'x'; meta.len() as usize]).unwrap();
    std::fs::File::options().write(true).open(&base).unwrap().set_modified(mtime).unwrap();

    let cold = HistoryStoreDataFusion::open_read_only(dir.path()).unwrap();
    assert!(
        cold.series_coverage(&first[0].0).is_err(),
        "the premise: a re-read refuses the garbage"
    );
    assert_eq!(df.inventory().unwrap(), first, "answered from the memo, not from the manifest");
}

/// The other half of the memo: a write moves the stamp, so the next inventory sees it.
#[test]
fn a_write_is_seen_by_the_next_inventory() {
    let dir = tempfile::tempdir().unwrap();
    let df = HistoryStoreDataFusion::open(dir.path()).unwrap();
    df.append_bars("binance", "BTCUSDT", "1m", &[bar(1_000, 100.0, None)], None).unwrap();
    assert_eq!(df.inventory().unwrap()[0].1.rows, 1);

    df.append_bars("binance", "BTCUSDT", "1m", &[bar(61_000, 101.0, None)], None).unwrap();
    let after = df.inventory().unwrap();
    assert_eq!(after[0].1.rows, 2, "the append is in the next answer");
    assert_eq!(after[0].1, df.series_coverage(&after[0].0).unwrap(), "and equals a fresh fold");

    df.delete_series(&after[0].0).unwrap();
    assert!(df.inventory().unwrap().is_empty(), "a deleted series leaves the answer");
}

/// A series being recorded moves only its LOG between two folds of its base, and the memo then
/// replays just the frames logged since over the manifest it kept. Every inventory and coverage
/// report still equals a fresh store's: through appends on a held and on a new day, a compaction (a
/// frame that removes parts) and an append after it.
#[test]
fn a_series_whose_log_alone_moved_folds_like_a_fresh_read() {
    let dir = tempfile::tempdir().unwrap();
    let df = HistoryStoreDataFusion::open(dir.path()).unwrap();
    let check = |step: &str| {
        let fresh = HistoryStoreDataFusion::open(dir.path()).unwrap();
        assert_eq!(df.inventory().unwrap(), fresh.inventory().unwrap(), "{step}: the inventory");
        assert_eq!(
            df.coverage_report().unwrap(),
            fresh.coverage_report().unwrap(),
            "{step}: the coverage report"
        );
    };
    let append = |ts: i64, key: &str| {
        df.append_quotes("binance", "BTCUSDT", &[qt(ts, 1.0)], Some(key)).unwrap();
    };
    append(1_000, "q0");
    check("the base alone");
    for (n, ts) in [2_000, DAY + 1_000, DAY + 2_000, 3 * DAY].into_iter().enumerate() {
        append(ts, &format!("q{}", n + 1));
        check(&format!("append {n}"));
    }
    let config =
        vike_store::CompactionConfig { target_bytes: 1 << 20, min_parts: 2, ..Default::default() };
    df.compact_series(StoreKindId::Quote, "binance", "BTCUSDT", None, &config).unwrap();
    check("a compaction");
    append(3 * DAY + 1_000, "q9");
    check("an append after the compaction");
}

/// PR-6: the `HistoryStore` TRAIT overrides for `list_series`/`inventory`/`series_gaps` reach the REAL
/// manifest walk when the store is erased to `&dyn HistoryStore` (how vike-datahub's `serve` holds it),
/// NOT the trait default (which now REFUSES the catalog pair outright). This is the whole point of
/// the widening — proving the
/// inherent→trait delegation on `HistoryStoreDataFusion` so `HistoryStoreDatahub` can serve the catalog.
#[test]
fn trait_object_metadata_verbs_reach_real_data() {
    let dir = tempfile::tempdir().unwrap();
    let df = HistoryStoreDataFusion::open(dir.path()).unwrap();
    let bars = vec![bar(1_000, 100.0, None), bar(61_000, 101.0, None)];
    df.append_bars("binance", "BTCUSDT", "1m", &bars, None).unwrap();

    // erase to the trait object exactly as `serve` holds it (`Arc<dyn HistoryStore + Send + Sync>`).
    let store: &dyn HistoryStore = &df;

    let series = store.list_series().unwrap();
    assert_eq!(series.len(), 1, "the trait method reaches the real series, NOT the trait default");
    assert_eq!(series[0].venue, "binance");
    assert_eq!(series[0].kind, StoreKindId::Bar);

    let inventory = store.inventory().unwrap();
    assert_eq!(inventory.len(), 1, "trait inventory reaches real coverage");
    assert_eq!(inventory[0].1.rows, 2, "coverage is the real manifest fold, not a default zero");

    // series_gaps via the trait matches the concrete inherent call (delegation is behaviour-identical)
    let gaps = store.series_gaps(&series[0]).unwrap();
    assert_eq!(gaps, df.series_gaps(&series[0]).unwrap(), "trait gaps == inherent gaps");
}

#[test]
fn delete_series_removes_the_series_and_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let df = HistoryStoreDataFusion::open(dir.path()).unwrap();
    df.append_bars("binance", "BTCUSDT", "1m", &[bar(1_000, 100.0, None)], None).unwrap();
    df.append_bars("okx", "BTC-USDT", "1m", &[bar(1_000, 100.0, None)], None).unwrap();
    assert_eq!(df.inventory().unwrap().len(), 2);

    let target = df.list_series().unwrap().into_iter().find(|s| s.venue == "binance").unwrap();
    df.delete_series(&target).unwrap();

    let left = df.list_series().unwrap();
    assert_eq!(left.len(), 1, "only the binance series was deleted");
    assert_eq!(left[0].venue, "okx", "sibling untouched");

    // idempotent: deleting an already-absent series is Ok
    df.delete_series(&target).unwrap();
    assert_eq!(df.list_series().unwrap().len(), 1);
}

/// ⚠ The idempotent path must not CREATE the leaf it came to delete.
///
/// `SeriesLock::acquire` opens its lock file with a `create_dir_all` in front of it, so a locked
/// delete that took the guard before probing would materialize a `kind=/venue=/symbol=` directory
/// holding one empty `_manifest.lock` for every mistyped series anybody ever asked it to remove.
/// The probe therefore comes FIRST, and this is what says so.
#[test]
fn deleting_an_absent_series_creates_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let df = HistoryStoreDataFusion::open(dir.path()).unwrap();
    let ghost =
        SeriesId::per_symbol(StoreKindId::Bar, "binance", "NOTHING", Some("1m".to_string()));

    df.delete_series(&ghost).unwrap();

    assert!(
        !dir.path().join("kind=bar").exists(),
        "a delete of an absent series must leave no directory behind"
    );
    assert!(df.list_series().unwrap().is_empty());
}

/// **The lock the delete never used to take.** Every other mutating verb on this type serializes on
/// `_manifest.lock`; `delete_series` did not, so a cleanup run against the box that is RECORDING
/// could remove a leaf mid-append.
///
/// The proof holds the OS advisory lock from the TEST (exactly as a live writer's `SeriesLock`
/// does) and asserts the delete cannot proceed — and, crucially, that the series is still whole
/// afterwards, because a delete that failed AFTER removing the parts would be worse than one that
/// never locked.
///
/// ⚠ It costs the full spin budget (`SPIN_ATTEMPTS` × 2 ms ≈ 4 s) by construction: the contended
/// path IS the timeout, and shortening it would need a test-only knob on a code path whose whole
/// value is that it has none.
#[test]
fn a_delete_cannot_take_a_series_a_live_writer_is_holding() {
    let dir = tempfile::tempdir().unwrap();
    let df = HistoryStoreDataFusion::open(dir.path()).unwrap();
    df.append_bars("binance", "BTCUSDT", "1m", &[bar(1_000, 100.0, None)], Some("a")).unwrap();
    let id = df.list_series().unwrap().pop().unwrap();
    let before = df.series_coverage(&id).unwrap();
    assert_eq!(before.rows, 1);

    // What a live writer holds: the OS lock on `_manifest.lock`, not the file's existence.
    let held = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(bars_series(dir.path()).join("_manifest.lock"))
        .unwrap();
    held.try_lock().expect("the test must be the holder for this to prove anything");

    let err =
        df.delete_series(&id).expect_err("a held series must not be deleted under the holder");
    assert!(err.to_string().contains("timeout acquiring series lock"), "{err}");
    assert_eq!(
        df.series_coverage(&id).unwrap(),
        before,
        "a refused delete must leave the series exactly as it found it"
    );

    // ...and once the writer is gone the same call succeeds, so the refusal was the LOCK and not
    // something about the series.
    held.unlock().unwrap();
    drop(held);
    df.delete_series(&id).unwrap();
    assert!(df.list_series().unwrap().is_empty());
}

/// The provenance assertion, at the one place it can be trusted: inside the critical section.
///
/// Three shapes, and the middle one is the whole feature — a MIXED series refuses rather than
/// being silently skipped, because a filter would have removed the panel rows and left an operator
/// believing the venue's own candles had gone with them (or vice versa).
#[test]
fn a_checked_delete_refuses_a_key_the_assertion_does_not_cover() {
    let dir = tempfile::tempdir().unwrap();
    let df = HistoryStoreDataFusion::open(dir.path()).unwrap();
    df.append_bars("hyperliquid", "BTC", "1h", &[bar(1_000, 100.0, None)], Some("panel_bars:1"))
        .unwrap();
    df.append_bars("hyperliquid", "BTC", "1h", &[bar(2_000, 101.0, None)], Some("klines:1"))
        .unwrap();
    let id = df.list_series().unwrap().pop().unwrap();
    assert_eq!(df.series_commits(&id).unwrap(), vec!["panel_bars:1", "klines:1"]);

    let err = df.delete_series_checked(&id, Some("panel_bars:")).unwrap_err();
    assert!(err.to_string().contains("klines:1"), "the refusal names the offending key: {err}");
    assert_eq!(df.series_coverage(&id).unwrap().rows, 2, "nothing was deleted");

    // A series whose EVERY key carries the prefix goes.
    let dir2 = tempfile::tempdir().unwrap();
    let df2 = HistoryStoreDataFusion::open(dir2.path()).unwrap();
    df2.append_bars("hyperliquid", "BTC", "1h", &[bar(1_000, 100.0, None)], Some("panel_bars:1"))
        .unwrap();
    let id2 = df2.list_series().unwrap().pop().unwrap();
    df2.delete_series_checked(&id2, Some("panel_bars:")).unwrap();
    assert!(df2.list_series().unwrap().is_empty());

    // A KEYLESS series records nothing, so it can satisfy no assertion — and refusing is the only
    // safe reading of "I do not know who wrote this".
    let dir3 = tempfile::tempdir().unwrap();
    let df3 = HistoryStoreDataFusion::open(dir3.path()).unwrap();
    df3.append_bars("hyperliquid", "BTC", "1h", &[bar(1_000, 100.0, None)], None).unwrap();
    let id3 = df3.list_series().unwrap().pop().unwrap();
    assert!(df3.series_commits(&id3).unwrap().is_empty());
    let err = df3.delete_series_checked(&id3, Some("panel_bars:")).unwrap_err();
    assert!(err.to_string().contains("NO commit keys"), "{err}");
    // ...and WITHOUT an assertion the same series is deletable, which is the difference between
    // deleting by name and deleting by a checked property.
    df3.delete_series(&id3).unwrap();
    assert!(df3.list_series().unwrap().is_empty());
}

/// A GROUPED series is deleted by its `group=` leaf, not by a phantom `symbol=` path — the
/// five-bug class `series_dir_of`'s doc records, asserted for the verb that used to be one of them.
#[test]
fn a_grouped_series_is_deleted_by_its_group_leaf() {
    let dir = tempfile::tempdir().unwrap();
    let df = HistoryStoreDataFusion::open(dir.path()).unwrap();
    // A GROUPED row must carry its own symbol — the group leaf tells rows apart by that column.
    let mut in_group = qt(1_000, 1.0);
    in_group.symbol = "0xdead".into();
    df.append_quotes_grouped("polymarket", "btc-5m", &[in_group], Some("g1")).unwrap();
    df.append_quotes("polymarket", "0xdead", &[qt(1_000, 1.0)], Some("s1")).unwrap();
    assert_eq!(df.list_series().unwrap().len(), 2);

    let grouped =
        df.list_series().unwrap().into_iter().find(|s| s.group.is_some()).expect("grouped series");
    assert_eq!(df.series_commits(&grouped).unwrap(), vec!["g1"]);
    df.delete_series(&grouped).unwrap();

    let left = df.list_series().unwrap();
    assert_eq!(left.len(), 1, "only the grouped leaf went");
    assert!(left[0].group.is_none(), "the per-symbol sibling is untouched");
}

/// The plan/execute pair over a real store: the selector intersects the store's enumeration, the
/// plan carries provenance, and a refused verdict deletes NOTHING — not even the series that would
/// have passed on their own.
#[test]
fn a_removal_plan_is_all_or_nothing_across_the_selected_set() {
    use vike_data::store::removal::{SeriesSelector, execute_removal, plan_removal};

    let dir = tempfile::tempdir().unwrap();
    let df = HistoryStoreDataFusion::open(dir.path()).unwrap();
    df.append_bars("hyperliquid", "BTC", "1h", &[bar(1_000, 100.0, None)], Some("panel_bars:1"))
        .unwrap();
    df.append_bars("hyperliquid", "ETH", "1h", &[bar(1_000, 100.0, None)], Some("panel_bars:2"))
        .unwrap();
    // The mixed one: a legitimate producer wrote into a series the sweep also selects.
    df.append_bars("hyperliquid", "SOL", "1h", &[bar(1_000, 100.0, None)], Some("panel_bars:3"))
        .unwrap();
    df.append_bars("hyperliquid", "SOL", "1h", &[bar(2_000, 101.0, None)], Some("klines:9"))
        .unwrap();
    // ...and a series OUTSIDE the selector, to prove the blast radius.
    df.append_bars("binance", "BTCUSDT", "1h", &[bar(1_000, 100.0, None)], Some("klines:1"))
        .unwrap();

    let selector = SeriesSelector::new(StoreKindId::Bar, "hyperliquid");
    let plan = plan_removal(&df, &selector, Some("panel_bars:")).unwrap();
    assert_eq!(plan.matched(), 3, "the binance series is outside the selector");
    assert_eq!(plan.rows(), 4);

    let err = execute_removal(&df, &plan).unwrap_err().to_string();
    assert!(err.contains("provenance REFUSED"), "{err}");
    assert_eq!(df.list_series().unwrap().len(), 4, "ONE foreign key refuses the WHOLE run");

    // Naming the mixed series in full is the way through — a different command line with a
    // different plan, rather than a flag that disarms the check.
    let mut narrowed = SeriesSelector::new(StoreKindId::Bar, "hyperliquid");
    narrowed.symbol = Some("BTC".to_string());
    narrowed.interval = Some("1h".to_string());
    let plan = plan_removal(&df, &narrowed, Some("panel_bars:")).unwrap();
    assert_eq!(plan.matched(), 1);
    let outcome = execute_removal(&df, &plan).unwrap();
    assert!(outcome.is_clean());
    assert_eq!(outcome.deleted.len(), 1);
    assert_eq!(df.list_series().unwrap().len(), 3);

    // Nothing matched is a plan that says so, and executing it is a clean no-op.
    let plan = plan_removal(&df, &narrowed, Some("panel_bars:")).unwrap();
    assert_eq!(plan.matched(), 0);
    let outcome = execute_removal(&df, &plan).unwrap();
    assert!(outcome.is_clean() && outcome.deleted.is_empty());
}

// ---- series_gaps (per-series gap detection over the manifest) ------------------------------

fn bar_series_id(venue: &str, symbol: &str) -> SeriesId {
    SeriesId {
        kind: StoreKindId::Bar,
        venue: venue.to_string(),
        symbol: symbol.to_string(),
        interval: Some("1m".to_string()),
        group: None,
        source: None,
    }
}

#[test]
fn series_gaps_finds_the_hole_between_two_non_adjacent_date_ranges() {
    let dir = tempfile::tempdir().unwrap();
    let df = HistoryStoreDataFusion::open(dir.path()).unwrap();
    // present: day 0, day 1 ... then a hole ... present again: day 5. Days 2,3,4 are missing.
    let bars: Vec<Bar> =
        vec![bar(0, 100.0, None), bar(DAY, 101.0, None), bar(5 * DAY, 102.0, None)];
    df.append_bars("binance", "BTCUSDT", "1m", &bars, None).unwrap();

    let id = bar_series_id("binance", "BTCUSDT");
    let gaps = df.series_gaps(&id).unwrap();
    // inclusive epoch-ms range spanning the whole missing days [2,4]: 2*DAY .. (5*DAY - 1)
    assert_eq!(gaps, vec![(2 * DAY, 5 * DAY - 1)], "days 2,3,4 missing between day 1 and day 5");
}

#[test]
fn series_gaps_reports_multiple_holes_across_more_than_two_runs() {
    let dir = tempfile::tempdir().unwrap();
    let df = HistoryStoreDataFusion::open(dir.path()).unwrap();
    // present days: 0, 2, 3, 7 -> gaps at day 1, and days 4..6
    let bars: Vec<Bar> = vec![
        bar(0, 100.0, None),
        bar(2 * DAY, 101.0, None),
        bar(3 * DAY, 102.0, None),
        bar(7 * DAY, 103.0, None),
    ];
    df.append_bars("binance", "BTCUSDT", "1m", &bars, None).unwrap();

    let id = bar_series_id("binance", "BTCUSDT");
    let gaps = df.series_gaps(&id).unwrap();
    assert_eq!(gaps, vec![(DAY, 2 * DAY - 1), (4 * DAY, 7 * DAY - 1)]);
}

#[test]
fn series_gaps_empty_for_contiguous_coverage_and_never_errors_on_absent_series() {
    let dir = tempfile::tempdir().unwrap();
    let df = HistoryStoreDataFusion::open(dir.path()).unwrap();
    let bars: Vec<Bar> =
        vec![bar(0, 100.0, None), bar(DAY, 101.0, None), bar(2 * DAY, 102.0, None)];
    df.append_bars("binance", "BTCUSDT", "1m", &bars, None).unwrap();

    let contiguous = bar_series_id("binance", "BTCUSDT");
    assert_eq!(df.series_gaps(&contiguous).unwrap(), Vec::new(), "contiguous coverage, no gaps");

    // a series with no data at all is Ok(vec![]), never an error
    let absent = bar_series_id("okx", "BTC-USDT");
    assert_eq!(df.series_gaps(&absent).unwrap(), Vec::new());
}

/// The marker a chunked ingest spends for the whole-day chunk `[first_day, end_day)`.
fn empty_marker(first_day: i64, end_day: i64) -> String {
    format!("binance:BTCUSDT:1m:{}-{}:empty", first_day * DAY, end_day * DAY - 1)
}

/// A day whose `:empty` key is spent was asked for and proven to hold nothing, so it is no hole; a
/// day nobody marked still is, and a chunk wider than a day clears every day it spans.
#[test]
fn series_gaps_subtracts_the_days_whose_empty_marker_is_spent() {
    let dir = tempfile::tempdir().unwrap();
    let df = HistoryStoreDataFusion::open(dir.path()).unwrap();
    let bars: Vec<Bar> = [10, 11, 15, 19].map(|d| bar(d * DAY, 100.0, None)).to_vec();
    df.append_bars("binance", "BTCUSDT", "1m", &bars, None).unwrap();
    let id = bar_series_id("binance", "BTCUSDT");
    assert_eq!(
        df.series_gaps(&id).unwrap(),
        vec![(12 * DAY, 15 * DAY - 1), (16 * DAY, 19 * DAY - 1)]
    );

    // Day 13 alone, and days 16 and 17 as one two-day chunk.
    let markers = [empty_marker(13, 14), empty_marker(16, 18)];
    let keys: Vec<&str> = markers.iter().map(String::as_str).collect();
    df.spend_keys_without_rows(&id, &keys).unwrap();
    assert_eq!(
        df.series_gaps(&id).unwrap(),
        vec![(12 * DAY, 13 * DAY - 1), (14 * DAY, 15 * DAY - 1), (18 * DAY, 19 * DAY - 1)],
        "days 12, 14 and 18 are still holes; 13, 16 and 17 are proven empty"
    );
}

/// A marker clears a hole and never makes one: one outside the span the parts hold (a request that
/// opened before the first bar) changes nothing, and neither does a key that is not a marker.
#[test]
fn series_gaps_ignores_a_marker_outside_the_span_and_every_other_orphan_key() {
    let dir = tempfile::tempdir().unwrap();
    let df = HistoryStoreDataFusion::open(dir.path()).unwrap();
    let bars: Vec<Bar> = [10, 12].map(|d| bar(d * DAY, 100.0, None)).to_vec();
    df.append_bars("binance", "BTCUSDT", "1m", &bars, None).unwrap();
    let id = bar_series_id("binance", "BTCUSDT");
    let before = vec![(11 * DAY, 12 * DAY - 1)];
    assert_eq!(df.series_gaps(&id).unwrap(), before);

    let window_key = format!("binance:BTCUSDT:1m:{}-{}", 11 * DAY, 12 * DAY - 1);
    let no_such_venue = format!("no-such-venue:BTCUSDT:1m:{}-{}:empty", 11 * DAY, 12 * DAY - 1);
    let keys = [
        empty_marker(3, 5),
        empty_marker(20, 21),
        window_key,
        no_such_venue,
        "not-a-window-key:empty".to_string(),
    ];
    let keys: Vec<&str> = keys.iter().map(String::as_str).collect();
    df.spend_keys_without_rows(&id, &keys).unwrap();
    assert_eq!(df.series_gaps(&id).unwrap(), before, "no marker of this series covers day 11");
}

// ============================ grouped series: every SeriesId-taking verb ============================
//
// Five methods built their directory from `id.symbol`, which is EMPTY for a grouped series, so each
// looked at a path ending `symbol=`. `read_manifest` returns an EMPTY manifest for a missing dir
// rather than erroring, so all five silently succeeded while doing nothing. These pin the fix.
//
// Found live: a 148 MB recorded tape whose entire Polymarket group (37.5 M book rows) rendered in
// the Data Manager as `0 rows · 0 B`.

/// A store holding ONE grouped quote series (`polymarket/btc-5m`, three symbols, two days) plus a
/// per-symbol sibling, so every assertion below can also show the per-symbol path still works.
fn grouped_store() -> (tempfile::TempDir, HistoryStoreDataFusion, SeriesId) {
    let dir = tempfile::tempdir().unwrap();
    let store = HistoryStoreDataFusion::open(dir.path()).unwrap();
    let day = 86_400_000i64;
    let base = 1_700_000_000_000i64 - (1_700_000_000_000i64 % day); // midnight UTC
    let mk = |symbol_name: &str, ts: i64| TickQuote {
        timestamp: ts,
        local_timestamp: ts + 1,
        bid: 0.5,
        ask: 0.6,
        bid_size: 1.0,
        ask_size: 2.0,
        symbol: symbol_name.into(),
    };
    store
        .append_quotes_grouped(
            "polymarket",
            "btc-5m",
            &[mk("AAA", base), mk("BBB", base + 1), mk("CCC", base + day)],
            Some("g1"),
        )
        .unwrap();
    store.append_quotes("binance", "BTCUSDT", &[mk("BTCUSDT", base)], Some("s1")).unwrap();
    let id = SeriesId::grouped(StoreKindId::Quote, "polymarket", "btc-5m");
    (dir, store, id)
}

/// **The one the screenshot exposed.** A grouped series' coverage is its real rows/bytes/span, not
/// the zeroes a missing manifest yields.
#[test]
fn series_coverage_reads_a_grouped_series() {
    let (_d, store, id) = grouped_store();
    let cov = store.series_coverage(&id).unwrap();
    assert_eq!(cov.rows, 3, "grouped rows must be counted, not reported as 0");
    assert!(cov.bytes > 0, "grouped parts have a size on disk");
    assert_eq!(cov.dates, 2, "two `date=` partitions");
    assert!(cov.first_timestamp < cov.last_timestamp, "a real span, not the default sentinel");
}

/// `inventory()` is `series_coverage` per listed series — the Data Manager's actual source. It
/// must show the grouped series AND its coverage, not a row of zeroes.
#[test]
fn inventory_reports_grouped_coverage() {
    let (_d, store, _id) = grouped_store();
    let inventory = store.inventory().unwrap();
    let (_, cov) = inventory
        .iter()
        .find(|(id, _)| id.group.as_deref() == Some("btc-5m"))
        .expect("the grouped series is listed");
    assert_eq!(cov.rows, 3, "listed but empty is the bug this pins");
}

/// The cross-kind report keyed off the same path — a grouped instrument must contribute its days,
/// or the Partial column can never say anything about a grouped family.
#[test]
fn coverage_report_sees_a_grouped_instruments_days() {
    let (_d, store, _id) = grouped_store();
    let report = store.coverage_report().unwrap();
    let c = report
        .iter()
        .find(|c| c.key.grouped && c.key.label == "btc-5m")
        .expect("the grouped instrument is in the report");
    assert_eq!(c.spanned_days().len(), 2, "both recorded days, not zero");
}

/// The gap column AND `vike_archive_backfill --gaps` both read this. Reporting "no data" for a
/// grouped series would make `--gaps` re-fetch history that is already on disk.
#[test]
fn series_gaps_reads_a_grouped_series() {
    let (_d, store, id) = grouped_store();
    // "No gaps" would pass under the bug too — an empty day list has no gaps either. So punch a
    // REAL hole: the fixture holds days 0 and 1; add day 4, leaving days 2-3 missing.
    let day = 86_400_000i64;
    let base = 1_700_000_000_000i64 - (1_700_000_000_000i64 % day);
    let far = TickQuote {
        timestamp: base + 4 * day,
        local_timestamp: base + 4 * day + 1,
        bid: 0.5,
        ask: 0.6,
        bid_size: 1.0,
        ask_size: 2.0,
        symbol: "AAA".into(),
    };
    store.append_quotes_grouped("polymarket", "btc-5m", &[far], Some("g2")).unwrap();

    let gaps = store.series_gaps(&id).unwrap();
    assert_eq!(gaps.len(), 1, "the missing days are one range, got {gaps:?}");
    // Inclusive epoch-ms (same convention as `SeriesCoverage::first_timestamp`/`last_timestamp`), so the range
    // runs from the START of day 2 to the LAST MILLISECOND of day 3 — not to day 3's start.
    assert_eq!(gaps[0], (base + 2 * day, base + 4 * day - 1), "the gap spans exactly days 2-3");
}

/// **The most dangerous face.** Delete used to return `Ok(())` having removed nothing: the
/// Data Manager's Delete button was a silent no-op on every grouped series.
#[test]
fn delete_series_actually_deletes_a_grouped_series() {
    let (dir, store, id) = grouped_store();
    let group_dir = dir.path().join("kind=quote").join("venue=polymarket").join("group=btc-5m");
    assert!(group_dir.exists(), "fixture sanity");

    store.delete_series(&id).unwrap();

    assert!(!group_dir.exists(), "delete reported success but left the series on disk");
    assert!(
        !store.inventory().unwrap().iter().any(|(i, _)| i.group.as_deref() == Some("btc-5m")),
        "deleted series still listed"
    );
    // The per-symbol sibling under a DIFFERENT venue is untouched.
    assert!(store.inventory().unwrap().iter().any(|(i, _)| i.symbol == "BTCUSDT"));
}

/// Rebuilding from the parts on disk must find the grouped parts, not a phantom empty directory.
#[test]
fn rebuild_series_manifest_reads_a_grouped_series() {
    let (_d, store, id) = grouped_store();
    let report = store.rebuild_series_manifest(&id).unwrap();
    assert!(
        report.parts_recovered > 0,
        "rebuild recovered no parts: {report:?} — the phantom-directory bug (it was looking at a \
         `symbol=` path that does not exist, and an empty dir has nothing to recover)"
    );
    assert_eq!(report.parts_unreadable, 0, "{report:?}");
    assert_eq!(store.series_coverage(&id).unwrap().rows, 3, "rebuild preserved the rows");
}
