//! Workspace persistence — capture/restore of the CHART window layout as JSON.
//!
//! v2 scope (chart-UX bundle T10): chart windows only — symbol, interval, style,
//! geometry, min/max state, follow-live flags, PLUS the per-window state the
//! bundle added: the price-`scale` mode (T3), the flattened `ChartOptions`
//! (colors/flags/precision/`show_volume`, T6), the per-pane height `panes` (T9),
//! and per-indicator `params` + per-output-line color/width (T8). Tool windows
//! (Trade/News/…) are launcher-recreatable and carry runtime state, so they are
//! deliberately skipped. The file lives at `$VIKE_WORKSPACE`, else
//! `<project>/settings/state/workspace.json` (see [`path`] and
//! `vike_model::paths::state_path` for the resolution), is loaded
//! automatically at startup when present (skipped under `VIKE_SHOT` so QA
//! captures stay deterministic), and is written only by an explicit
//! File → Save Workspace.
//!
//! **Named layouts** (TradingView-style): in addition to that single default
//! file, any number of NAMED layouts can be saved as individual files under a
//! `layouts/<name>.json` directory beside the workspace file — so, like it,
//! `<project>/settings/state/layouts/` (see `base_dir`) — same `Workspace`
//! schema, same forward-compat. `save_layout`/`load_layout`/`delete_layout`/
//! `list_layouts` drive them; `sanitize_layout_name` maps a user-entered name to
//! a safe filename stem; `last_layout`/`last_layout.txt` remember the last one
//! used so a restart can reopen it. The single `workspace.json` is untouched by
//! all of this (see the helpers block after `load_from`).
//!
//! **v1 forward-compat:** `VERSION` is now 2, but a v1 file still loads. The
//! interim v1 `show_volume` was a standalone top-level bool; it now flows into
//! `options.show_volume` because `ChartOptions` is `#[serde(flatten)]`ed (its
//! volume field is the top-level key `show_volume`) and struct-level
//! `#[serde(default)]` fills every ABSENT option/scale/pane/param field with its
//! CUSTOM default (see `vike_chart::chart::options`). So a v1 JSON parses cleanly
//! into the v2 structs rather than falling through to `None` — startup never
//! bricks (gated by `tests::v1_json_loads_with_flatten_and_custom_defaults`).

use super::state::{DEFAULT_VENUE, WinKind, WinState};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use vike_chart::chart::{ChartStyle, PaneKey, ScaleAssign};
use vike_chart::{ChartOptions, DisplayTz, ScaleMode};
use vike_model::AssetClass;

pub const VERSION: u32 = 2;

