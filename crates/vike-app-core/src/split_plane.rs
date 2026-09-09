//! `split_plane` — the pure ARM decisions behind `vike-app`'s `App::new` (split-plane B2, the
//! third mode).
//!
//! The split-plane spec (`docs/superpowers/specs/2026-08-18-split-plane-client-backend-design.md`
//! §1) divides the client's data into two planes with different owners: the **account plane**
//! (orders, positions, equity, kline history) is the backend's — it arrives as `WireSnapshot`s
//! over the observe bridge — while the **market-data plane** (DOM books, trade tape, tick/volume
//! and orderflow series) is the venue's, subscribed directly and keylessly by the client. Before
//! B2 the two planes were mutually exclusive BY CONSTRUCTION: `App::new`'s observe arm hard-coded
//! `core: None` + empty feeds, and its fat arm mounted feeds only alongside a local core. The
//! third mode is both at once — observe a remote backend AND run the local direct-to-venue feeds
//! in one process.
//!
//! `vike-app`'s `main.rs` is CI-excluded (spec §2 principle 4), so every DECISION lives here as a
//! pure function and `main.rs` keeps only the wiring:
//!
//! - [`app_mode`] — which of the three arms a `(build, --observe)` pair selects. `main.rs`'s
//!   `#[cfg(feature = "fat")]` / `observe_addr` branch structure is this function's wiring; the
//!   table is pinned HERE so the two cannot drift silently.
//! - [`feed_venues`] / [`LOCAL_FEED_VENUES`] — which venues the local market-data plane mounts.
//!   ONE authority: the fat arm and the third mode both build their feed map from the same
//!   construction helper (`build_local_feeds` in `main.rs`), which `debug_assert!`s its keys
//!   against this list — so the third mode's feed set equals the fat arm's by construction, and
//!   this list is the CI-testable pin of both.
//! - [`series_render_source`] / [`folds_from_snapshot`] — THE DOUBLE-FOLD GUARD: which single
//!   source renders a series. Kline series fold from `snap.bars` (filled by the local core, or by
//!   the backend's streamed bar tail) — EXCEPT in the third mode on a venue whose local feed
//!   serves a bar lane, where they fold from the GUI-side [`data_sink::DirectBarStore`]
//!   (crate::data_sink::DirectBarStore) instead; tick/volume/orderflow series fold from the
//!   GUI-side trade tape in every mode. A series must never fold from two sources — that is the
//!   bug class `backend_conn::clear_session_state`'s clear list exists for — so
//!   [`core_sync`](crate::core_sync)'s snapshot fold AND its direct-bar fold each consult this
//!   function before touching a chart, and the backend-switch clear uses [`folds_from_snapshot`]
//!   to decide which chart entries are backend-session state.
//!
//! # The direct-bar path (the v1 kline residual, closed)
//!
//! v1 shipped with a documented residual here: the third mode's kline windows rendered the
//! BACKEND's streamed bar tail (`WireSnapshot::bars`, ~300 bars per series) because the GUI had
//! no core-free bar store — [`data_sink::GuiFeedSink`](crate::data_sink::GuiFeedSink) dropped
//! its bar lanes in explicit no-ops. That store now exists
//! ([`data_sink::DirectBarStore`](crate::data_sink::DirectBarStore)): the local bar
//! subscriptions the uniform `ensure_feed_on` cadence opens land their seed/close/forming lanes
//! there, and in the third mode a kline series on a [`DIRECT_BAR_VENUES`] venue renders from it
//! — venue-direct, [`SeriesSource::DirectBars`] — while every other kline series (a venue whose
//! feed serves no bar lane, e.g. polymarket, or one the client mounts no feed for at all) keeps
//! the backend tail. History comes from the venue's own REST-warmup seed where one exists (the
//! same lane that seeds the fat arm's core cache), with the backend tail offered as a
//! ONE-TIME seed only while a series holds no closed bars (`DirectBarStore::seed_backend_tail` —
//! hyperliquid's candle pump is live-only, so without it those charts would start empty); the
//! store's boundary-ts dedup rule keeps the seam single-painted. Tick/volume, orderflow, the
//! trade tape and the DOM books are fully client-direct in the third mode
//! ([`SeriesSource::LocalTape`] and the `BookStore` path carry no core dependency at all).

