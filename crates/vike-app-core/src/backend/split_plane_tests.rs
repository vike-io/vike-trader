use super::*;
use crate::ui::workspace::series_key;

/// The whole arm-decision table, all four `(build, flag)` combinations: fat+observe is the
/// third mode (feeds ON), thin+observe the feed-less observer (feeds OFF), fat local the
/// unchanged pre-B2 arm (feeds ON), and thin without `--observe` is refused before `App::new`.
///
/// ⚠ Only the thin (`false`) rows are reachable from a launch since the `fat` build was deleted on
/// 2026-09-09 (#1727) — `crates/vike-desktop/src/main.rs`'s `main` passes a literal `false` — so
/// these tables pin the functions, which still encode all four rows, not four live builds.
#[test]
fn the_arm_decision_table_covers_all_four_combinations() {
    assert_eq!(app_mode(true, false), Some(AppMode::LocalCore));
    assert_eq!(app_mode(true, true), Some(AppMode::ObserveWithFeeds));
    assert_eq!(app_mode(false, true), Some(AppMode::ObserveOnly));
    assert_eq!(app_mode(false, false), None, "thin without --observe: refused in main()");

    assert!(feeds_mount(AppMode::LocalCore), "fat local mounts feeds — unchanged");
    assert!(feeds_mount(AppMode::ObserveWithFeeds), "the third mode mounts feeds");
    assert!(!feeds_mount(AppMode::ObserveOnly), "thin observe stays feed-less");
}

/// ⚠ THE ARM TABLE, DRIVEN FROM THE RAW STARTUP FACTS — the pairing the 2026-09-06 P1 broke.
/// `observes` is what turns "was observing requested" into the `observing` argument above, and
/// the row that matters is the FIRST one: a fat build that requested nothing composes
/// [`AppMode::LocalCore`], i.e. `App::new` took the `else` arm, mounted a local trading core and
/// dispatched orders to it (while a `fat` build existed). The three others pin that the fix cannot
/// regress the other way.
///
/// The thin rows are why `observes` takes `fat_build` at all: a thin build has no core, so it
/// observes whatever argv said, and `app_mode`'s refused row stays unreachable from a launch.
#[test]
fn the_raw_startup_facts_compose_the_same_arm_table() {
    assert_eq!(
        app_mode(true, observes(true, false)),
        Some(AppMode::LocalCore),
        "fat + nothing requested MUST keep the local core — the #1610 regression"
    );
    assert_eq!(app_mode(true, observes(true, true)), Some(AppMode::ObserveWithFeeds));
    assert_eq!(app_mode(false, observes(false, true)), Some(AppMode::ObserveOnly));
    assert_eq!(
        app_mode(false, observes(false, false)),
        Some(AppMode::ObserveOnly),
        "a thin build observes even unasked: it has no local core, and refusing to start is \
             the behaviour #1610 removed"
    );
    assert!(!observes(true, false), "the ONE combination that is not observing");
    for requested in [false, true] {
        assert!(observes(false, requested), "a thin build always observes");
        assert!(
            app_mode(true, observes(true, requested)).is_some()
                && app_mode(false, observes(false, requested)).is_some(),
            "no real launch can reach the refused row"
        );
    }
}

/// The third mode's feed set IS the fat arm's — the same `&'static` slice, one authority,
/// never a second copy — and the feed-less observer mounts nothing.
#[test]
fn the_third_mode_mounts_exactly_the_fat_arms_feed_set() {
    assert_eq!(feed_venues(AppMode::ObserveWithFeeds), feed_venues(AppMode::LocalCore));
    assert_eq!(feed_venues(AppMode::ObserveWithFeeds), LOCAL_FEED_VENUES);
    assert!(feed_venues(AppMode::ObserveOnly).is_empty());
}

