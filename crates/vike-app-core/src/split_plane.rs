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
//! - [`bar_plane`] / [`series_render_source`] / [`folds_from_snapshot`] — THE DOUBLE-FOLD GUARD:
//!   which single source renders a series. Kline series fold from `snap.bars` (filled by the local
//!   core, or by the backend's streamed bar tail) — EXCEPT where a [`BarPlane`] fills the GUI-side
//!   [`data_sink::DirectBarStore`](crate::data_sink::DirectBarStore) and claims that series;
//!   tick/volume/orderflow series fold from the GUI-side trade tape in every mode. A series must
//!   never fold from two sources — that is the bug class `backend_conn::clear_session_state`'s
//!   clear list exists for — so [`core_sync`](crate::core_sync)'s snapshot fold AND its store fold
//!   each consult this function before touching a chart, and the backend-switch clear uses
//!   [`folds_from_snapshot`] to decide which chart entries are backend-session state.
//!
//! ⚠ **The mode signal is [`BarPlane`], not `direct_bars.is_some()`.** It was the store HANDLE's
//! presence until the store-read path landed, which meant the only way to give the desktop a bar
//! store was to declare every kline on a [`DIRECT_BAR_VENUES`] venue venue-direct — see
//! [`BarPlane`]'s own doc, which carries the whole argument.
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
/// `crates/vike-desktop/src/main.rs`'s `observe_backend` used that always-`Some` answer AS the mode, so
/// from #1610 (2026-09-03) every ordinary desktop launch fell through to the local default and took
/// the OBSERVER arm: `core: None`, no exec engines, no recorder, no recon driver, and
/// `crates/vike-desktop/src/app_ui.rs`'s `dispatch` a read-only no-op — the GUI could not place an order
/// at all. **Which address to watch and whether to watch at all are different questions**; the
/// ladder answers the first and this answers the second.
pub fn observes(fat_build: bool, observe_requested: bool) -> bool {
    !fat_build || observe_requested
}

/// Does this mode mount the local direct-to-venue market-data plane?
pub fn feeds_mount(mode: AppMode) -> bool {
    !matches!(mode, AppMode::ObserveOnly)
}

/// **WHAT FILLS the GUI-side bar store** — the render-source decision's MODE SIGNAL, made
/// explicit.
///
/// # ⚠ Why this exists as a type rather than as `direct_bars.is_some()`
///
/// The signal used to be the STORE HANDLE's presence: `core_sync` computed
/// `direct_bars.is_some()` and passed it to [`series_render_source`] as a `bool`, and
/// `backend_conn`'s two retains did the same off their own slots. That conflation was load-bearing
/// in the wrong direction — it made "is there a store" and "what does the store MEAN" the same
/// question, so the only way to give the desktop a bar store at all was to also declare that every
/// kline on a [`DIRECT_BAR_VENUES`] venue is VENUE-DIRECT. `crates/vike-desktop/src/main.rs` says so
/// in its own comment at the `direct_bars` binding: binding it `Some` "would reassign every kline
/// series on the five CEX venues to a store nothing fills". The handle therefore had to stay `None`,
/// and the desktop had no way to hold bars it fetched itself.
///
/// The two facts are now separate: the HANDLE says whether a store exists, this says who fills it
/// and therefore which series may render from it. `core_sync` `debug_assert!`s that the two agree
/// ([`BarPlane::None`] ⟺ no handle), so the pairing is CHECKED where it used to be assumed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BarPlane {
    /// No GUI-side bar store at all: every kline series renders from `snap.bars`. The fat local
    /// arm (the core owns klines) and, before the store-read path, the feed-less observer.
    None,
    /// The store is filled by LOCAL venue feeds — the third mode's
    /// [`data_sink::GuiFeedSink`](crate::data_sink::GuiFeedSink) bar lanes. A kline series on a
    /// venue whose feed serves bars ([`venue_serves_direct_bars`]) is VENUE TRUTH and renders from
    /// the store; every other kline keeps the backend tail. ⚠ Ruling 2 deleted the arm that
    /// composes this (`crates/vike-desktop/src/main.rs`'s feed tombstone), so no shipped binary
    /// selects it today — it is kept because it is a DIFFERENT answer, still exercised by
    /// `core_sync`'s and `backend_conn`'s suites, and deleting it would silently re-conflate the
    /// two meanings the moment a local feed plane comes back.
    VenueFeeds,
    /// The store is filled by ONE-SHOT reads of **the BACKEND'S OWN hist store**
    /// ([`store_bars`](crate::store_bars)) — the desktop asking the datahub for an interval the
    /// daemon does not stream. A kline series renders from the store exactly where the live
    /// snapshot does NOT publish it, and from the snapshot everywhere else.
    ///
    /// ⚠ **The predicate is "does this backend publish this series", NOT
    /// [`DIRECT_BAR_VENUES`]** — and that difference is the whole point of the variant.
    /// `DIRECT_BAR_VENUES` means "venues whose LOCAL feed serves a bar lane", which is a fact about
    /// a market-data socket this process no longer opens; it says nothing about which series the
    /// SERVER's store holds. Keying the store read on it would have claimed the store for every
    /// binance/bybit/okx/aster/hyperliquid kline — including the 1m one the daemon is streaming
    /// live — and left every OTHER venue unable to reach the store at all.
    BackendStore,
}