/// `#[serde(default)]` for [`WinSnap::venue`] — a pre-feature file (no `venue` key) restores as
/// Binance, keeping every existing workspace byte-identical to the single-venue behavior.
fn default_venue() -> String {
    DEFAULT_VENUE.to_string()
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct IndSnap {
    pub name: String, // vike-indicators registry key (e.g. "macd")
    pub visible: bool,
    /// Live parameter values, index-aligned to the registry `params` (chart-UX
    /// bundle T8). `#[serde(default)]` so a v1 file (no `params`) loads as the
    /// registry defaults — `apply` only calls `set_params` when this is non-empty.
    #[serde(default)]
    pub params: Vec<f64>,
    /// Per-output-line paint: `(output name, rgb, stroke width)` (chart-UX bundle
    /// T8). Matched back onto `Active::outputs` BY NAME on `apply` (order-robust).
    /// `#[serde(default)]` for v1 compat (no `lines` → keep the rotation palette).
    #[serde(default)]
    pub lines: Vec<(String, [u8; 3], f32)>,
    /// TradingView "symbol" input (foreign-source study): the `(venue, symbol)` this
    /// study computes off instead of the window's own primary — see
    /// [`vike_chart::indicators::SourceSymbol`]. `#[serde(default)]` +
    /// `skip_serializing_if` so a pre-feature file (and every ordinary same-symbol
    /// study) loads/saves EXACTLY as before: absent ⇒ `None` ⇒ the primary symbol.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<(String, String)>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct WinSnap {
    pub kind: String, // WinKind as a stable string ("chart"; others reserved)
    pub symbol: String,
    pub interval: String,
    /// Data venue for the chart's live feed (cross-exchange symbol search). `#[serde(default)]`
    /// → a pre-feature file (no `venue` key) loads as `"binance"` (see [`default_venue`]),
    /// byte-identical to the single-venue behavior every existing workspace was written under.
    #[serde(default = "default_venue")]
    pub venue: String,
    /// Asset class of the picked instrument (feed-routing slice 1), the persisted twin of
    /// `WinState::asset_class` — read back on restore so a chart on a non-spot instrument
    /// (e.g. an OKX perp) keeps routing to its native product feed after a workspace reload.
    /// `#[serde(default)]` + `skip_serializing_if` so a pre-feature file (no `asset_class` key
    /// at all) loads as `None` (byte-identical to the pre-feature spot-only behavior), and an
    /// ordinary spot window's saved JSON stays unchanged (no new key written for it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asset_class: Option<AssetClass>,
    pub style: usize, // index into ChartStyle::ALL (documented stable order)
    pub pos: (f32, f32),
    pub size: (f32, f32),
    pub open: bool,
    pub minimized: bool,
    pub maximized: bool,
    /// The app-owns-this-geometry LATCH, the persisted twin of [`WinState::user_sized`]: the user
    /// grabbed one of this window's own edge bands, so the app — not egui — sizes it. `size` above
    /// is persisted, so WITHOUT this key a window the user deliberately dragged smaller reloaded
    /// at that size and then re-grew to its content, silently discarding the drag.
    /// `#[serde(default)]` → a pre-feature file (no `user_sized` key) loads as `false`,
    /// byte-identically to the unlatched behavior every existing workspace was written under (the
    /// [`WinSnap::venue`] / [`WinSnap::asset_class`] idiom).
    ///
    /// ⚠ Every key WRITTEN today is `false`, and deliberately so rather than by oversight:
    /// [`capture`] filters to `WinKind::Chart`, and a chart is a `fills` kind that
    /// `show_window` never lets take the latch. This carries the plumbing so the value survives
    /// the day tool windows join that capture.
    #[serde(default)]
    pub user_sized: bool,
    pub follow: bool,
    pub auto_y: bool,
    /// Price-scale mode (chart-UX bundle T3). `#[serde(default)]` → v1 = Linear.
    #[serde(default)]
    pub scale: ScaleMode,
    /// TradingView "Invert scale" flag. `#[serde(default)]` → an old file (no
    /// `invert` key) loads `false` (not inverted) — byte-identical to pre-invert.
    #[serde(default)]
    pub invert: bool,
    /// Chart appearance/behavior (chart-UX bundle T6), FLATTENED so its
    /// `show_volume` field is a top-level key: a v1 file's standalone
    /// `show_volume` flows straight in, and `ChartOptions`' struct-level
    /// `#[serde(default)]` fills its (absent-in-v1) color/flag/precision fields
    /// with their CUSTOM defaults (not type-zeros). See the module docs.
    #[serde(flatten)]
    pub options: ChartOptions,
    /// Per-pane height fractions (chart-UX bundle T9), stored uid-INDEPENDENTLY.
    /// Post C1 Task 5, each `PaneKey::Study` entry holds the study PANE's 0-based
    /// ORDINAL among `w.present_study_panes()` (top→bottom) — NOT an individual
    /// oscillator's index (a pane can now hold more than one merged study, see
    /// Task 2's `move_study`) and NOT the pane's own volatile session-local id
    /// (`WinState::next_pane_id`, reassigned every reload). On the default
    /// one-oscillator-per-pane layout a pane's ordinal is numerically identical
    /// to its sole oscillator's index, so this is BYTE-IDENTICAL to the
    /// pre-Task-5 encoding — old files still restore heights correctly with no
    /// migration. `capture`/`apply` (`panes_to_snap`/`snap_to_panes`) translate
    /// pane-id↔ordinal in both directions, in lockstep with `pane_membership`
    /// below (same ordinal numbering for both fields). This is wire-identical to
    /// a `PaneFractions` (a serde-transparent newtype over
    /// `Vec<(PaneKey, f32)>`), but kept as the inner `Vec` here so `WinSnap` stays
    /// `PartialEq` and the entries are readable/translatable. `#[serde(default)]`
    /// → a v1 file (no `panes`) loads empty (every pane takes its default share).
    /// C2 tidy (FIX 2): `PaneKey::Series` entries round-trip the SAME way, ordinal-keyed
    /// against `w.present_series_panes()`/`series_panes` below (previously dropped —
    /// `panes_to_snap`/`snap_to_panes` had a placeholder `None` arm for `Series` until this
    /// fix, so a reloaded own-paned compare series always came back at its 0.16 default).
    #[serde(default)]
    pub panes: Vec<(PaneKey, f32)>,
    /// C1 Task 5: authored STUDY PANE membership + order — the persisted twin of
    /// `WinState::pane_order`/`study_pane`. Outer index = a study pane's 0-based
    /// ORDINAL among `w.present_study_panes()` (top→bottom, matching `panes`'
    /// `Study(ordinal)` keys above); inner = the 0-based INDICES (into
    /// `osc_uids(w)`, i.e. among the window's oscillator-kind indicators, in add
    /// order) of the oscillators assigned to that pane. Uid-independent for the
    /// same reason `panes` is: an `Active::uid` and a `PaneKey::Study`'s pane id
    /// are both reassigned every reload, so persistence must reference
    /// oscillators/panes by stable POSITION, not by either volatile id.
    /// `#[serde(default)]` → an old/pre-Task-5 file (no `pane_membership` key at
    /// all) loads empty, and `apply` falls back to today's default: one pane per
    /// oscillator, in order (`WinState::assign_default_pane`'s shape) — under
    /// which a pane's ordinal coincides with its oscillator's index, so the SAME
    /// old `panes` heights (already `Study(osc index)`-keyed) still land on the
    /// right pane with no migration.
    #[serde(default)]
    pub pane_membership: Vec<Vec<usize>>,
    /// Chart single-max default: the authored UNIFIED sub-pane ORDER — Volume,
    /// CVD, and study panes as PEERS, top→bottom, exactly as the user arranged
    /// them (`WinState::pane_order`). `PaneKey::Volume`/`Cvd` are stored verbatim
    /// (singletons); each `PaneKey::Study` is stored by its 0-based ORDINAL among
    /// `w.present_study_panes()` (top→bottom) — the SAME ordinal basis as
    /// `pane_membership`/`panes` above, so a study's position survives a uid
    /// reassignment. This is what preserves whether Volume/CVD sit above or below
    /// a study pane (previously the order was hardcoded Volume→CVD→studies and
    /// this field did not exist). `#[serde(default)]` → an OLD file (no key) loads
    /// EMPTY, and `apply` leaves the study-only order `pane_membership` rebuilt,
    /// letting `WinState::sync_sub_panes` re-slot Volume→CVD at the front next
    /// frame — i.e. the exact pre-reorder Volume→CVD→studies sequence, so old
    /// saves restore byte-identically.
    #[serde(default)]
    pub sub_pane_order: Vec<PaneKey>,
    /// Sync group membership (chart sync seam, task B8): `1..=4` or `None` (ungrouped).
    /// `#[serde(default)]` → a pre-B8 file (no key at all) loads as `None`, matching
    /// `WinState::sync_group`'s zero-visual-change default (see its doc).
    #[serde(default)]
    pub sync_group: Option<u8>,
    /// SP2 orderflow (Task 7): CVD sub-pane toggle. `#[serde(default)]` → a pre-SP2 file
    /// (no key at all) loads `false`, matching `WinState::cvd_on`'s zero-visual-change default.
    #[serde(default)]
    pub cvd_on: bool,
    /// SP2 orderflow (Task 7): volume-profile overlay toggle. `#[serde(default)]` → `false`.
    #[serde(default)]
    pub profile_on: bool,
    /// SP2 orderflow (Task 7): volume-profile / footprint tick-size override.
    /// `#[serde(default)]` → `None` (the aggregator's own default) for a pre-SP2 file.
    #[serde(default)]
    pub of_tick_size: Option<f64>,
    /// C2b Task 8: overlaid+own-pane "Compare" symbols (C2a Task 4), in add order
    /// (== color order — see `WinState::compare`'s doc). The index basis for
    /// `series_panes` below. `#[serde(default)]` → a pre-C2a file (no `compare`
    /// key at all) loads empty — zero overlays, byte-identical to a chart that
    /// predates the Compare feature entirely.
    #[serde(default)]
    pub compare: Vec<String>,
    /// C2b Task 8: own-pane compare-series membership, ordinal-keyed exactly like
    /// `pane_membership` above but over `compare` instead of `osc_uids` — outer
    /// index = a series pane's 0-based ORDINAL among `w.present_series_panes()`
    /// (top→bottom); inner = the 0-based INDICES into `compare` of the symbols
    /// (`w.series_pane` members) assigned to that pane. A `Vec<Vec<usize>>` (not a
    /// flat `Vec<usize>`) because `WinState::move_series`'s `Into` target can merge
    /// more than one compare symbol into a single pane — mirroring `move_study`
    /// exactly — so a flat encoding would silently explode a merged pane back into
    /// one-pane-per-symbol on reload. Uid/id-independent for the same reason
    /// `pane_membership` is: a `PaneKey::Series`'s id is reassigned every reload
    /// (`WinState::next_series_pane_id` restarts at 1), so persistence must
    /// reference panes by stable POSITION, not the volatile id. `#[serde(default)]`
    /// → an old file (no `series_panes` key) loads empty, and `apply` leaves every
    /// restored `compare` symbol overlaid in the price pane (there is no per-symbol
    /// "default own pane" the way studies default to one-per-oscillator — see
    /// `WinState::series_pane`'s doc) — the C2a-only, pre-C2b behavior.
    #[serde(default)]
    pub series_panes: Vec<Vec<usize>>,
    /// C2b Task 8: per-compare-symbol "Pin to scale" assignment (C2b Task 7),
    /// `(symbol, assignment)` pairs mirroring `w.series_scale` verbatim (a `Vec`
    /// rather than a map — serde-simple, and insertion order is preserved either
    /// way). Only symbols with a non-default (`Right`/`Left`) pin need to be
    /// stored, but storing every entry is simplest and still correct (`ABSENT` and
    /// `Percent` are equivalent per `WinState::series_scale_of`). `#[serde(default)]`
    /// → an old file (no `series_scale` key) loads empty — every symbol resolves to
    /// the `Percent` default, the C2a-only shared-% behavior.
    #[serde(default)]
    pub series_scale: Vec<(String, ScaleAssign)>,
    pub indicators: Vec<IndSnap>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Workspace {
    pub version: u32,
    /// Global display timezone (task A6), `DisplayTz::name()` (e.g. `"Local"`, `"UTC"`,
    /// `"Europe/Dublin"`). `#[serde(default = "default_tz_name")]` so a pre-A6 file (no key
    /// at all) loads as `"Local"` (the ONE GLOBAL default — see `vike_chart::tz`'s module
    /// doc); an unrecognized/exotic name still round-trips via `DisplayTz::parse`'s own IANA
    /// fallback (never bricks a load).
    #[serde(default = "default_tz_name")]
    pub display_tz: String,
    /// SP3 orderflow backfill (Task 3): how many hours of pre-live aggTrade history each
    /// orderflow symbol backfills in the background — GLOBAL (applies to every symbol, not
    /// per-window; see `main.rs`'s `App::of_backfill_hours` doc for why global is simpler than
    /// per-window here). `#[serde(default = "default_backfill_hours")]` so a pre-SP3 file (no
    /// key at all) loads as `2.0`, never `0.0` — an old workspace still gets background
    /// backfill on upgrade rather than silently losing the feature (`0.0` is reserved for an
    /// explicit opt-out, matching `global-constraints.md`'s "default = SP2 behavior" framing
    /// being about a FRESH `of_backfill_hours` field, not about old saves).
    #[serde(default = "default_backfill_hours")]
    pub of_backfill_hours: f64,
    /// GPU candle-layer render toggle (GPU Phase 2, Task 3), GLOBAL like `display_tz`/
    /// `of_backfill_hours` above (one knob, not per-window — see `main.rs`'s `App::gpu_render`
    /// doc). `#[serde(default)]` (bool's zero value already IS the wanted default) so a
    /// pre-Task-3 file (no key at all) loads `false` — the pre-GPU-toggle egui/LOD-only
    /// behavior, byte-identical.
    #[serde(default)]
    pub gpu_render: bool,
    /// Indicator favourites (feature part b): the user's ⭐-starred indicator
    /// registry keys (e.g. `["rsi", "macd"]`), GLOBAL like `display_tz`/`gpu_render`
    /// above (one set, shared by every chart's ƒx picker — not per-window). Order is
    /// the star order (a `Vec`, not a set — insertion order is the display order).
    /// `#[serde(default)]` so a pre-feature file (no key at all) loads as an EMPTY
    /// list — a picker with no Favourites section, byte-identical to today. Written
    /// by [`save_to`]/[`save_layout`] (not [`capture`], which stays layout-only), so
    /// the whole global preference lives at the top level beside the other globals.
    #[serde(default)]
    pub indicator_favs: Vec<String>,
    pub wins: Vec<WinSnap>,
}

fn default_tz_name() -> String {
    DisplayTz::Local.name().to_string()
}

/// SP3 Task 3: the default background-backfill window when no workspace file (or an old one
/// predating this field) sets it — 2 hours of pre-live aggTrade history per orderflow symbol.
/// `pub(crate)` (not private): `main.rs`'s `App::new` reads it too, as `App::of_backfill_hours`'s
/// own initial value — ONE source of truth shared with this module's serde default, rather than
/// a second hardcoded `2.0` main.rs could drift out of sync with.
pub fn default_backfill_hours() -> f64 {
    2.0
}

/// `ChartStyle` → stable index in `ChartStyle::ALL` (the documented menu order).
fn style_index(s: ChartStyle) -> usize {
    ChartStyle::ALL.iter().position(|&x| x == s).unwrap_or(0)
}

/// Oscillator-kind indicator uids in list order — the INDEX basis for
/// `WinSnap::pane_membership`'s inner indices (and, historically, `panes`'
/// pre-Task-5 `PaneKey::Study` encoding — see both fields' docs).
fn osc_uids(w: &WinState) -> Vec<u64> {
    w.indicators.iter().filter(|a| !a.is_overlay()).map(|a| a.uid).collect()
}

/// Snapshot `w.panes` (a `PaneFractions`, whose inner `Vec` is private to
/// vike-chart — read it via a serde round-trip), translating each
/// `PaneKey::Study(pane_id)` entry to `PaneKey::Study(ordinal)`, where `ordinal`
/// is that pane's position in `w.present_study_panes()` (top→bottom). Post C1
/// Task 5 a `Study` pane may hold more than one merged oscillator, so the
/// uid-independent key is the PANE's ordinal, not an individual oscillator's
/// index (see `pane_membership_snap` below for which oscillators are in it). A
/// stored fraction for a pane no longer present (every study in it
/// removed/moved elsewhere) is dropped; Price/Volume/Cvd pass through.
///
/// On the default one-oscillator-per-pane layout (every window that hasn't
/// used the move-to-pane feature) a pane's ordinal is numerically IDENTICAL to
/// its sole oscillator's `osc_uids` index — the pre-Task-5 encoding — so this
/// is byte-identical to what an old file already has stored; old files keep
/// restoring heights correctly with zero migration.
fn panes_to_snap(w: &WinState) -> Vec<(PaneKey, f32)> {
    let study_order = w.present_study_panes();
    let series_order = w.present_series_panes();
    let entries: Vec<(PaneKey, f32)> = serde_json::to_value(&w.panes)
        .ok()
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default();
    entries
        .into_iter()
        .filter_map(|(pid, frac)| match pid {
            PaneKey::Price => Some((PaneKey::Price, frac)),
            PaneKey::Volume => Some((PaneKey::Volume, frac)),
            // SP2: CVD is a singleton pane like Volume — no ordinal translation needed.
            PaneKey::Cvd => Some((PaneKey::Cvd, frac)),
            PaneKey::Study(_) => study_order
                .iter()
                .position(|&k| k == pid)
                .map(|ord| (PaneKey::Study(ord as u64), frac)),
            // C2 tidy (FIX 2): mirrors the `Study` arm immediately above — `chart::draw` (C2b
            // Task 6/9) DOES feed live `PaneKey::Series` entries into `w.panes` now (an
            // own-paned compare series), so this used to silently drop a dragged height every
            // capture, reverting a reloaded series pane to the 0.16 default. Translated to the
            // pane's ordinal in `w.present_series_panes()` (top→bottom) — NOT its volatile
            // `next_series_pane_id`-assigned id (reassigned every reload) — exactly like
            // `Study`'s uid-independent encoding. A stored fraction for a pane no longer
            // present (its last compare symbol removed/overlaid) is dropped, same as an
            // emptied Study pane.
            PaneKey::Series(_) => series_order
                .iter()
                .position(|&k| k == pid)
                .map(|ord| (PaneKey::Series(ord as u64), frac)),
        })
        .collect()
}

/// C1 Task 5: snapshot pane MEMBERSHIP, ordinal-keyed exactly like
/// `panes_to_snap`'s heights above — outer index = a study pane's ordinal in
/// `w.present_study_panes()` (top→bottom); inner = the `osc_uids(w)` indices of
/// the oscillators (`w.study_pane` members) assigned to that pane. Paired with
/// `panes_to_snap`, this is what lets `apply`/`snap_to_panes` rebuild
/// `pane_order`/`study_pane` uid-independently, the same way heights were
/// already made uid-independent (chart-UX bundle T9/T10).
fn pane_membership_snap(w: &WinState) -> Vec<Vec<usize>> {
    let uids = osc_uids(w);
    w.present_study_panes()
        .into_iter()
        .map(|pane| {
            w.study_pane
                .iter()
                .filter(|entry| *entry.1 == pane)
                .filter_map(|entry| uids.iter().position(|u| *u == *entry.0))
                .collect()
        })
        .collect()
}

/// Inverse of [`panes_to_snap`] + [`pane_membership_snap`]: rebuild
/// `w.pane_order`, `w.study_pane`, and `w.next_pane_id` from the persisted
/// `pane_membership` (pane-ordinal → osc-indices), against the REBUILT
/// window's oscillator uids (`osc_uids(w)`; an osc-index past the reloaded
/// oscillator count — a dropped indicator — is silently skipped), then
/// rebuilds `w.panes`' heights from `s.panes` (`Study(ordinal)` entries), keyed
/// onto the SAME freshly-allocated panes so membership and heights stay
/// coherent under one ordinal numbering.
///
/// An EMPTY `pane_membership` (old/pre-Task-5 file, or simply a window with no
/// oscillators — the two shapes coincide there) falls back to one pane per
/// oscillator in add order — today's default `assign_default_pane` layout — so
/// an old file's heights (already `Study(osc index)`-keyed under that same
/// default one-pane-per-oscillator layout) keep landing on the right pane.
///
/// Must run AFTER the caller's indicator-rebuild loop (oscillator uids must
/// already exist). It first WIPES whatever default per-oscillator pane
/// assignment `WinState::add_indicator` already made while rebuilding each
/// indicator (that default is only the right shape for a BRAND-NEW window, not
/// a reload) and replaces it with the persisted arrangement; `next_pane_id` is
/// left wherever it lands (continuing on, not resetting, from whatever
/// `add_indicator` already advanced it to), since its only contract is "never
/// hand out a currently-live pane id twice," which holds either way.
fn snap_to_panes(s: &WinSnap, w: &mut WinState) {
    let uids = osc_uids(w);
    let groups: Vec<Vec<usize>> = if s.pane_membership.is_empty() {
        (0..uids.len()).map(|i| vec![i]).collect()
    } else {
        s.pane_membership.clone()
    };

    w.pane_order.clear();
    w.study_pane.clear();
    for group in &groups {
        let pane = PaneKey::Study(w.next_pane_id);
        w.next_pane_id += 1;
        w.pane_order.push(pane);
        for &idx in group {
            if let Some(&uid) = uids.get(idx) {
                w.study_pane.insert(uid, pane);
            }
        }
    }

    let entries: Vec<(PaneKey, f32)> = s
        .panes
        .iter()
        .filter_map(|(pid, frac)| match pid {
            PaneKey::Price => Some((PaneKey::Price, *frac)),
            PaneKey::Volume => Some((PaneKey::Volume, *frac)),
            // SP2: CVD is a singleton pane like Volume — no ordinal translation needed.
            PaneKey::Cvd => Some((PaneKey::Cvd, *frac)),
            PaneKey::Study(ordinal) => w.pane_order.get(*ordinal as usize).map(|&k| (k, *frac)),
            // C2 tidy (FIX 2): mirrors the `Study` arm above, resolved against
            // `w.series_pane_order` instead of `w.pane_order`. The caller (`apply`) now runs
            // `snap_to_series` BEFORE this function specifically so `series_pane_order` is
            // already rebuilt from `s.series_panes` by the time this arm runs — the ordinal
            // lands on the SAME freshly allocated `PaneKey::Series` the membership rebuild
            // produced, keeping heights and membership coherent under one ordinal numbering
            // (same contract `pane_membership`/`Study` already has).
            PaneKey::Series(ordinal) => {
                w.series_pane_order.get(*ordinal as usize).map(|&k| (k, *frac))
            }
        })
        .collect();
    w.panes = serde_json::to_value(&entries)
        .ok()
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default();
}

/// Chart single-max default: snapshot the authored UNIFIED sub-pane order for
/// [`WinSnap::sub_pane_order`]. Volume/CVD pass through verbatim; each study pane
/// is translated to its 0-based ORDINAL in `w.present_study_panes()` (top→bottom)
/// — the identical uid-independent encoding `panes_to_snap`/`pane_membership_snap`
/// use, so a study's slot in the unified order survives a uid reassignment.
fn sub_pane_order_snap(w: &WinState) -> Vec<PaneKey> {
    let study_order = w.present_study_panes();
    w.pane_order
        .iter()
        .filter_map(|p| match p {
            PaneKey::Volume => Some(PaneKey::Volume),
            PaneKey::Cvd => Some(PaneKey::Cvd),
            PaneKey::Study(_) => {
                study_order.iter().position(|k| k == p).map(|ord| PaneKey::Study(ord as u64))
            }
            PaneKey::Price | PaneKey::Series(_) => None,
        })
        .collect()
}

/// Inverse of [`sub_pane_order_snap`]: re-thread `w.pane_order` into the persisted
/// unified order, run AFTER [`snap_to_panes`] has allocated the study panes (and
/// resolved their heights) — so the study panes already exist in `w.pane_order`
/// in ordinal order and this only REORDERS them + splices Volume/CVD back to
/// their authored slots. An EMPTY `sub_pane_order` (old/pre-single-max file)
/// leaves `w.pane_order` as `snap_to_panes` built it (study-only, ordinal order);
/// `WinState::sync_sub_panes` then prepends Volume→CVD at the default front next
/// frame — the pre-reorder sequence. Any study pane not referenced by the
/// persisted order (defensive) is appended so it is never dropped.
fn apply_sub_pane_order(s: &WinSnap, w: &mut WinState) {
    if s.sub_pane_order.is_empty() {
        return;
    }
    // The study panes `snap_to_panes` just allocated, indexed by ordinal.
    let study_alloc: Vec<PaneKey> = w.pane_order.clone();
    let mut new_order: Vec<PaneKey> = Vec::with_capacity(s.sub_pane_order.len());
    for tok in &s.sub_pane_order {
        match tok {
            PaneKey::Volume => new_order.push(PaneKey::Volume),
            PaneKey::Cvd => new_order.push(PaneKey::Cvd),
            PaneKey::Study(ord) => {
                if let Some(&pk) = study_alloc.get(*ord as usize)
                    && !new_order.contains(&pk)
                {
                    new_order.push(pk);
                }
            }
            PaneKey::Price | PaneKey::Series(_) => {}
        }
    }
    for &pk in &study_alloc {
        if !new_order.contains(&pk) {
            new_order.push(pk);
        }
    }
    w.pane_order = new_order;
}

/// C2b Task 8: snapshot own-pane compare-series membership, ordinal-keyed
/// exactly like [`pane_membership_snap`] above but over `w.compare` instead of
/// `osc_uids` — outer index = a series pane's ordinal in
/// `w.present_series_panes()` (top→bottom); inner = the `w.compare` indices of
/// the symbols (`w.series_pane` members) assigned to that pane. Paired with
/// [`snap_to_series`], this is what lets `apply` rebuild `series_pane_order`/
/// `series_pane` pane-id-independently, the same way `pane_membership_snap` did
/// for study panes.
fn series_pane_membership_snap(w: &WinState) -> Vec<Vec<usize>> {
    w.present_series_panes()
        .into_iter()
        .map(|pane| {
            w.series_pane
                .iter()
                .filter(|entry| *entry.1 == pane)
                .filter_map(|entry| w.compare.iter().position(|s| s == entry.0))
                .collect()
        })
        .collect()
}

/// Inverse of [`series_pane_membership_snap`]: rebuild `w.compare`,
/// `w.series_pane_order`, `w.series_pane`, `w.next_series_pane_id`, and
/// `w.series_scale` from the persisted `WinSnap::{compare,series_panes,
/// series_scale}`. Mirrors [`snap_to_panes`] exactly, but the index basis is
/// `s.compare` itself (verbatim, not a filtered subset the way `osc_uids` is).
///
/// An EMPTY `series_panes` (old/pre-C2b file, or simply a window with no
/// own-pane compare series) leaves every restored `compare` symbol overlaid in
/// the price pane — `w.series_pane` stays empty, matching `add_compare`'s own
/// default (there is no per-symbol "default own pane" fallback the way study
/// panes default to one-per-oscillator).
///
/// C2 tidy (FIX 2): `apply` now calls this BEFORE `snap_to_panes` — a change from the
/// original C2b ordering, which ran it after (back when `panes`/`snap_to_panes` had no
/// `Series` arm to resolve at all). `snap_to_panes` needs `w.series_pane_order` already
/// rebuilt so it can translate a persisted `PaneKey::Series(ordinal)` height onto the SAME
/// freshly allocated pane id this function produces — see `snap_to_panes`'s `Series` arm.
/// This function itself has no dependency the other way: it only reads `s` (never `w.panes`/
/// `w.pane_order`/`w.study_pane`/indicators), and `series_panes`' indices are resolved against
/// `s.compare` (the snapshot itself), not `w.compare` — so reordering ahead of `snap_to_panes`
/// is safe.
fn snap_to_series(s: &WinSnap, w: &mut WinState) {
    w.compare = s.compare.clone();
    w.series_pane_order.clear();
    w.series_pane.clear();
    for group in &s.series_panes {
        let pane = vike_chart::chart::PaneKey::Series(w.next_series_pane_id);
        w.next_series_pane_id += 1;
        w.series_pane_order.push(pane);
        for &idx in group {
            if let Some(sym) = s.compare.get(idx) {
                w.series_pane.insert(sym.clone(), pane);
            }
        }
    }
    w.series_scale = s.series_scale.iter().map(|(sym, sc)| (sym.clone(), *sc)).collect();
}

/// Snapshot the current chart windows (tool windows are skipped — see module docs) plus the
/// global `display_tz` (task A6), `of_backfill_hours` (SP3 Task 3), and `gpu_render` (GPU
/// Phase 2 Task 3).
pub fn capture(
    wins: &[WinState],
    display_tz: DisplayTz,
    of_backfill_hours: f64,
    gpu_render: bool,
) -> Workspace {
    let wins = wins
        .iter()
        .filter(|w| w.kind == WinKind::Chart)
        .map(|w| WinSnap {
            kind: "chart".into(),
            symbol: w.symbol.clone(),
            interval: w.interval.clone(),
            venue: w.venue.clone(),
            asset_class: w.asset_class,
            style: style_index(w.style),
            pos: (w.pos.x, w.pos.y),
            size: (w.size.x, w.size.y),
            open: w.open,
            minimized: w.minimized,
            maximized: w.maximized,
            user_sized: w.user_sized,
            follow: w.follow.on,
            auto_y: w.follow.auto_y,
            scale: w.scale,
            invert: w.invert,
            options: w.options.clone(),
            panes: panes_to_snap(w),
            pane_membership: pane_membership_snap(w),
            sub_pane_order: sub_pane_order_snap(w),
            sync_group: w.sync_group,
            cvd_on: w.cvd_on,
            profile_on: w.profile_on,
            of_tick_size: w.of_tick_size,
            compare: w.compare.clone(),
            series_panes: series_pane_membership_snap(w),
            series_scale: w.series_scale.iter().map(|(sym, sc)| (sym.clone(), *sc)).collect(),
            indicators: w
                .indicators
                .iter()
                .map(|a| IndSnap {
                    name: a.spec.name.into(),
                    visible: a.visible,
                    params: a.params.clone(),
                    lines: a
                        .outputs
                        .iter()
                        .map(|o| {
                            (o.name.to_string(), [o.color.r(), o.color.g(), o.color.b()], o.width)
                        })
                        .collect(),
                    source: a.source_symbol.as_ref().map(|s| (s.venue.clone(), s.symbol.clone())),
                })
                .collect(),
        })
        .collect();
    Workspace {
        version: VERSION,
        display_tz: display_tz.name().to_string(),
        of_backfill_hours,
        gpu_render,
        // Favourites (part b) are a global picker preference, not part of the captured
        // window LAYOUT — `save_to`/`save_layout` inject the live set post-capture (see
        // the field doc). `capture` alone yields an empty list, so every existing
        // `capture(..)`-based round-trip test stays byte-identical.
        indicator_favs: Vec::new(),
        wins,
    }
}

/// Rebuild fresh `WinState`s from a snapshot. The caller is responsible for
/// `ensure_feed`-ing each returned window's (symbol, interval). Unknown styles
/// fall back to Candles; unknown indicator names are dropped (forward compat).
pub fn apply(ws: &Workspace) -> Vec<WinState> {
    ws.wins
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let r = egui::Rect::from_min_size(
                egui::pos2(s.pos.0, s.pos.1),
                egui::vec2(s.size.0, s.size.1),
            );
            let mut w = WinState::new(
                &format!("ws-{}-{i}", s.kind),
                &s.symbol,
                &s.interval,
                WinKind::Chart,
                r,
            );
            w.venue = s.venue.clone();
            w.asset_class = s.asset_class;
            // A SAVED LAYOUT IS A CHOICE. `WinState::new` above leaves this `false`, which is what
            // lets `series_follow::follow_backend` retarget a chart nobody named onto a series the
            // backend actually publishes; a window that came out of a workspace file was authored
            // and saved by the operator, so it is off limits to that planner. Set unconditionally
            // and persisted nowhere — see `WinState::series_pinned`.
            //
            // BOTH halves: a saved layout named an interval as deliberately as it named a symbol,
            // so `interval_pinned` is set here too. The two flags split at the MENUS (an interval
            // pick is not a series pick), not at the restore — see `WinState::interval_pinned`.
            w.series_pinned = true;
            w.interval_pinned = true;
            w.retitle(); // fold the venue prefix into the title for a non-binance restored window
            w.style = ChartStyle::ALL.get(s.style).copied().unwrap_or(ChartStyle::Candles);
            w.open = s.open;
            w.minimized = s.minimized;
            w.maximized = s.maximized;
            w.user_sized = s.user_sized;
            w.follow.on = s.follow;
            w.follow.auto_y = s.auto_y;
            w.scale = s.scale;
            w.invert = s.invert;
            w.options = s.options.clone();
            w.sync_group = s.sync_group;
            w.cvd_on = s.cvd_on;
            w.profile_on = s.profile_on;
            w.of_tick_size = s.of_tick_size;
            for ind in &s.indicators {
                let before = w.indicators.len();
                w.add_indicator(&ind.name, &[]); // no-op for unknown names
                if w.indicators.len() == before {
                    continue; // unknown name → nothing added; don't touch a prior indicator
                }
                let a = w.indicators.last_mut().expect("just pushed above");
                // Apply persisted params BEFORE any bars exist: `set_params(p, &[])`
                // rebuilds the streaming instance at `make_with(p)` and refolds over
                // an EMPTY series, so the correct instance is in place when the live
                // feed later folds real data in (T7a: reset preserves params).
                if !ind.params.is_empty() {
                    a.set_params(ind.params.clone(), &[]);
                }
                // Restore per-line paint by output name — AFTER any `set_params`,
                // whose `recompute_full` only clears each `series` (color/width are
                // left untouched), so these edits are not overwritten.
                for (lname, rgb, width) in &ind.lines {
                    if let Some(o) = a.outputs.iter_mut().find(|o| o.name == lname.as_str()) {
                        o.color = vike_ui_theme::color::rgb(*rgb);
                        o.width = *width;
                    }
                }
                a.visible = ind.visible;
                // Foreign-source study (TradingView "symbol" input): restore the
                // `(venue, symbol)` this study computes off. Absent ⇒ `None` ⇒ the
                // window's own primary (byte-identical to a pre-feature file). The
                // per-frame window loop ensure-subscribes the foreign feed and folds
                // the study over it — no bars are needed here at restore time.
                a.source_symbol = ind.source.as_ref().map(|(venue, symbol)| {
                    vike_chart::indicators::SourceSymbol {
                        venue: venue.clone(),
                        symbol: symbol.clone(),
                    }
                });
            }
            // C2b Task 8: compare symbols + own-pane placement + per-symbol scale, rebuilt
            // FIRST — C2 tidy (FIX 2): `snap_to_panes` below now has a `PaneKey::Series` arm
            // that resolves against `w.series_pane_order`, so that must already exist by the
            // time it runs (see `snap_to_series`'s doc for why this ordering is now a hard
            // dependency, not just a convention).
            snap_to_series(s, &mut w);
            // Panes + pane membership need the REBUILT indicators' uids
            // (index/ordinal → new-uid/new-pane-id) AND the REBUILT `series_pane_order`
            // above, so rebuild after both.
            snap_to_panes(s, &mut w);
            // Chart single-max default: re-thread the unified Volume/CVD/Study order
            // AFTER `snap_to_panes` has allocated the study panes + resolved heights
            // (it only reorders + splices Volume/CVD; empty ⇒ old-file forward-compat).
            apply_sub_pane_order(s, &mut w);
            w
        })
        .collect()
}