/// The double-fold guard is TOTAL and single-valued: for EVERY `(direct_bars, venue,
/// interval)` triple exactly one source answers — so a series present in `snap.bars`, the
/// local tick path AND the direct-bar store still has exactly one render source, decided
/// here. Tick/volume intervals are tape-rendered in every mode on every venue; kline
/// intervals (and the unrecognized fallback, matching `BarKind::parse`'s kline fallback)
/// split on `(direct_bars, venue)` and never on anything else.
#[test]
fn every_interval_has_exactly_one_render_source() {
    let venues = ["binance", "bybit", "okx", "aster", "hyperliquid", "polymarket", "deribit", "ig"];
    for plane in [BarPlane::None, BarPlane::VenueFeeds, BarPlane::BackendStore] {
        for venue in venues {
            for kline in ["1m", "5m", "1h", "1d", "weird"] {
                for live in [false, true] {
                    let expect = match plane {
                        BarPlane::None => SeriesSource::SnapshotBars,
                        BarPlane::VenueFeeds if venue_serves_direct_bars(venue) => {
                            SeriesSource::DirectBars
                        }
                        BarPlane::VenueFeeds => SeriesSource::SnapshotBars,
                        BarPlane::BackendStore if live => SeriesSource::SnapshotBars,
                        BarPlane::BackendStore => SeriesSource::DirectBars,
                    };
                    assert_eq!(
                        series_render_source(plane, venue, kline, live),
                        expect,
                        "kline {venue}@{kline} plane={plane:?} published_live={live}"
                    );
                }
            }
            for tape in ["100t", "1t", "10v", "2.5v"] {
                for live in [false, true] {
                    assert_eq!(
                        series_render_source(plane, venue, tape, live),
                        SeriesSource::LocalTape,
                        "tape intervals fold from the tape under every plane ({venue}@{tape})"
                    );
                }
            }
        }
    }
}

/// ⚠ THE MODE SIGNAL, stated as the property it replaced. Under
/// [`BarPlane::BackendStore`] the store is claimed by "the backend does not publish this
/// series", NOT by [`DIRECT_BAR_VENUES`] — so a 5m chart on ANY venue reaches the store, and a
/// 1m chart the daemon IS streaming keeps folding from the snapshot on a `DIRECT_BAR_VENUES`
/// venue and off it alike. A regression to the venue predicate fails both halves.
#[test]
fn the_backend_store_plane_keys_on_publication_not_on_the_venue() {
    for venue in ["binance", "bybit", "okx", "aster", "hyperliquid", "polymarket", "deribit"] {
        assert_eq!(
            series_render_source(BarPlane::BackendStore, venue, "5m", false),
            SeriesSource::DirectBars,
            "{venue}: an unpublished kline must reach the backend's store, venue regardless"
        );
        assert_eq!(
            series_render_source(BarPlane::BackendStore, venue, "1m", true),
            SeriesSource::SnapshotBars,
            "{venue}: a PUBLISHED series still folds from the live snapshot — no regression"
        );
    }
    // ...and the VenueFeeds plane is untouched by publication, which is what makes the two
    // planes different answers rather than one answer with a new spelling.
    for live in [false, true] {
        assert_eq!(
            series_render_source(BarPlane::VenueFeeds, "binance", "1m", live),
            SeriesSource::DirectBars
        );
        assert_eq!(
            series_render_source(BarPlane::VenueFeeds, "deribit", "1m", live),
            SeriesSource::SnapshotBars
        );
    }
}

/// The plane arm table, and the ONE row the store-read path changed.
#[test]
fn the_bar_plane_arm_table_names_one_filler_per_mode() {
    assert_eq!(bar_plane(AppMode::LocalCore), BarPlane::None, "the core owns klines");
    assert_eq!(bar_plane(AppMode::ObserveWithFeeds), BarPlane::VenueFeeds);
    assert_eq!(
        bar_plane(AppMode::ObserveOnly),
        BarPlane::BackendStore,
        "the shipped desktop: the store is the BACKEND's, read once per unpublished series"
    );
    // `direct_bars_mount` is DERIVED from this, so the handle and the plane cannot disagree.
    for mode in [AppMode::LocalCore, AppMode::ObserveWithFeeds, AppMode::ObserveOnly] {
        assert_eq!(direct_bars_mount(mode), bar_plane(mode) != BarPlane::None);
    }
}