use crate::tickvol::BarKind;

/// The three ways `App::new` can compose (split-plane §5 B2). Selected once at startup — there is
/// deliberately NO runtime path between the local-core arm and the observe arms (mode is a CLI +
/// build-feature fact), so "leaving observe mode" happens only at process exit, where `on_exit`'s
/// bounded teardown shuts every feed down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppMode {
    /// Fat build, no `--observe`: local trading core + local venue feeds (pre-B2, unchanged).
    LocalCore,
    /// Fat build + `--observe ADDR` — **the third mode**: the account plane observed from a
    /// remote backend (orders dispatch through the `Scope::Control` channel, `snap_cell` fed by
    /// the observe bridge), the market-data plane mounted locally (the same keyless
    /// direct-to-venue feeds the fat arm runs). No local core, no exec engines, no recorder, no
    /// recon; `live_venues` stays empty.
    ObserveWithFeeds,
    /// Thin build + `--observe ADDR`: the feed-less observer. The venue bridges are not in the
    /// thin build's dependency graph at all, so this arm CANNOT mount feeds — a per-bridge
    /// `feeds` feature that would let a thin client subscribe without linking the order signers
    /// is the spec's §7 Phase-5 hardening item, a separate program.
    ObserveOnly,
}

/// The arm-decision table: `(fat build?, --observe given?)` → the mode `App::new` composes.
/// `None` = refused before `App::new` (a thin build without `--observe` has nothing to show;
/// `main()` exits with guidance). `main.rs`'s `if let Some(observe_addr)` + `#[cfg(feature =
/// "fat")]` structure is this table's wiring and `debug_assert!`s agreement per arm.
pub fn app_mode(fat_build: bool, observing: bool) -> Option<AppMode> {
    match (fat_build, observing) {
        (true, false) => Some(AppMode::LocalCore),
        (true, true) => Some(AppMode::ObserveWithFeeds),
        (false, true) => Some(AppMode::ObserveOnly),
        (false, false) => None,
    }
}

/// **Was observing actually REQUESTED** — the `observing` argument [`app_mode`] takes, computed from
/// the two RAW startup facts a binary owns. A FAT build observes only when somebody asked for it
/// ([`backend_conn::StartupObserve::requested`](crate::backend_conn::StartupObserve::requested): the
/// `--observe` word, or an active registry record); a THIN build ALWAYS observes, because it has no
/// local core to fall back on — which is also what keeps [`app_mode`]'s one refused row
/// (`(false, false)`, a thin build with nothing to show) unreachable from a real launch.
///
/// # ⚠ Why this is a SECOND predicate rather than "is there an address"
///
/// [`backend_conn::startup_backend_from`](crate::backend_conn::startup_backend_from)'s ladder has
/// no `None` rung, deliberately: the Connections UI needs an address to show, so a bare launch still
/// resolves [`backend_registry::DEFAULT_OBSERVE_ADDR`](crate::backend_registry::DEFAULT_OBSERVE_ADDR).
/// `crates/vike-app/src/main.rs`'s `observe_backend` used that always-`Some` answer AS the mode, so
/// from #1610 (2026-09-03) every ordinary desktop launch fell through to the local default and took
/// the OBSERVER arm: `core: None`, no exec engines, no recorder, no recon driver, and
/// `crates/vike-app/src/app_ui.rs`'s `dispatch` a read-only no-op — the GUI could not place an order
/// at all. **Which address to watch and whether to watch at all are different questions**; the
/// ladder answers the first and this answers the second.
pub fn observes(fat_build: bool, observe_requested: bool) -> bool {
    !fat_build || observe_requested
}

/// Does this mode mount the local direct-to-venue market-data plane?
pub fn feeds_mount(mode: AppMode) -> bool {
    !matches!(mode, AppMode::ObserveOnly)
}