/// The workspace file's basename, stated once (the `layouts/` siblings use their own names).
const WORKSPACE_FILE: &str = "workspace.json";

/// The `$VIKE_WORKSPACE` override, when set: it names the workspace FILE outright, and its
/// directory is the base for the named-layout family, so one variable moves the whole family
/// together.
fn workspace_override() -> Option<PathBuf> {
    std::env::var("VIKE_WORKSPACE").ok().map(PathBuf::from)
}

/// `<project>/settings/state` exactly as the COMPOSITION ROOT's boot resolved it — the middle rung
/// of [`state_dir`], set once by [`declare_project_state_dir`].
///
/// `None` inside the cell is a legitimate answer (the root booted and found no project); an UNSET
/// cell means no root ever declared, which is a test, a tool, or any binary that does not boot.
static DECLARED_STATE_DIR: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();

/// Raised the first time [`state_dir`] answers WITHOUT a declaration in force, so a declaration
/// arriving afterwards can say it changed nothing instead of silently relocating the family
/// mid-run. The same guard `vike_bridge_core::halt`'s `RESOLVED` provides for the sentinel.
static RESOLVED_UNDECLARED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// **Declare the project state directory the composition root already resolved** — call it with
/// `vike_boot::Booted::state_dir`, once, before anything reads the workspace family.
///
/// # ⚠ Why this exists: `$VIKE_SETTINGS_DIR` did not reach this family, and the mismatch was silent
///
/// MEASURED 2026-09-15. Launching `vike-desktop` with `VIKE_SETTINGS_DIR=<project>/settings` from a
/// DIFFERENT working directory read the CREDENTIALS out of that project while resolving the backend
/// registry somewhere else entirely: the client reported *"no --observe and no active backend"*
/// while a perfectly valid `backends.json` sat in the named settings directory. Passing
/// `$VIKE_STATE_ROOT` as well made it work, which is the shape of a defect rather than a
/// configuration.
///
/// The cause is the rule the root `CLAUDE.md` states as **ONE walk DECIDES**: every project-relative
/// path a root uses must be derived from `Booted`, "because the `_from`-less resolvers are
/// `$VIKE_SETTINGS_DIR`-BLIND and a second call answers with whatever the working directory sits
/// above". [`state_dir`]'s `current_dir()` walk was exactly that second call.
///
/// # Why a declaration rather than a parameter on every function
///
/// The cure may NOT be a second environment read here — `crates/vike-ops/tests/settings_secrets/settings_registry.rs`'s
/// `LIBRARY_PIN` is a ratchet that may shrink and never grow, and a library resolving global state
/// its caller can neither see nor override is the defect that ratchet exists to stop. So the answer
/// arrives as a PARAMETER, exactly as `vike_bridge_core::halt::declare_project_state_dir` takes it
/// for the kill switch's rung 2 and for the same reason. Threading a directory through the whole
/// `path`/`base_dir`/`save_path`/`layouts`/`backends` surface and its GUI call sites would be a far
/// larger change than the defect, in the one crate a size ratchet polices.
///
/// # What it returns
///
/// `Err` when the declaration is refused or arrived too late to matter. `Ok(Some(notice))` when it
/// MOVED the family — the answer changed and the old directory still holds files — which the
/// binary logs once it has a subscriber. `Ok(None)` when nothing moved. This module logs nothing
/// itself: a root declares before `vike_log::init` in every binary that has one.
pub fn declare_project_state_dir(state_dir: Option<PathBuf>) -> Result<Option<String>, String> {
    let before = resolve_state_dir();
    let mut first = false;
    let declared = DECLARED_STATE_DIR.get_or_init(|| {
        first = true;
        state_dir.clone()
    });
    if declared.as_deref() != state_dir.as_deref() {
        return Err(format!(
            "the workspace family's project was already declared as {}; the second declaration \
             ({}) is IGNORED — one process has one workspace directory",
            state_dir_label(declared.as_deref()),
            state_dir_label(state_dir.as_deref())
        ));
    }
    // Only a declaration that WOULD have changed something can be late. Repeating the one already
    // in force is not a fault and must not be reported as one.
    if first && RESOLVED_UNDECLARED.load(std::sync::atomic::Ordering::Relaxed) {
        return Err(format!(
            "the workspace directory was already resolved as {} before this declaration, so the \
             declaration changed nothing — it has to run before the first workspace, layout or \
             backend-registry read",
            state_dir_label(before.as_deref())
        ));
    }
    Ok(relocation_notice(before.as_deref(), resolve_state_dir().as_deref()))
}

