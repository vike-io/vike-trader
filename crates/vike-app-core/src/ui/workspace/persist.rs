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
//! `vike_paths::state_path` for the resolution), is loaded
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
use vike_chart::chart::style::ChartStyle;
use vike_chart::panes::PaneKey;
use vike_chart::scale::ScaleAssign;
use vike_chart::{ChartOptions, DisplayTz, ScaleMode};
use vike_model::AssetClass;

mod layouts;
mod store;

pub(crate) use layouts::base_dir;
pub use layouts::{
    delete_layout, last_layout, layout_path, layouts_dir, list_layouts, load_layout,
    sanitize_layout_name, save_layout,
};
#[cfg(doc)]
use store::save_to;
pub use store::{declare_project_state_dir, load, path, save};

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
    /// flat `Vec<usize>`) because a series pane may hold more than one compare symbol
    /// (the since-deleted `WinState::move_series`'s `Into` target merged them,
    /// mirroring `move_study`), so a flat encoding would silently explode a merged pane
    /// back into one-pane-per-symbol on reload. Uid/id-independent for the same reason
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

/// One saved workspace file. ⚠ A RETIRED key just stops being read (decision 0117): an old file
/// still carrying one (`of_backfill_hours`, the chart's deleted backfill-hours knob) loads because
/// serde skips an unknown field, so this struct must never take `deny_unknown_fields`.
/// `persist_tests/snapshot.rs`'s `an_old_workspace_with_the_retired_backfill_hours_key_still_loads` pins it.
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
    /// GPU candle-layer render toggle (GPU Phase 2, Task 3), GLOBAL like `display_tz` above (one
    /// knob, not per-window — see `main.rs`'s `App::gpu_render` doc). `#[serde(default)]` (bool's zero value already IS the wanted default) so a
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
        let pane = vike_chart::panes::PaneKey::Series(w.next_series_pane_id);
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
/// global `display_tz` (task A6) and `gpu_render` (GPU Phase 2 Task 3).
pub fn capture(wins: &[WinState], display_tz: DisplayTz, gpu_render: bool) -> Workspace {
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

#[path = "persist_tests.rs"]
#[cfg(test)]
mod persist_tests;