/// The venues whose market-data feeds the GUI mounts when [`feeds_mount`] says yes — the ONE
/// written-down copy of the set `build_local_feeds` (in `vike-app`'s `main.rs`, the shared
/// construction both feeds-mounted arms call) constructs, and the list that helper
/// `debug_assert!`s its map keys against. The strings are the venue slugs the
/// [`feed_lifecycle`](crate::feed_lifecycle) routing resolves against (`FeedMap` keys /
/// [`venue_of_key`](crate::venue_routing::venue_of_key) prefixes).
pub const LOCAL_FEED_VENUES: [&str; 6] =
    ["binance", "bybit", "okx", "aster", "hyperliquid", "polymarket"];

/// Which venues' feeds a mode mounts: [`LOCAL_FEED_VENUES`] wherever feeds mount, nothing in the
/// feed-less observer. Both feeds-mounted arms return the SAME slice — the third mode's feed set
/// matches the fat arm's because there is exactly one list, not two copies.
pub fn feed_venues(mode: AppMode) -> &'static [&'static str] {
    if feeds_mount(mode) { &LOCAL_FEED_VENUES } else { &[] }
}

/// The single source a series renders from — see the module doc's double-fold guard section.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeriesSource {
    /// `snap.bars`, the published `CoreSnapshot`'s bar map — filled by the LOCAL CORE in
    /// [`AppMode::LocalCore`] and by the BACKEND's streamed bar tail in the observe modes. In
    /// the third mode this is the kline source only for series WITHOUT a local bar feed
    /// (a venue outside [`DIRECT_BAR_VENUES`]); the rest are [`SeriesSource::DirectBars`].
    SnapshotBars,
    /// The GUI-side trade tape ([`TradeStore`](crate::data_sink::TradeStore)) — client-direct
    /// wherever feeds mount; structurally empty (so the series renders empty, from one source) in
    /// [`AppMode::ObserveOnly`].
    LocalTape,
    /// The GUI-side direct-bar store ([`DirectBarStore`](crate::data_sink::DirectBarStore)) —
    /// the third mode's kline source for venues whose LOCAL feed serves a bar lane: the venue's
    /// own seed/close/forming stream, no core and no backend tail behind it. Exists only where
    /// the store is mounted ([`direct_bars_mount`]), i.e. never in [`AppMode::LocalCore`] (the
    /// core owns klines there) and never in [`AppMode::ObserveOnly`] (no local feeds at all).
    DirectBars,
}

/// The venues whose LOCAL market-data feed serves a native BAR lane (`subscribe_bars` succeeds
/// and emits seed/close/forming through the sink) — the direct-bar subset of
/// [`LOCAL_FEED_VENUES`], and therefore the venues whose kline series go venue-direct in the
/// third mode. polymarket is deliberately absent: its `Feeds::subscribe_bars` refuses
/// (`"polymarket serves ticks; bars via resample"`), so its kline series keep the backend tail.
pub const DIRECT_BAR_VENUES: [&str; 5] = ["binance", "bybit", "okx", "aster", "hyperliquid"];

/// Does `venue`'s local feed serve a bar lane? (One row per [`DIRECT_BAR_VENUES`] entry.)
pub fn venue_serves_direct_bars(venue: &str) -> bool {
    DIRECT_BAR_VENUES.contains(&venue)
}

/// Does this mode mount the core-free [`DirectBarStore`](crate::data_sink::DirectBarStore)?
/// TRUE only in the third mode: the fat local arm's bar lanes feed the CORE (klines render from
/// `snap.bars`, unchanged), and the feed-less observer has no local bar lane to store. This is
/// the arm-table pin for the store's presence — `App`'s `direct_bars` slot is `Some` exactly
/// when this says so, which is what lets the folds take the store's presence AS the mode signal
/// (`series_render_source`'s `direct_bars` parameter).
pub fn direct_bars_mount(mode: AppMode) -> bool {
    matches!(mode, AppMode::ObserveWithFeeds)
}