/// A state directory, for an operator-facing message. `None` is a real answer and has to read as
/// one rather than as an empty string.
fn state_dir_label(dir: Option<&std::path::Path>) -> String {
    match dir {
        Some(p) => p.display().to_string(),
        None => "<no project above the working directory>".to_string(),
    }
}

/// The family's members, named once — what a relocation actually moves. `layouts/` is a directory;
/// the rest are files.
const FAMILY_MEMBERS: [&str; 4] = [
    WORKSPACE_FILE,
    LAYOUTS_SUBDIR,
    LAST_LAYOUT_FILE,
    crate::backend::backend_registry::BACKENDS_FILE,
];

/// ⚠ **The migration question, answered out loud rather than silently.** When a declaration moves
/// the family, an operator who was relying on the working-directory walk now reads a DIFFERENT
/// `workspace.json`, a different `layouts/`, a different `last_layout.txt` and a different
/// `backends.json` — a saved layout and a whole backend registry appearing to vanish.
///
/// Nothing is copied, moved or deleted: this workspace never relocates a file the operator wrote.
/// The notice names both directories and the members that exist at the old one, so the fix is a
/// copy the operator can make with their eyes open. Silence here would look exactly like data loss.
///
/// `None` when the answer did not change, or when the old location holds nothing to lose.
fn relocation_notice(
    before: Option<&std::path::Path>,
    after: Option<&std::path::Path>,
) -> Option<String> {
    if before == after {
        return None;
    }
    let old = before?;
    let found: Vec<&str> = FAMILY_MEMBERS.into_iter().filter(|m| old.join(m).exists()).collect();
    if found.is_empty() {
        return None;
    }
    Some(format!(
        "the workspace family now resolves to {} (the settings directory this process booted \
         with), not {} (what the working directory walks up to). {} still sit(s) at the old \
         location and will NOT be read — nothing here moves or deletes them; copy them across if \
         you want them, or set VIKE_STATE_ROOT to the old directory to keep reading it.",
        state_dir_label(after),
        state_dir_label(Some(old)),
        found.join(", ")
    ))
}

