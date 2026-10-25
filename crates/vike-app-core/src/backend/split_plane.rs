//! `split_plane` — the pure ARM decisions behind the desktop shell's `App::new`, and the
//! double-fold guard that says which single source renders a series.
//!
//! ⚠ **ONE arm is reachable, and it is the only one encoded.** The desktop is a thin client of a
//! REMOTE `vike-tradehub` core: it signs nothing and opens no venue socket (decisions 0115 and
//! 0128). `crates/vike-desktop/src/main.rs`'s `main` calls [`observes`] with a literal `false` for
//! the build fact and `crates/vike-desktop/src/app_state.rs`'s `BAR_PLANE` binds
//! [`AppMode::ObserveOnly`] outright, so every launch composes that arm. What this module used to
//! encode beside it — the `fat` build's local-core arm (`AppMode::LocalCore`), the third mode that
//! observed a remote backend AND ran venue feeds in the same process (`AppMode::ObserveWithFeeds`
//! with its `BarPlane::VenueFeeds` plane), and the per-venue `DIRECT_BAR_VENUES` list that decided
//! which kline series those feeds owned — is deleted: the `fat` build went on 2026-09-09 (#1727,
//! rulings 1 and 2 of the desktop split) and nothing composed those arms since.
//! `crates/vike-desktop/tests/app_local_core_gate/dead_leftovers.rs` keeps `LocalCore` out.
//!
//! The split-plane spec (`docs/superpowers/specs/2026-08-18-split-plane-client-backend-design.md`
//! §1) divides the client's data into two planes with different owners: the **account plane**
//! (orders, positions, equity, kline history) is the backend's — it arrives as `WireSnapshot`s
//! over the observe bridge — while the **market-data plane** (DOM books, trade tape, tick/volume
//! and orderflow series) is read from the backend's datahub over ONE connection
//! ([`md_session`](crate::data::md_session)), not from a venue.
//!
//! The shell's `main.rs` is outside the derived CI roster (spec §2 principle 4), so every DECISION
//! lives here as a pure function and `main.rs` keeps only the wiring:
//!
//! - [`observes`] — whether a launch observes at all.
//! - [`bar_plane`] / [`series_render_source`] / [`folds_from_snapshot`] — THE DOUBLE-FOLD GUARD:
//!   which single source renders a series. Kline series fold from `snap.bars` (the backend's
//!   streamed bar tail) — EXCEPT where the [`BarPlane`] fills the GUI-side
//!   [`data_sink::DirectBarStore`](crate::data::data_sink::DirectBarStore) and claims that series;
//!   tick/volume/orderflow series fold from the GUI-side trade tape. A series must never fold from
//!   two sources — that is the bug class `backend_conn::clear_session_state`'s clear list exists
//!   for — so [`core_sync`](crate::ui::core_sync)'s snapshot fold AND its store fold each consult
//!   this function before touching a chart, and the backend-switch clear uses
//!   [`folds_from_snapshot`] to decide which chart entries are backend-session state.
//!
//! ⚠ **The mode signal is [`BarPlane`], not `direct_bars.is_some()`.** It was the store HANDLE's
//! presence until the store-read path landed — see [`BarPlane`]'s own doc, which carries the whole
//! argument.

use vike_orderflow::tickvol::BarKind;

/// The ways `App::new` can compose. Selected once at startup, so "leaving observe mode" happens
/// only at process exit, where `on_exit`'s bounded teardown shuts every feed down.
///
/// ⚠ **A one-variant enum on purpose, kept as the seam [`bar_plane`] is keyed on** (the desktop's
/// `BAR_PLANE` const and `crates/vike-app-core/tests/chart_follows_backend_series.rs` bind through
/// it). Two more variants stood beside it until 2026-10 — `LocalCore` (fat build, no `--observe`:
/// a local trading core plus local venue feeds) and `ObserveWithFeeds` (observe a remote backend
/// AND mount the keyless direct-to-venue feeds) — and no launch had composed either since the
/// `fat` build was deleted on 2026-09-09.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppMode {
    /// `--observe ADDR` (or the registry's active record): the account plane observed from a
    /// remote backend (orders dispatch through the `Scope::Write` channel, `snap_cell` fed by the
    /// observe bridge), the market-data plane read from that backend's datahub. No local core, no
    /// exec engines, no recorder, no recon, no venue armed live, and no venue socket.
    ObserveOnly,
}