/// THE DOUBLE-FOLD GUARD: the one source a series renders from. Total and single-valued — every
/// `(direct_bars, venue, interval)` triple has exactly one source, so no series can ever be
/// assigned two. `direct_bars` is whether the direct-bar store is mounted (structurally: the
/// third mode — [`direct_bars_mount`]); `venue` is the series' venue slug.
///
/// Tick (`"100t"`) and volume (`"10v"`) intervals fold from the local tape in every mode. Kline
/// intervals (and anything unrecognized, which [`BarKind::parse`] treats as a kline) fold from
/// the DIRECT-BAR STORE when it is mounted AND the venue's feed serves bars
/// ([`venue_serves_direct_bars`]), and from the snapshot otherwise.
/// [`core_sync::sync_from_core`](crate::core_sync::sync_from_core) enforces both bar halves:
/// its snapshot fold SKIPS any series this function does not assign to the snapshot, and its
/// direct-bar fold folds only series assigned to the store.
pub fn series_render_source(direct_bars: bool, venue: &str, interval: &str) -> SeriesSource {
    match BarKind::parse(interval) {
        BarKind::Kline(_) => {
            if direct_bars && venue_serves_direct_bars(venue) {
                SeriesSource::DirectBars
            } else {
                SeriesSource::SnapshotBars
            }
        }
        _ => SeriesSource::LocalTape,
    }
}