/// The project's STATE directory — `<project>/settings/state` — or `None` when neither an
/// override, a root's declaration, nor a walk above the working directory answers.
///
/// Three rungs, in order:
///
/// 1. **`$VIKE_STATE_ROOT`** — an operator naming a path outright means it, and it keeps winning
///    over everything below. The env-reading half of `vike_model::paths::state_path` (which is pure), and
///    the one `Layer::Library` row this family carries in `vike_ops::settings::SETTINGS`.
/// 2. **The COMPOSITION ROOT's declaration** ([`declare_project_state_dir`]) — the one walk the
///    boot already performed, which is the only rung that honours `$VIKE_SETTINGS_DIR`.
/// 3. **The walk**, from the working directory, for a process whose root declares nothing (a test,
///    a tool). `$VIKE_SETTINGS_DIR`-BLIND, which is why rung 2 exists above it.
///
/// There is no fourth location: program-written JSON lands in the same folder as every other
/// setting.
fn state_dir() -> Option<PathBuf> {
    if DECLARED_STATE_DIR.get().is_none() {
        // Rung 3 is about to answer (or rung 1 is, which a declaration could not have changed
        // either). Either way a later declaration is too late to have decided this call.
        RESOLVED_UNDECLARED.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    resolve_state_dir()
}

/// [`state_dir`] WITHOUT the lateness bookkeeping, so [`declare_project_state_dir`] can ask what
/// the answer is (before and after) without making itself late.
fn resolve_state_dir() -> Option<PathBuf> {
    if let Some(explicit) = std::env::var("VIKE_STATE_ROOT").ok().filter(|s| !s.trim().is_empty()) {
        return Some(PathBuf::from(explicit));
    }
    if let Some(declared) = DECLARED_STATE_DIR.get() {
        return declared.clone();
    }
    std::env::current_dir()
        .ok()
        .and_then(|cwd| vike_model::paths::state_path::project_state_dir(&cwd))
}

/// Workspace file path for READING: `$VIKE_WORKSPACE`, else `<state_dir>/workspace.json`.
///
/// `None` when neither resolves — no override, and no project (nor `$VIKE_STATE_ROOT`) above the
/// working directory. There is no file to read then, and [`load`] reports that as "no saved
/// workspace" rather than this resolver inventing a second location.
pub fn path() -> Option<PathBuf> {
    match workspace_override() {
        Some(p) => Some(p),
        None => Some(state_dir()?.join(WORKSPACE_FILE)),
    }
}

/// Workspace file path for WRITING: `$VIKE_WORKSPACE`, else `<state_dir>/workspace.json`, whose
/// directory is created lazily here.
///
/// `Err` — never a panic, and never a different location — when there is no state directory to
/// resolve or it cannot be created (a scrubbed environment, a read-only project). The caller
/// surfaces that to the user, who can then say where the file should go; a save that silently
/// landed somewhere else would be found by nothing afterwards.
fn save_path() -> std::io::Result<PathBuf> {
    match workspace_override() {
        Some(p) => Ok(p),
        None => vike_model::paths::state_path::write_path(state_dir().as_deref(), WORKSPACE_FILE),
    }
}

/// Serialize + write the current layout; returns the path written.
pub fn save(
    wins: &[WinState],
    display_tz: DisplayTz,
    of_backfill_hours: f64,
    gpu_render: bool,
    indicator_favs: &[String],
) -> std::io::Result<PathBuf> {
    let p = save_path()?;
    save_to(wins, display_tz, of_backfill_hours, gpu_render, indicator_favs, &p)?;
    Ok(p)
}

fn save_to(
    wins: &[WinState],
    display_tz: DisplayTz,
    of_backfill_hours: f64,
    gpu_render: bool,
    indicator_favs: &[String],
    p: &std::path::Path,
) -> std::io::Result<()> {
    // Favourites (part b) are injected here rather than inside `capture` (which stays
    // window-layout-only — see `Workspace::indicator_favs`'s doc).
    let mut ws = capture(wins, display_tz, of_backfill_hours, gpu_render);
    ws.indicator_favs = indicator_favs.to_vec();
    let json = serde_json::to_string_pretty(&ws).map_err(std::io::Error::other)?;
    std::fs::write(p, json)
}

/// Read + parse the workspace file; `None` if absent or unparseable (a corrupt
/// file must never brick startup — fall back to the default layout). A v1 file
/// parses successfully into the v2 structs via serde defaults (module docs).
pub fn load() -> Option<Workspace> {
    load_from(&path()?)
}

fn load_from(p: &std::path::Path) -> Option<Workspace> {
    let raw = std::fs::read_to_string(p).ok()?;
    match serde_json::from_str::<Workspace>(&raw) {
        Ok(ws) => Some(ws),
        Err(e) => {
            tracing::warn!("workspace file unparseable, using default layout: {e}");
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Named / multiple saved layouts (TradingView-style named layouts)
//
// The single `workspace.json` above stays the DEFAULT / auto-saved layout —
// still loaded at startup, still what File → Save/Open Workspace writes/reads.
// Named layouts live ALONGSIDE it as individual files under a sibling
// `layouts/` directory (`<name>.json`, the SAME `Workspace` serde schema as the
// single file, so all the v1/v2 forward-compat above applies to them verbatim).
// A tiny pointer file (`last_layout.txt`) records the last layout explicitly
// loaded/saved-as so a restart can reopen it. Names are sanitized into safe
// filename stems; the stem IS the displayed layout name (a lossless,
// path-injection-proof mapping).
// ---------------------------------------------------------------------------

/// The `layouts/` sub-directory name and the pointer file's basename, stated once.
const LAYOUTS_SUBDIR: &str = "layouts";
const LAST_LAYOUT_FILE: &str = "last_layout.txt";

/// The named-layout family's base directory: `$VIKE_WORKSPACE`'s directory — the override moves
/// the whole family together, which is why it short-circuits — else [`state_dir`].
///
/// ONE base for `layouts/`, `last_layout.txt` AND the backend registry's `backends.json`
/// (`crate::backend::backend_registry`, which is why this is `pub(crate)`), so the family can never
/// half-move. `None` when neither resolves, which every member below reports as "no layouts"
/// rather than reaching for a directory of its own.
pub(crate) fn base_dir() -> Option<PathBuf> {
    match workspace_override() {
        Some(p) => Some(p.parent().map(|d| d.to_path_buf()).unwrap_or_default()),
        None => state_dir(),
    }
}

/// Directory named-layout files live in: `<base_dir>/layouts/`.
pub fn layouts_dir() -> Option<PathBuf> {
    Some(base_dir()?.join(LAYOUTS_SUBDIR))
}

/// Pointer file recording the last layout name explicitly loaded or saved-as,
/// so a restart can reopen it: `<base_dir>/last_layout.txt`.
fn last_layout_path() -> Option<PathBuf> {
    Some(base_dir()?.join(LAST_LAYOUT_FILE))
}

/// Sanitize a user-entered layout name into a safe filename STEM: keep
/// alphanumerics, dash, underscore and space; every other character (path
/// separators, dots, control chars, …) becomes `_`; collapse internal
/// whitespace runs to a single space; trim leading/trailing space/underscore/
/// dot; cap the length. Returns `""` for a name that sanitizes to nothing (the
/// empty/whitespace-only case) — callers treat that as "no valid name".
pub fn sanitize_layout_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut prev_space = false;
    for ch in name.trim().chars() {
        let keep = if ch.is_alphanumeric() || ch == '-' || ch == '_' {
            ch
        } else if ch.is_whitespace() {
            ' '
        } else {
            '_'
        };
        // collapse runs of spaces (only spaces — not underscores/dashes)
        if keep == ' ' {
            if prev_space {
                continue;
            }
            prev_space = true;
        } else {
            prev_space = false;
        }
        out.push(keep);
        if out.chars().count() >= 64 {
            break;
        }
    }
    out.trim_matches(|c: char| c == ' ' || c == '_' || c == '.').to_string()
}

/// Absolute path of the named layout `name`, or `None` if the name sanitizes to nothing
/// (empty/whitespace/all-punctuation) or no [`layouts_dir`] resolves.
///
/// The same path for reads and writes — this is where a layout of that name lives.
pub fn layout_path(name: &str) -> Option<PathBuf> {
    let stem = sanitize_layout_name(name);
    if stem.is_empty() {
        return None;
    }
    Some(layouts_dir()?.join(format!("{stem}.json")))
}

/// The saved named layouts (sanitized stems), sorted case-insensitively.
/// A missing directory ⇒ empty list (never an error — the feature is simply unused yet).
/// Non-`.json` entries and unreadable names are skipped.
pub fn list_layouts() -> Vec<String> {
    layouts_dir().map(|d| list_layouts_in(&d)).unwrap_or_default()
}

fn list_layouts_in(dir: &std::path::Path) -> Vec<String> {
    let mut names: Vec<String> = match std::fs::read_dir(dir) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("json"))
            .filter_map(|e| e.path().file_stem().and_then(|s| s.to_str()).map(String::from))
            .collect(),
        Err(_) => Vec::new(),
    };
    names.sort_by_key(|n| n.to_lowercase());
    names.dedup();
    names
}

/// Save the current layout as the named layout `name`, creating `layouts/` if
/// needed. Returns the path written, or an error (invalid name, or IO). The
/// name is also recorded as the last-active layout (best-effort).
pub fn save_layout(
    name: &str,
    wins: &[WinState],
    display_tz: DisplayTz,
    of_backfill_hours: f64,
    gpu_render: bool,
    indicator_favs: &[String],
) -> std::io::Result<PathBuf> {
    let p = layout_path(name).ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "empty/invalid layout name")
    })?;
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir)?;
    }
    save_to(wins, display_tz, of_backfill_hours, gpu_render, indicator_favs, &p)?;
    set_last_layout(name);
    Ok(p)
}

