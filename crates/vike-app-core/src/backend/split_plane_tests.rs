use super::*;
use crate::ui::workspace::series_key;

/// ⚠ THE RAW STARTUP FACTS — the pairing the 2026-09-06 P1 broke. `observes` turns "was observing
/// requested" into whether a launch observes, and the row that matters is the FIRST one: a fat
/// build that requested nothing does NOT observe, i.e. `App::new` took the `else` arm, mounted a
/// local trading core and dispatched orders to it (while a `fat` build existed). The others pin
/// that the fix cannot regress the other way.
///
/// The thin rows are why `observes` takes `fat_build` at all: a thin build has no core, so it
/// observes whatever argv said.
#[test]
fn the_raw_startup_facts_decide_whether_a_launch_observes() {
    assert!(
        !observes(true, false),
        "fat + nothing requested MUST keep the local core — the #1610 regression"
    );
    assert!(observes(true, true), "fat + --observe observes");
    assert!(
        observes(false, false),
        "a thin build observes even unasked: it has no local core, and refusing to start is \
             the behaviour #1610 removed"
    );
    for requested in [false, true] {
        assert!(observes(false, requested), "a thin build always observes");
    }
}

/// The double-fold guard is TOTAL and single-valued: for EVERY `(plane, interval,
/// published_live)` triple exactly one source answers — so a series present in `snap.bars`, the
/// local tick path AND the bar store still has exactly one render source, decided here.
/// Tick/volume intervals are tape-rendered under every plane; kline intervals (and the
/// unrecognized fallback, matching `BarKind::parse`'s kline fallback) split on `(plane,
/// published_live)` and never on anything else.
#[test]
fn every_interval_has_exactly_one_render_source() {
    for plane in [BarPlane::None, BarPlane::BackendStore] {
        for kline in ["1m", "5m", "1h", "1d", "weird"] {
            for live in [false, true] {
                let expect = match plane {
                    BarPlane::None => SeriesSource::SnapshotBars,
                    BarPlane::BackendStore if live => SeriesSource::SnapshotBars,
                    BarPlane::BackendStore => SeriesSource::DirectBars,
                };
                assert_eq!(
                    series_render_source(plane, kline, live),
                    expect,
                    "kline @{kline} plane={plane:?} published_live={live}"
                );
            }
        }
        for tape in ["100t", "1t", "10v", "2.5v"] {
            for live in [false, true] {
                assert_eq!(
                    series_render_source(plane, tape, live),
                    SeriesSource::LocalTape,
                    "tape intervals fold from the tape under every plane (@{tape})"
                );
            }
        }
    }
}

/// ⚠ THE MODE SIGNAL, stated as the property it replaced. Under [`BarPlane::BackendStore`] the
/// store is claimed by "the backend does not publish this series" and by nothing about the
/// venue: [`series_render_source`] takes no venue at all, so a 5m chart on ANY venue reaches the
/// store, and a 1m chart the daemon IS streaming keeps folding from the snapshot on any venue.
/// The chart-level halves (the fold keeps a published binance series on the snapshot and a
/// deribit 5m one on the store) are `direct_bar_tests`'.
#[test]
fn the_backend_store_plane_keys_on_publication_alone() {
    assert_eq!(
        series_render_source(BarPlane::BackendStore, "5m", false),
        SeriesSource::DirectBars,
        "an unpublished kline must reach the backend's store"
    );
    assert_eq!(
        series_render_source(BarPlane::BackendStore, "1m", true),
        SeriesSource::SnapshotBars,
        "a PUBLISHED series still folds from the live snapshot"
    );
}

/// The plane arm table: the shipped desktop's store is the BACKEND's, read once per unpublished
/// series.
#[test]
fn the_bar_plane_arm_table_names_one_filler_per_mode() {
    assert_eq!(
        bar_plane(AppMode::ObserveOnly),
        BarPlane::BackendStore,
        "the shipped desktop: the store is the BACKEND's, read once per unpublished series"
    );
}

/// The third source exists ONLY where a plane fills the store: under [`BarPlane::None`] NO
/// tuple answers `DirectBars`.
#[test]
fn direct_bars_appear_only_under_a_plane_that_fills_the_store() {
    for interval in ["1m", "1h", "100t", "10v", "weird"] {
        for live in [false, true] {
            assert_ne!(
                series_render_source(BarPlane::None, interval, live),
                SeriesSource::DirectBars,
                "no plane: no series may render DirectBars (@{interval})"
            );
        }
    }
}

/// The key classifier agrees with the interval classifier across both `series_key` shapes
/// (bare Binance and venue-prefixed) and every plane, and classifies a shape `series_key`
/// cannot produce as snapshot-rendered under all of them — no local fold exists for an unknown
/// shape, so clearing on switch stays the safe direction.
#[test]
fn key_classification_matches_the_interval_classifier_for_both_key_shapes() {
    // No plane: every kline key is snapshot-rendered.
    assert!(folds_from_snapshot(BarPlane::None, &series_key("binance", "BTCUSDT", "1m")));
    assert!(folds_from_snapshot(BarPlane::None, &series_key("okx", "BTC-USDT", "1h")));
    assert!(folds_from_snapshot(BarPlane::None, &series_key("polymarket", "1071", "1m")));
    // ⚠ The backend-store plane: EVERY kline key is backend-session state, published or not —
    // both sources are this backend, so a switch must clear all of them. See the fn doc.
    let bs = BarPlane::BackendStore;
    for key in ["BTCUSDT@1m", "bybit:BTCUSDT@5m", "deribit:BTC-PERPETUAL@1h", "okx:X@1d"] {
        assert!(folds_from_snapshot(bs, key), "{key}: store-read bars are backend-session");
    }
    // Tape keys are venue truth under every plane.
    for plane in [BarPlane::None, bs] {
        assert!(!folds_from_snapshot(plane, &series_key("binance", "BTCUSDT", "100t")));
        assert!(!folds_from_snapshot(plane, &series_key("okx", "BTC-USDT", "10v")));
        assert!(
            folds_from_snapshot(plane, "no-interval-separator"),
            "unknown shapes clear, never linger (plane={plane:?})"
        );
    }
}