/// Does this CHART KEY (`"SYM@interval"` / `"venue:SYM@interval"` —
/// [`workspace::series_key`](crate::workspace::series_key)'s two shapes) belong to the
/// snapshot-rendered family? The backend-switch clear
/// ([`backend_conn::clear_session_state`](crate::backend_conn::clear_session_state)) uses this to
/// decide which `charts` entries are backend-session state (snapshot-rendered → backend A's bars,
/// cleared) and which are local-plane state (tape- or direct-bar-rendered → venue truth, kept);
/// its complement ([`backend_conn::teardown_feed_plane`](crate::backend_conn::teardown_feed_plane))
/// uses the same predicate mirrored, so the two stay complementary by construction.
/// `direct_bars` is the store-mounted flag, exactly as in [`series_render_source`]. A key with no
/// `'@'` cannot have been built by `series_key` and is classified snapshot-rendered REGARDLESS of
/// `direct_bars` — no local fold exists for an unknown shape, so clearing it is the safe
/// direction (a stale paint is the bug class; a refold is one frame).
pub fn folds_from_snapshot(direct_bars: bool, key: &str) -> bool {
    let Some((_, interval)) = key.rsplit_once('@') else {
        return true; // not a series_key shape — clear on switch, never linger
    };
    let venue = crate::venue_routing::venue_of_key(key);
    series_render_source(direct_bars, venue, interval) == SeriesSource::SnapshotBars
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::series_key;

    /// The whole arm-decision table, all four `(build, flag)` combinations: fat+observe is the
    /// third mode (feeds ON), thin+observe the feed-less observer (feeds OFF), fat local the
    /// unchanged pre-B2 arm (feeds ON), and thin without `--observe` is refused before `App::new`.
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
    /// [`AppMode::LocalCore`], i.e. `App::new` takes the `else` arm, mounts a local trading core and
    /// dispatches orders to it. The three others pin that the fix cannot regress the other way.
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
        let venues =
            ["binance", "bybit", "okx", "aster", "hyperliquid", "polymarket", "deribit", "ig"];
        for direct in [false, true] {
            for venue in venues {
                for kline in ["1m", "5m", "1h", "1d", "weird"] {
                    let expect = if direct && venue_serves_direct_bars(venue) {
                        SeriesSource::DirectBars
                    } else {
                        SeriesSource::SnapshotBars
                    };
                    assert_eq!(
                        series_render_source(direct, venue, kline),
                        expect,
                        "kline {venue}@{kline} direct_bars={direct}"
                    );
                }
                for tape in ["100t", "1t", "10v", "2.5v"] {
                    assert_eq!(
                        series_render_source(direct, venue, tape),
                        SeriesSource::LocalTape,
                        "tape intervals fold from the tape in every mode ({venue}@{tape})"
                    );
                }
            }
        }
    }

    /// The third source exists ONLY where the store is mounted and the venue's feed serves bars:
    /// with the store unmounted (fat local / thin observe) NO triple answers `DirectBars` — the
    /// pre-existing modes are unchanged by construction — and with it mounted, a venue without a
    /// bar feed (polymarket: `subscribe_bars` refuses) or outside the local feed set (deribit)
    /// still keeps the snapshot tail.
    #[test]
    fn direct_bars_appear_only_with_the_store_mounted_on_a_bar_serving_venue() {
        for venue in ["binance", "bybit", "okx", "aster", "hyperliquid", "polymarket", "deribit"] {
            for interval in ["1m", "1h", "100t", "10v", "weird"] {
                assert_ne!(
                    series_render_source(false, venue, interval),
                    SeriesSource::DirectBars,
                    "unmounted store: no series may render DirectBars ({venue}@{interval})"
                );
            }
        }
        for venue in DIRECT_BAR_VENUES {
            assert_eq!(series_render_source(true, venue, "1m"), SeriesSource::DirectBars);
        }
        assert_eq!(
            series_render_source(true, "polymarket", "1m"),
            SeriesSource::SnapshotBars,
            "polymarket's feed refuses subscribe_bars — its klines keep the backend tail"
        );
        assert_eq!(
            series_render_source(true, "deribit", "1m"),
            SeriesSource::SnapshotBars,
            "a venue with no local feed at all keeps the backend tail"
        );
    }

    /// The store-mount arm table ([`direct_bars_mount`]): mounted ONLY in the third mode. Fat
    /// local keeps its core-owned klines, thin observe keeps its feed-less snapshot rendering —
    /// both byte-identical to pre-direct-bars — and the direct-bar subset is a SUBSET of the
    /// mounted feed set (a venue cannot serve direct bars without a mounted feed).
    #[test]
    fn the_direct_bar_store_mounts_only_in_the_third_mode() {
        assert!(!direct_bars_mount(AppMode::LocalCore), "fat local: the core owns klines");
        assert!(direct_bars_mount(AppMode::ObserveWithFeeds), "the third mode mounts the store");
        assert!(!direct_bars_mount(AppMode::ObserveOnly), "thin observe has no local feeds");
        for venue in DIRECT_BAR_VENUES {
            assert!(
                LOCAL_FEED_VENUES.contains(&venue),
                "{venue}: DIRECT_BAR_VENUES must stay a subset of LOCAL_FEED_VENUES"
            );
        }
    }

    /// The key classifier agrees with the interval classifier across both `series_key` shapes
    /// (bare Binance and venue-prefixed) and BOTH store-mount states, and classifies a shape
    /// `series_key` cannot produce as snapshot-rendered under both — no local fold exists for an
    /// unknown shape, so clearing on switch stays the safe direction even in the third mode.
    #[test]
    fn key_classification_matches_the_interval_classifier_for_both_key_shapes() {
        // Store unmounted (fat local / thin observe): every kline key is snapshot-rendered.
        assert!(folds_from_snapshot(false, &series_key("binance", "BTCUSDT", "1m")));
        assert!(folds_from_snapshot(false, &series_key("okx", "BTC-USDT", "1h")));
        assert!(folds_from_snapshot(false, &series_key("polymarket", "1071", "1m")));
        // Store mounted (the third mode): a direct-bar venue's kline key is venue truth…
        assert!(!folds_from_snapshot(true, &series_key("binance", "BTCUSDT", "1m")));
        assert!(!folds_from_snapshot(true, &series_key("okx", "BTC-USDT", "1h")));
        assert!(!folds_from_snapshot(true, &series_key("hyperliquid", "BTC", "1m")));
        // …while a bar-less venue's stays backend-session state.
        assert!(folds_from_snapshot(true, &series_key("polymarket", "1071", "1m")));
        assert!(folds_from_snapshot(true, &series_key("deribit", "BTC-PERPETUAL", "1h")));
        // Tape keys are venue truth under both.
        for direct in [false, true] {
            assert!(!folds_from_snapshot(direct, &series_key("binance", "BTCUSDT", "100t")));
            assert!(!folds_from_snapshot(direct, &series_key("okx", "BTC-USDT", "10v")));
            assert!(
                folds_from_snapshot(direct, "no-interval-separator"),
                "unknown shapes clear, never linger (direct_bars={direct})"
            );
        }
    }
}