/// Load the named layout `name`; `None` if the name is invalid, the file is
/// absent, or it is unparseable (same never-brick contract as [`load`]). Records
/// the name as last-active on a successful load (best-effort).
pub fn load_layout(name: &str) -> Option<Workspace> {
    let p = layout_path(name)?;
    let ws = load_from(&p);
    if ws.is_some() {
        set_last_layout(name);
    } else if !p.exists() {
        tracing::warn!("layout '{}' not found at {}", name, p.display());
    }
    ws
}

/// Delete the named layout `name`. `Ok(())` if the file was removed OR was
/// already absent (idempotent); an invalid name is an error. Clears the
/// last-active pointer if it named the deleted layout.
pub fn delete_layout(name: &str) -> std::io::Result<()> {
    let stem = sanitize_layout_name(name);
    if stem.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "empty/invalid layout name",
        ));
    }
    if let Some(p) = layout_path(&stem) {
        match std::fs::remove_file(&p) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    if last_layout().as_deref() == Some(stem.as_str())
        && let Some(p) = last_layout_path()
    {
        let _ = std::fs::remove_file(p);
    }
    Ok(())
}

/// Record `name` (sanitized) as the last explicitly-used layout. Best-effort:
/// a write failure is logged, never propagated (it must not fail a save/load).
fn set_last_layout(name: &str) {
    let stem = sanitize_layout_name(name);
    if stem.is_empty() {
        return;
    }
    let Some(p) = last_layout_path() else { return };
    if let Err(e) = std::fs::write(p, &stem) {
        tracing::warn!("could not record last layout: {e}");
    }
}

/// The last explicitly-used layout name, if the pointer file exists AND that
/// layout still exists on disk (a stale pointer to a deleted layout is ignored).
pub fn last_layout() -> Option<String> {
    let raw = std::fs::read_to_string(last_layout_path()?).ok()?;
    let name = raw.trim();
    let stem = sanitize_layout_name(name);
    if stem.is_empty() {
        return None;
    }
    match layout_path(&stem) {
        Some(p) if p.exists() => Some(stem),
        _ => None,
    }
}

#[path = "persist_tests.rs"]
#[cfg(test)]
mod persist_tests;
