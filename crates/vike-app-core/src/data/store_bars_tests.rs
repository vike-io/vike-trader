use super::*;
use crate::backend::split_plane::{BarPlane, SeriesSource, series_render_source};
use vike_data::test_support::MemHistStore;

/// A fixed "now" for every read below, so the bounded window [`read_range`] computes is a
/// function of the test rather than of when it ran.
const NOW_MS: i64 = 1_700_000_000_000;

fn target(venue: &str, symbol: &str, interval: &str, has_bars: bool) -> ChartTarget {
    classed_target(venue, symbol, interval, has_bars, None)
}

fn classed_target(
    venue: &str,
    symbol: &str,
    interval: &str,
    has_bars: bool,
    class: Option<vike_model::AssetClass>,
) -> ChartTarget {
    ChartTarget {
        id: egui::Id::new(format!("{venue}:{symbol}@{interval}:{class:?}")),
        venue: venue.to_string(),
        symbol: symbol.to_string(),
        interval: interval.to_string(),
        pinned: false,
        interval_pinned: false,
        has_bars,
        class,
    }
}

fn bar(ts: i64) -> vike_model::Bar {
    vike_model::Bar {
        ts,
        open: 100.0,
        high: 101.0,
        low: 99.0,
        close: 100.5,
        volume: 1.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}

/// THE OWNER'S CASE: a `5m` chart on a node publishing only `1m`. It is planned, and the `1m`
/// chart beside it is NOT — the backend streams that one.
#[test]
fn an_unpublished_kline_is_planned_and_a_published_one_is_not() {
    let targets =
        [target("bybit", "BTCUSDT", "5m", false), target("bybit", "BTCUSDT", "1m", false)];
    let published = [PublishedSeries::new("bybit", "BTCUSDT", "1m")];
    let plan = plan_store_reads(&targets, &published, &HashSet::new());
    assert_eq!(plan.len(), 1, "exactly the series the backend does not stream");
    assert_eq!(plan[0].key(), "bybit:BTCUSDT@5m");
}

/// ⚠ The fetch rule and the RENDER rule are the same rule from two ends: every key this plans
/// is one `series_render_source` assigns to the store under the desktop's plane, and every key
/// it refuses on publication grounds is one that function keeps on the snapshot. A regression
/// in either direction (a planned read the fold would never paint, or a painted series nobody
/// fetches) fails here.
#[test]
fn everything_planned_is_what_the_render_side_assigns_to_the_store() {
    let targets = [
        target("bybit", "BTCUSDT", "5m", false),
        target("okx", "BTC-USDT", "1h", false),
        target("bybit", "BTCUSDT", "1m", false), // published — snapshot's
    ];
    let published = [PublishedSeries::new("bybit", "BTCUSDT", "1m")];
    let plan = plan_store_reads(&targets, &published, &HashSet::new());
    for r in &plan {
        assert_eq!(
            series_render_source(BarPlane::BackendStore, &r.venue, &r.interval, false),
            SeriesSource::DirectBars,
            "{}: planned but the fold would not paint it from the store",
            r.key()
        );
    }
    assert_eq!(
        series_render_source(BarPlane::BackendStore, "bybit", "1m", true),
        SeriesSource::SnapshotBars,
        "the published series the plan skipped is the fold's snapshot half"
    );
}

/// A chart that is already painting is never disturbed, and a tick/volume chart is never asked
/// for at all (the store has no such series — those fold from the local tape).
#[test]
fn a_painting_chart_and_a_tape_chart_are_both_left_alone() {
    let targets = [
        target("bybit", "BTCUSDT", "5m", true), // painting
        target("bybit", "BTCUSDT", "100t", false),
        target("bybit", "BTCUSDT", "10v", false),
    ];
    assert!(plan_store_reads(&targets, &[], &HashSet::new()).is_empty());
}

/// **ASKED ONCE, NOT ONCE PER FRAME.** The caller records the key before spawning, so the very
/// next frame — with the chart still empty, because the read is in flight — plans nothing.
/// Two windows on the same series are one read in the same plan, for the same reason.
#[test]
fn a_key_is_asked_once_across_frames_and_once_within_a_frame() {
    let targets =
        [target("bybit", "BTCUSDT", "5m", false), target("bybit", "BTCUSDT", "5m", false)];
    let plan = plan_store_reads(&targets, &[], &HashSet::new());
    assert_eq!(plan.len(), 1, "two windows, one series, ONE read");

    let asked: HashSet<String> = plan.iter().map(StoreBarRequest::key).collect();
    for _frame in 0..5 {
        assert!(
            plan_store_reads(&targets, &[], &asked).is_empty(),
            "the chart is still empty while the read flies — it must not be re-asked"
        );
    }
}

/// **0061 Phase 2's first seam, end to end:** a window that knows what kind of instrument it is
/// on hands that fact to the request, and a window that does not hands `None`. Nothing
/// downstream of here reads it yet — the destination is the seed request's optional class — so
/// this is the test that would catch the field being silently dropped again.
#[test]
fn a_windows_class_reaches_the_request_and_an_unclassed_window_carries_none() {
    let targets = [
        classed_target("bybit", "BTCUSDT.P", "5m", false, Some(vike_model::AssetClass::CryptoPerp)),
        target("binance", "BTCUSDT", "5m", false),
    ];
    let plan = plan_store_reads(&targets, &[], &HashSet::new());
    assert_eq!(plan.len(), 2);
    assert_eq!(plan[0].class, Some(vike_model::AssetClass::CryptoPerp));
    assert_eq!(plan[1].class, None, "a window with no class claims none");
}

/// ⚠ **The collision the ONCE rule forces, answered explicitly.** [`StoreBarRequest::key`] does
/// not carry the class (its doc argues why), so two windows on ONE `(venue, symbol, interval)`
/// that disagree about the class still produce ONE read — and the survivor is the FIRST in
/// `targets` order rather than a refusal or a `None`.
///
/// This is a behaviour question rather than an implementation detail, which is why it is pinned
/// here rather than left to be discovered: a reader who assumed the class refined the key would
/// expect two reads, and a reader who assumed a disagreement was dropped would expect `None`.
#[test]
fn two_windows_on_one_series_with_different_classes_ask_once_and_the_first_wins() {
    let targets = [
        classed_target("bybit", "BTCUSD", "5m", false, Some(vike_model::AssetClass::CryptoSpot)),
        classed_target("bybit", "BTCUSD", "5m", false, Some(vike_model::AssetClass::CryptoPerp)),
    ];
    let plan = plan_store_reads(&targets, &[], &HashSet::new());
    assert_eq!(plan.len(), 1, "one store partition, one read — the class does not split the key");
    assert_eq!(
        plan[0].class,
        Some(vike_model::AssetClass::CryptoSpot),
        "first in `targets` order wins"
    );
    assert_eq!(plan[0].key(), workspace::series_key("bybit", "BTCUSD", "5m"));
}

/// An interval with no parsable bar width is refused rather than turned into an unbounded scan
/// of the operator's store.
#[test]
fn an_unparsable_interval_is_refused_rather_than_read_unbounded() {
    assert!(read_range("nonsense", 0).is_none());
    assert!(
        plan_store_reads(&[target("bybit", "X", "nonsense", false)], &[], &HashSet::new())
            .is_empty()
    );
    // …and a real one is a BOUNDED window ending now.
    let r = read_range("5m", 10_000_000_000).expect("5m parses");
    assert_eq!(r.end, Some(10_000_000_000));
    assert_eq!(r.start, Some(10_000_000_000 - 300_000 * STORE_READ_BARS));
}

/// THE READ, end to end over a REAL [`vike_data::HistStore`]: a store holding 5m bars seeds the
/// chart's series, and the fold's dirty flag moves so the next frame paints it.
#[test]
fn a_store_holding_the_interval_seeds_the_chart() {
    let store = MemHistStore::default();
    // ⚠ The rows end AT `now`, walking BACKWARD — [`read_range`] asks for the last
    // `STORE_READ_BARS` bars ENDING there, so rows stamped in this store's future are out of
    // range and land nothing. (They did, on the first run of this test: 1 of 4.)
    let rows: Vec<vike_model::Bar> = (1..=4).map(|i| bar(NOW_MS - (5 - i) * 300_000)).collect();
    store.append_bars("bybit", "BTCUSDT", "5m", &rows, None).expect("seed the store");

    let bars = DirectBarStore::default();
    let before = bars.generation();
    let reqs = [StoreBarRequest {
        venue: "bybit".into(),
        symbol: "BTCUSDT".into(),
        interval: "5m".into(),
        class: None,
    }];
    assert_eq!(read_into_store(&reqs, &store, &bars, NOW_MS).landed, 1);

    let (closed, _forming) = bars.series("bybit", "BTCUSDT", "5m").expect("the series landed");
    assert_eq!(closed.len(), 4, "THE DEFECT: a 5m chart can paint from the backend's store");
    assert_ne!(bars.generation(), before, "the fold's dirty flag must move");
}

/// ⚠ …and the RANGE is a real bound rather than decoration: a row older than
/// [`STORE_READ_BARS`] bars back is outside the window and does not land. Without this, a
/// `read_range` that silently widened to unbounded would pass every other test here.
#[test]
fn a_row_older_than_the_window_is_outside_the_read() {
    let store = MemHistStore::default();
    let inside = bar(NOW_MS - 300_000);
    let outside = bar(NOW_MS - 300_000 * (STORE_READ_BARS + 10));
    store.append_bars("bybit", "BTCUSDT", "5m", &[outside, inside], None).expect("seed the store");
    let bars = DirectBarStore::default();
    let reqs = [StoreBarRequest {
        venue: "bybit".into(),
        symbol: "BTCUSDT".into(),
        interval: "5m".into(),
        class: None,
    }];
    read_into_store(&reqs, &store, &bars, NOW_MS);
    let (closed, _) = bars.series("bybit", "BTCUSDT", "5m").expect("the series landed");
    assert_eq!(closed.len(), 1, "only the row inside the bounded window");
    assert_eq!(closed[0].ts, NOW_MS - 300_000);
}

/// A store that holds NOTHING for the key lands nothing — no empty series, no generation bump
/// — so the badge and the empty-plot hint keep saying what is true.
#[test]
fn an_empty_answer_lands_nothing_rather_than_an_empty_series() {
    let store = MemHistStore::default();
    let bars = DirectBarStore::default();
    let before = bars.generation();
    let reqs = [StoreBarRequest {
        venue: "bybit".into(),
        symbol: "BTCUSDT".into(),
        interval: "5m".into(),
        class: None,
    }];
    let out = read_into_store(&reqs, &store, &bars, NOW_MS);
    assert_eq!(out.landed, 0);
    assert!(bars.keys().is_empty(), "no phantom series");
    assert_eq!(bars.generation(), before, "and no wasted refold");
    // ...and the series is REPORTED as empty, which is the gap arm's whole input: "the store
    // holds no 5m rows for this symbol" is the one outcome a caller can act on
    // (`crate::data::chart_seed`). Without this the seed path would have nothing to plan from.
    assert_eq!(out.empty, reqs.to_vec(), "the empty series is reported, in request order");
}

/// A SKIPPED request does not cost the batch: the request the store cannot answer is passed
/// over and the next one still lands. (`MemHistStore` answers an unknown series with an honest
/// empty vec — the same skip path a real `Err` takes, which is why this states the batch
/// property rather than the error one.)
#[test]
fn a_skipped_read_does_not_cost_the_rest_of_the_batch() {
    let store = MemHistStore::default();
    store.append_bars("bybit", "BTCUSDT", "5m", &[bar(NOW_MS - 300_000)], None).expect("seed");
    let bars = DirectBarStore::default();
    let reqs = [
        StoreBarRequest {
            venue: "bybit".into(),
            symbol: "NOSUCH".into(),
            interval: "5m".into(),
            class: None,
        },
        StoreBarRequest {
            venue: "bybit".into(),
            symbol: "BTCUSDT".into(),
            interval: "5m".into(),
            class: None,
        },
    ];
    assert_eq!(read_into_store(&reqs, &store, &bars, NOW_MS).landed, 1);
    assert!(bars.series("bybit", "BTCUSDT", "5m").is_some(), "the second request still ran");
    assert!(bars.series("bybit", "NOSUCH", "5m").is_none(), "…and the first landed nothing");
}

/// The spawned form does the same work off-thread and wakes the GUI exactly once.
#[test]
fn the_spawned_read_seeds_and_wakes() {
    let store = MemHistStore::default();
    store.append_bars("bybit", "BTCUSDT", "15m", &[bar(NOW_MS - 300_000)], None).expect("seed");
    let bars = std::sync::Arc::new(DirectBarStore::default());
    let woke = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let w = std::sync::Arc::clone(&woke);
    spawn_store_seed(
        vec![StoreBarRequest {
            venue: "bybit".into(),
            symbol: "BTCUSDT".into(),
            interval: "15m".into(),
            class: None,
        }],
        store,
        std::sync::Arc::clone(&bars),
        NOW_MS,
        // No gap arm: this test is about the READ, and `None` is byte-identical to the
        // behaviour before one existed. The gap arm's own tests live in `crate::data::chart_seed`
        // and in `crates/vike-datahub/tests/seed_series.rs`, where a server can answer.
        None,
        move || {
            w.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        },
    )
    .join()
    .expect("the read thread");
    assert!(bars.series("bybit", "BTCUSDT", "15m").is_some());
    assert_eq!(woke.load(std::sync::atomic::Ordering::SeqCst), 1, "ONE repaint for the batch");
}