/// **Was observing actually REQUESTED** — computed from the two RAW startup facts a binary owns. A
/// FAT build observed only when somebody asked for it
/// ([`backend_conn::StartupObserve::requested`](crate::backend::backend_conn::StartupObserve::requested): the
/// `--observe` word, or an active registry record), and composed the local-core arm otherwise; a
/// THIN build ALWAYS observes, because it has no local core to fall back on. ⚠ The one caller has
/// passed `fat_build = false` since the `fat` build was deleted (2026-09-09), so this answers
/// `true` on every launch. (It fed `app_mode`'s `observing` argument until that arm table and the
/// local-core variant were deleted in 2026-10.)
///
/// # ⚠ Why this is a SECOND predicate rather than "is there an address"
///
/// [`backend_conn::startup_backend_from`](crate::backend::backend_conn::startup_backend_from)'s ladder has
/// no `None` rung, deliberately: the Connections UI needs an address to show, so a bare launch still
/// resolves [`backend_registry::DEFAULT_OBSERVE_ADDR`](crate::backend::backend_registry::DEFAULT_OBSERVE_ADDR).
/// `crates/vike-desktop/src/main.rs`'s `observe_backend` used that always-`Some` answer AS the mode, so
/// from #1610 (2026-09-03) every ordinary desktop launch fell through to the local default and took
/// the OBSERVER arm: `core: None`, no exec engines, no recorder, no recon driver, and
/// `crates/vike-desktop/src/app_ui.rs`'s `dispatch` a read-only no-op — the GUI could not place an order
/// at all. **Which address to watch and whether to watch at all are different questions**; the
/// ladder answers the first and this answers the second.
pub fn observes(fat_build: bool, observe_requested: bool) -> bool {
    !fat_build || observe_requested
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
/// kline on five hand-listed CEX venues is VENUE-DIRECT, a fact about a market-data socket this
/// process does not open.
///
/// The two facts are now separate: the HANDLE says whether a store exists, this says who fills it
/// and therefore which series may render from it. `core_sync` `debug_assert!`s that the two agree
/// ([`BarPlane::None`] ⟺ no handle), so the pairing is CHECKED where it used to be assumed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BarPlane {
    /// No GUI-side bar store at all: every kline series renders from `snap.bars`. No shipped
    /// binary selects it (the arm that did, the local core, is deleted); it is kept because it is
    /// a DIFFERENT answer — "no store" — that `core_sync`'s and `backend_conn`'s suites exercise
    /// through the handle-versus-plane pairing check.
    None,
    /// The store is filled by ONE-SHOT reads of **the BACKEND'S OWN hist store**
    /// ([`store_bars`](crate::data::store_bars)) — the desktop asking the datahub for an interval the
    /// daemon does not stream. A kline series renders from the store exactly where the live
    /// snapshot does NOT publish it, and from the snapshot everywhere else.
    ///
    /// ⚠ **The predicate is "does this backend publish this series", NOT a venue list.** Which
    /// series the SERVER's store holds is a fact about the server, so no client-side list of
    /// venues can answer it; a list keyed on venue would claim the store for a 1m series the
    /// daemon is streaming live and leave every other venue unable to reach the store at all.
    BackendStore,
}

/// Which plane fills the bar store in each arm — the arm-table pin for [`BarPlane`].
///
/// `const fn` so a composition root can bind it as a `const` beside its arm rather than computing
/// it per frame — which is also what keeps the binary's copy provably equal to this table.
pub const fn bar_plane(mode: AppMode) -> BarPlane {
    match mode {
        AppMode::ObserveOnly => BarPlane::BackendStore,
    }
}