/// The third source exists ONLY where a plane fills the store: under [`BarPlane::None`] NO
/// tuple answers `DirectBars` — the fat local arm is unchanged by construction — and under
/// [`BarPlane::VenueFeeds`] a venue without a bar feed (polymarket: `subscribe_bars` refuses)
/// or outside the local feed set (deribit) still keeps the snapshot tail.
#[test]
fn direct_bars_appear_only_under_a_plane_that_fills_the_store() {
    for venue in ["binance", "bybit", "okx", "aster", "hyperliquid", "polymarket", "deribit"] {
        for interval in ["1m", "1h", "100t", "10v", "weird"] {
            for live in [false, true] {
                assert_ne!(
                    series_render_source(BarPlane::None, venue, interval, live),
                    SeriesSource::DirectBars,
                    "no plane: no series may render DirectBars ({venue}@{interval})"
                );
            }
        }
    }
    for venue in DIRECT_BAR_VENUES {
        assert_eq!(
            series_render_source(BarPlane::VenueFeeds, venue, "1m", false),
            SeriesSource::DirectBars
        );
    }
    assert_eq!(
        series_render_source(BarPlane::VenueFeeds, "polymarket", "1m", false),
        SeriesSource::SnapshotBars,
        "polymarket's feed refuses subscribe_bars — its klines keep the backend tail"
    );
    assert_eq!(
        series_render_source(BarPlane::VenueFeeds, "deribit", "1m", false),
        SeriesSource::SnapshotBars,
        "a venue with no local feed at all keeps the backend tail"
    );
}

/// The store-mount arm table ([`direct_bars_mount`]) and the `DIRECT_BAR_VENUES` subset rule.
/// ⚠ The `ObserveOnly` row FLIPPED with the store-read path — it used to read "thin observe has
/// no local feeds, so no store"; it now mounts one the BACKEND fills, which is a different
/// claim about a different filler and is why [`bar_plane`] exists.
#[test]
fn the_store_is_mounted_by_every_mode_that_has_a_filler() {
    assert!(!direct_bars_mount(AppMode::LocalCore), "fat local: the core owns klines");
    assert!(direct_bars_mount(AppMode::ObserveWithFeeds), "the third mode: venue feeds fill it");
    assert!(direct_bars_mount(AppMode::ObserveOnly), "the desktop: the backend's store fills it");
    for venue in DIRECT_BAR_VENUES {
        assert!(
            LOCAL_FEED_VENUES.contains(&venue),
            "{venue}: DIRECT_BAR_VENUES must stay a subset of LOCAL_FEED_VENUES"
        );
    }
}

/// The key classifier agrees with the interval classifier across both `series_key` shapes
/// (bare Binance and venue-prefixed) and every plane, and classifies a shape `series_key`
/// cannot produce as snapshot-rendered under all of them — no local fold exists for an unknown
/// shape, so clearing on switch stays the safe direction.
#[test]
fn key_classification_matches_the_interval_classifier_for_both_key_shapes() {
    // No plane (fat local): every kline key is snapshot-rendered.
    assert!(folds_from_snapshot(BarPlane::None, &series_key("binance", "BTCUSDT", "1m")));
    assert!(folds_from_snapshot(BarPlane::None, &series_key("okx", "BTC-USDT", "1h")));
    assert!(folds_from_snapshot(BarPlane::None, &series_key("polymarket", "1071", "1m")));
    // Venue feeds (the third mode): a direct-bar venue's kline key is venue truth…
    let vf = BarPlane::VenueFeeds;
    assert!(!folds_from_snapshot(vf, &series_key("binance", "BTCUSDT", "1m")));
    assert!(!folds_from_snapshot(vf, &series_key("okx", "BTC-USDT", "1h")));
    assert!(!folds_from_snapshot(vf, &series_key("hyperliquid", "BTC", "1m")));
    // …while a bar-less venue's stays backend-session state.
    assert!(folds_from_snapshot(vf, &series_key("polymarket", "1071", "1m")));
    assert!(folds_from_snapshot(vf, &series_key("deribit", "BTC-PERPETUAL", "1h")));
    // ⚠ The backend-store plane: EVERY kline key is backend-session state, published or not —
    // both sources are this backend, so a switch must clear all of them. See the fn doc.
    let bs = BarPlane::BackendStore;
    for key in ["BTCUSDT@1m", "bybit:BTCUSDT@5m", "deribit:BTC-PERPETUAL@1h", "okx:X@1d"] {
        assert!(folds_from_snapshot(bs, key), "{key}: store-read bars are backend-session");
    }
    // Tape keys are venue truth under every plane.
    for plane in [BarPlane::None, vf, bs] {
        assert!(!folds_from_snapshot(plane, &series_key("binance", "BTCUSDT", "100t")));
        assert!(!folds_from_snapshot(plane, &series_key("okx", "BTC-USDT", "10v")));
        assert!(
            folds_from_snapshot(plane, "no-interval-separator"),
            "unknown shapes clear, never linger (plane={plane:?})"
        );
    }
}