/// Which plane fills the bar store in each arm — the arm-table pin for [`BarPlane`], the same
/// shape [`feed_venues`] and [`direct_bars_mount`] have.
///
/// ⚠ [`AppMode::ObserveOnly`]'s answer CHANGED: it was `None` (stated as `direct_bars_mount` being
/// false) and is now [`BarPlane::BackendStore`]. That is the one behavioural claim of the
/// store-read path, and it is narrow: a series the backend publishes still folds from the
/// snapshot, byte-identically. Only a series it does NOT publish moves — from "renders nothing"
/// to "renders whatever the backend's store answers with".
/// `const fn` so a composition root can bind it as a `const` beside its arm rather than computing
/// it per frame — which is also what keeps the binary's copy provably equal to this table.
pub const fn bar_plane(mode: AppMode) -> BarPlane {
    match mode {
        AppMode::LocalCore => BarPlane::None,
        AppMode::ObserveWithFeeds => BarPlane::VenueFeeds,
        AppMode::ObserveOnly => BarPlane::BackendStore,
    }
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
    /// The GUI-side bar store ([`DirectBarStore`](crate::data_sink::DirectBarStore)). WHAT fills
    /// it — and therefore which series may claim it — is [`BarPlane`]: under
    /// [`BarPlane::VenueFeeds`] it is the venue's own seed/close/forming stream (the third mode's
    /// kline source on a [`DIRECT_BAR_VENUES`] venue); under [`BarPlane::BackendStore`] it is the
    /// backend's HIST STORE, read once per series the live snapshot does not publish. Never
    /// reachable under [`BarPlane::None`].
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
/// DERIVED from [`bar_plane`] rather than restated, so the HANDLE's presence and the PLANE cannot
/// answer differently: `App`'s `direct_bars` slot is `Some` exactly when this says so, and
/// `core_sync` `debug_assert!`s that pairing at the fold.
pub fn direct_bars_mount(mode: AppMode) -> bool {
    !matches!(bar_plane(mode), BarPlane::None)
}

/// THE DOUBLE-FOLD GUARD: the one source a series renders from. Total and single-valued — every
/// `(plane, venue, interval, published_live)` tuple has exactly one source, so no series can ever
/// be assigned two.
///
/// * `plane` — [`BarPlane`], WHAT fills the bar store. Not the store's presence: see that type's
///   doc for why the two were split.
/// * `venue` — the series' venue slug.
/// * `published_live` — **does the BACKEND's live snapshot carry this exact series THIS frame**
///   (`snap.bars` holds the `(venue, symbol, interval)` key). Read only under
///   [`BarPlane::BackendStore`], where it is the whole rule; ignored under the other two, whose
///   answers are static facts about the mode and the venue. The caller must not guess it:
///   [`core_sync::sync_from_core`](crate::core_sync::sync_from_core)'s snapshot fold is ITERATING
///   `snap.bars`, so it passes `true` by construction, and its store fold probes the same map.
///
/// Tick (`"100t"`) and volume (`"10v"`) intervals fold from the local tape under every plane.
/// Kline intervals (and anything unrecognized, which [`BarKind::parse`] treats as a kline) split:
///
/// | plane | kline source |
/// |---|---|
/// | [`BarPlane::None`] | the snapshot, always |
/// | [`BarPlane::VenueFeeds`] | the store on a [`DIRECT_BAR_VENUES`] venue, else the snapshot |
/// | [`BarPlane::BackendStore`] | the snapshot where `published_live`, else the store |
///
/// [`core_sync::sync_from_core`](crate::core_sync::sync_from_core) enforces both bar halves: its
/// snapshot fold SKIPS any series this function does not assign to the snapshot, and its store
/// fold folds only series assigned to the store.
pub fn series_render_source(
    plane: BarPlane,
    venue: &str,
    interval: &str,
    published_live: bool,
) -> SeriesSource {
    match BarKind::parse(interval) {
        BarKind::Kline(_) => match plane {
            BarPlane::None => SeriesSource::SnapshotBars,
            BarPlane::VenueFeeds => {
                if venue_serves_direct_bars(venue) {
                    SeriesSource::DirectBars
                } else {
                    SeriesSource::SnapshotBars
                }
            }
            BarPlane::BackendStore => {
                if published_live {
                    SeriesSource::SnapshotBars
                } else {
                    SeriesSource::DirectBars
                }
            }
        },
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
/// `plane` is [`BarPlane`], exactly as in [`series_render_source`]. A key with no `'@'` cannot have
/// been built by `series_key` and is classified snapshot-rendered under every plane — no local fold
/// exists for an unknown shape, so clearing it is the safe direction (a stale paint is the bug
/// class; a refold is one frame).
///
/// ⚠ **This deliberately takes NO `published_live`, and under [`BarPlane::BackendStore`] it answers
/// `true` for EVERY kline key.** The question here is ownership, not rendering: under that plane
/// BOTH sources are the backend — the live snapshot's tail and a read of that same backend's hist
/// store — so a switch to another backend must clear a store-rendered chart exactly as it clears a
/// snapshot-rendered one. Keying it on `published_live` would have made a chart's fate depend on
/// whether the OLD backend happened to be streaming that series at the moment of the switch, and
/// left the ones it was not streaming painting backend A's history under backend B.
/// [`clear_session_state`](crate::backend_conn::clear_session_state) clears the STORE itself under
/// this plane for the same reason.
pub fn folds_from_snapshot(plane: BarPlane, key: &str) -> bool {
    let Some((_, interval)) = key.rsplit_once('@') else {
        return true; // not a series_key shape — clear on switch, never linger
    };
    let venue = crate::venue_routing::venue_of_key(key);
    let owned_by_backend = plane == BarPlane::BackendStore;
    series_render_source(plane, venue, interval, owned_by_backend) == SeriesSource::SnapshotBars
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
        assert!(
            direct_bars_mount(AppMode::ObserveWithFeeds),
            "the third mode: venue feeds fill it"
        );
        assert!(
            direct_bars_mount(AppMode::ObserveOnly),
            "the desktop: the backend's store fills it"
        );
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
}