/// The single source a series renders from — see the module doc's double-fold guard section.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeriesSource {
    /// `snap.bars`, the published `CoreSnapshot`'s bar map — filled by the BACKEND's streamed bar
    /// tail (the local core that once filled it is deleted).
    SnapshotBars,
    /// The GUI-side trade tape ([`TradeStore`](crate::data::data_sink::TradeStore)), fed by the
    /// backend datahub's market-data wire.
    LocalTape,
    /// The GUI-side bar store ([`DirectBarStore`](crate::data::data_sink::DirectBarStore)). WHAT fills
    /// it — and therefore which series may claim it — is [`BarPlane`]: under
    /// [`BarPlane::BackendStore`] it is the backend's HIST STORE, read once per series the live
    /// snapshot does not publish. Never reachable under [`BarPlane::None`].
    DirectBars,
}

/// THE DOUBLE-FOLD GUARD: the one source a series renders from. Total and single-valued — every
/// `(plane, interval, published_live)` triple has exactly one source, so no series can ever be
/// assigned two. (It took the series' venue slug as well until the per-venue predicate behind
/// [`BarPlane`]'s deleted venue-fed plane went; nothing reads a venue now.)
///
/// * `plane` — [`BarPlane`], WHAT fills the bar store. Not the store's presence: see that type's
///   doc for why the two were split.
/// * `published_live` — **does the BACKEND's live snapshot carry this exact series THIS frame**
///   (`snap.bars` holds the `(venue, symbol, interval)` key). Read only under
///   [`BarPlane::BackendStore`], where it is the whole rule; ignored under [`BarPlane::None`]. The
///   caller must not guess it:
///   [`core_sync::sync_from_core`](crate::ui::core_sync::sync_from_core)'s snapshot fold is ITERATING
///   `snap.bars`, so it passes `true` by construction, and its store fold probes the same map.
///
/// Tick (`"100t"`) and volume (`"10v"`) intervals fold from the local tape under every plane.
/// Kline intervals (and anything unrecognized, which [`BarKind::parse`] treats as a kline) split:
///
/// | plane | kline source |
/// |---|---|
/// | [`BarPlane::None`] | the snapshot, always |
/// | [`BarPlane::BackendStore`] | the snapshot where `published_live`, else the store |
///
/// [`core_sync::sync_from_core`](crate::ui::core_sync::sync_from_core) enforces both bar halves: its
/// snapshot fold SKIPS any series this function does not assign to the snapshot, and its store
/// fold folds only series assigned to the store.
pub fn series_render_source(plane: BarPlane, interval: &str, published_live: bool) -> SeriesSource {
    match BarKind::parse(interval) {
        BarKind::Kline(_) => match plane {
            BarPlane::None => SeriesSource::SnapshotBars,
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
/// [`workspace::series_key`](crate::ui::workspace::series_key)'s two shapes) belong to the
/// snapshot-rendered family? The backend-switch clear
/// ([`backend_conn::clear_session_state`](crate::backend::backend_conn::clear_session_state)) uses this to
/// decide which `charts` entries are backend-session state (snapshot-rendered → backend A's bars,
/// cleared) and which are local-plane state (tape-rendered → kept).
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
/// [`clear_session_state`](crate::backend::backend_conn::clear_session_state) clears the STORE itself under
/// this plane for the same reason.
pub fn folds_from_snapshot(plane: BarPlane, key: &str) -> bool {
    let Some((_, interval)) = key.rsplit_once('@') else {
        return true; // not a series_key shape — clear on switch, never linger
    };
    let owned_by_backend = plane == BarPlane::BackendStore;
    series_render_source(plane, interval, owned_by_backend) == SeriesSource::SnapshotBars
}

#[path = "split_plane_tests.rs"]
#[cfg(test)]
mod split_plane_tests;
