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
//! `vike_model::state_path` for the resolution), is loaded
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

use super::state::{WinKind, WinState, DEFAULT_VENUE};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use vike_catalog::AssetClass;
use vike_chart::chart::{ChartOptions, ChartStyle, PaneKey, ScaleAssign};
use vike_chart::{DisplayTz, ScaleMode};

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
                if let Some(&pk) = study_alloc.get(*ord as usize) {
                    if !new_order.contains(&pk) {
                        new_order.push(pk);
                    }
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
            w.retitle(); // fold the venue prefix into the title for a non-binance restored window
            w.style = ChartStyle::ALL.get(s.style).copied().unwrap_or(ChartStyle::Candles);
            w.open = s.open;
            w.minimized = s.minimized;
            w.maximized = s.maximized;
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
                        o.color = egui::Color32::from_rgb(rgb[0], rgb[1], rgb[2]);
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

/// The project's STATE directory — `<project>/settings/state` — or `None` when no project sits
/// above the working directory.
///
/// The env-reading half of `vike_model::state_path` (which is pure) — the same split
/// `vike_backfill::cli::resolve` uses for the store root. This crate is a LIBRARY, so this read
/// carries a `Layer::Library` row in `vike_ops::settings::SETTINGS` and joins the STEP-2 work-list
/// the whole `persist::path` family is already on (see that registry's module doc: this family's
/// lift needs a value threaded through ~10 `vike-app` call sites and is its own PR).
fn state_dir() -> Option<PathBuf> {
    // `VIKE_STATE_ROOT` wins — an operator naming a path explicitly means it. Otherwise it is the
    // PROJECT's `settings/state`, so program-written JSON lands in the same folder as every other
    // setting. There is no third location.
    if let Some(explicit) = std::env::var("VIKE_STATE_ROOT").ok().filter(|s| !s.trim().is_empty()) {
        return Some(PathBuf::from(explicit));
    }
    std::env::current_dir().ok().and_then(|cwd| vike_model::state_path::project_state_dir(&cwd))
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
        None => vike_model::state_path::write_path(state_dir().as_deref(), WORKSPACE_FILE),
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
/// (`crate::backend_registry`, which is why this is `pub(crate)`), so the family can never
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
    if last_layout().as_deref() == Some(stem.as_str()) {
        if let Some(p) = last_layout_path() {
            let _ = std::fs::remove_file(p);
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{pos2, vec2, Rect};
    use vike_chart::chart::MoveTarget;

    fn chart_win(sym: &str, style: ChartStyle) -> WinState {
        let r = Rect::from_min_size(pos2(40.0, 30.0), vec2(700.0, 440.0));
        let mut w = WinState::new(&format!("t-{sym}"), sym, "1m", WinKind::Chart, r);
        w.style = style;
        w.options.show_volume = false;
        w.follow.on = false;
        w.add_indicator("sma", &[]);
        w.add_indicator("macd", &[]);
        if let Some(a) = w.indicators.last_mut() {
            a.visible = false;
        }
        w
    }

    #[test]
    fn capture_apply_round_trip() {
        let mut wins = vec![
            chart_win("BTCUSDT", ChartStyle::Candles),
            chart_win("ETHUSDT", ChartStyle::HeikinAshi),
        ];
        wins[0].sync_group = Some(2); // task B8: one grouped, one ungrouped — both must round-trip
                                      // SP2 orderflow (Task 7): one window with orderflow on + a pinned tick size, one
                                      // left at the all-default off state — both shapes must round-trip.
        wins[0].cvd_on = true;
        wins[0].profile_on = true;
        wins[0].of_tick_size = Some(2.5);
        // SP3 Task 3: a non-default value proves this isn't just always reading back 2.0.
        // GPU Phase 2 Task 3: `gpu_render: true` proves this isn't just always reading back false.
        let ws = capture(&wins, DisplayTz::Utc, 5.0, true);
        assert_eq!(ws.version, VERSION);
        assert_eq!(ws.display_tz, "UTC");
        assert_eq!(ws.of_backfill_hours, 5.0);
        assert!(ws.gpu_render);
        assert_eq!(ws.wins.len(), 2);
        assert_eq!(ws.wins[0].symbol, "BTCUSDT");
        assert_eq!(ws.wins[0].pos, (40.0, 30.0));
        assert_eq!(ws.wins[0].size, (700.0, 440.0));
        assert!(!ws.wins[0].options.show_volume);
        assert!(!ws.wins[0].follow);
        assert_eq!(ws.wins[0].sync_group, Some(2));
        assert_eq!(ws.wins[1].sync_group, None);
        assert!(ws.wins[0].cvd_on);
        assert!(ws.wins[0].profile_on);
        assert_eq!(ws.wins[0].of_tick_size, Some(2.5));
        assert!(!ws.wins[1].cvd_on);
        assert!(!ws.wins[1].profile_on);
        assert_eq!(ws.wins[1].of_tick_size, None);
        let inds: Vec<(&str, bool)> =
            ws.wins[0].indicators.iter().map(|i| (i.name.as_str(), i.visible)).collect();
        assert_eq!(inds, vec![("sma", true), ("macd", false)]);

        // JSON round-trip is lossless
        let json = serde_json::to_string(&ws).unwrap();
        let back: Workspace = serde_json::from_str(&json).unwrap();
        assert_eq!(back, ws);

        // apply → capture is a fixed point
        let rebuilt = apply(&back);
        assert_eq!(rebuilt.len(), 2);
        assert_eq!(rebuilt[0].symbol, "BTCUSDT");
        assert_eq!(rebuilt[1].style, ChartStyle::HeikinAshi);
        assert_eq!(rebuilt[0].indicators.len(), 2);
        assert!(!rebuilt[0].indicators[1].visible);
        assert!(!rebuilt[0].options.show_volume);
        assert!(!rebuilt[0].follow.on);
        assert_eq!(rebuilt[0].sync_group, Some(2));
        assert_eq!(rebuilt[1].sync_group, None);
        assert!(rebuilt[0].cvd_on);
        assert!(rebuilt[0].profile_on);
        assert_eq!(rebuilt[0].of_tick_size, Some(2.5));
        assert!(!rebuilt[1].cvd_on);
        assert!(!rebuilt[1].profile_on);
        assert_eq!(rebuilt[1].of_tick_size, None);
        assert_eq!(capture(&rebuilt, DisplayTz::Utc, ws.of_backfill_hours, ws.gpu_render), ws);
    }

    #[test]
    fn disk_round_trip_and_corrupt_file() {
        // ⚠ The directory is UNIQUE per run, and that is a CI-reliability property rather than a
        // tidiness one. This test used a fixed `<temp>/vike-layout-test`; CI runs as one user and
        // agents as another ON THE SAME BOX, whichever creates the directory first OWNS it, and
        // every later `create_dir_all` under the other user then returns `PermissionDenied` —
        // forever, because nothing ever cleans `/tmp`. Reproduced on the CI box by pre-creating the
        // path as the other user. `crates/vike-ops/tests/temp_path_gate.rs` is the gate.
        //
        // ⚠ The `TempDir` is BOUND, not discarded: `tempfile::tempdir().unwrap().path()` drops the
        // guard at the end of the statement and deletes the directory before the first write.
        let tmp = tempfile::tempdir().expect("temp dir");
        let dir = tmp.path();
        let p = dir.join("workspace.json");
        let wins = vec![chart_win("BTCUSDT", ChartStyle::Candles)];
        save_to(&wins, DisplayTz::Local, 4.0, true, &[], &p).unwrap();
        let ws = load_from(&p).expect("file written above");
        assert_eq!(ws, capture(&wins, DisplayTz::Local, 4.0, true));
        // corrupt file → None, never a panic (startup must survive it)
        std::fs::write(&p, "{not json").unwrap();
        assert!(load_from(&p).is_none());
        assert!(load_from(&dir.join("absent.json")).is_none());
        // No `remove_dir_all`: dropping `tmp` removes the tree, on the panic path too.
    }

    #[test]
    fn indicator_favs_round_trip_and_old_file_defaults_empty() {
        // Feature part (b): favourites written by `save_to` survive a disk round-trip,
        // and a file with NO `indicator_favs` key at all (pre-feature) loads empty.
        let dir = std::env::temp_dir().join(format!("vike-favs-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("workspace.json");
        let wins = vec![chart_win("BTCUSDT", ChartStyle::Candles)];
        let favs = vec!["rsi".to_string(), "macd".to_string()];

        save_to(&wins, DisplayTz::Local, 2.0, false, &favs, &p).unwrap();
        let ws = load_from(&p).expect("file written above");
        assert_eq!(ws.indicator_favs, favs, "favourites round-trip verbatim, in order");

        // An OLD file with no `indicator_favs` key still parses, defaulting to empty.
        let raw = std::fs::read_to_string(&p).unwrap();
        assert!(raw.contains("indicator_favs"));
        let stripped: serde_json::Value = {
            let mut v: serde_json::Value = serde_json::from_str(&raw).unwrap();
            v.as_object_mut().unwrap().remove("indicator_favs");
            v
        };
        let back: Workspace = serde_json::from_value(stripped).unwrap();
        assert!(back.indicator_favs.is_empty(), "pre-feature file ⇒ empty favourites");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn asset_class_round_trips_and_old_file_defaults_none() {
        // Feed-routing slice 1: `WinSnap::asset_class` is the persisted twin of
        // `WinState::asset_class`. A window with a derivative asset class must survive a
        // disk round-trip, AND an old file with no `asset_class` key at all (pre-feature)
        // must still load, defaulting to `None` (spot/legacy — byte-identical behavior).
        let dir =
            std::env::temp_dir().join(format!("vike-asset-class-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("workspace.json");

        let mut wins = vec![chart_win("BTC-USDT-SWAP", ChartStyle::Candles)];
        wins[0].venue = "okx".into();
        wins[0].asset_class = Some(AssetClass::CryptoPerp);
        save_to(&wins, DisplayTz::Local, 2.0, false, &[], &p).unwrap();

        let ws = load_from(&p).expect("file written above");
        assert_eq!(ws.wins[0].asset_class, Some(AssetClass::CryptoPerp));
        let rebuilt = apply(&ws);
        assert_eq!(rebuilt[0].asset_class, Some(AssetClass::CryptoPerp));

        // An OLD file with no `asset_class` key at all still parses, defaulting to None.
        let raw = std::fs::read_to_string(&p).unwrap();
        assert!(raw.contains("asset_class"));
        let stripped: serde_json::Value = {
            let mut v: serde_json::Value = serde_json::from_str(&raw).unwrap();
            v["wins"][0].as_object_mut().unwrap().remove("asset_class");
            v
        };
        let back: Workspace = serde_json::from_value(stripped).unwrap();
        assert_eq!(back.wins[0].asset_class, None, "pre-feature file ⇒ None (spot/legacy)");

        // An ordinary spot window (asset_class: None) never writes the key at all
        // (`skip_serializing_if`), so an existing all-spot workspace file is untouched.
        let spot = vec![chart_win("BTCUSDT", ChartStyle::Candles)];
        let spot_json =
            serde_json::to_string(&capture(&spot, DisplayTz::Local, 2.0, false)).unwrap();
        assert!(!spot_json.contains("asset_class"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tool_windows_are_skipped() {
        let r = Rect::from_min_size(pos2(0.0, 0.0), vec2(300.0, 200.0));
        let wins = vec![
            chart_win("BTCUSDT", ChartStyle::Candles),
            WinState::new("trade", "", "", WinKind::Trade, r),
        ];
        assert_eq!(capture(&wins, DisplayTz::Local, 2.0, false).wins.len(), 1);
    }

    #[test]
    fn unknown_style_and_indicator_degrade_gracefully() {
        let ws = Workspace {
            version: VERSION,
            display_tz: "Local".into(),
            of_backfill_hours: 2.0,
            gpu_render: false,
            indicator_favs: Vec::new(),
            wins: vec![WinSnap {
                kind: "chart".into(),
                symbol: "BTCUSDT".into(),
                interval: "1m".into(),
                venue: "binance".into(),
                asset_class: None,
                style: 999, // out of range → Candles
                pos: (0.0, 0.0),
                size: (100.0, 100.0),
                open: true,
                minimized: false,
                maximized: false,
                follow: true,
                auto_y: true,
                scale: ScaleMode::default(),
                invert: false,
                options: ChartOptions::default(),
                panes: Vec::new(),
                pane_membership: Vec::new(),
                sub_pane_order: Vec::new(),
                sync_group: None,
                cvd_on: false,
                profile_on: false,
                of_tick_size: None,
                compare: Vec::new(),
                series_panes: Vec::new(),
                series_scale: Vec::new(),
                indicators: vec![IndSnap {
                    name: "no-such-indicator".into(),
                    visible: true,
                    params: Vec::new(),
                    lines: Vec::new(),
                    source: None,
                }],
            }],
        };
        let rebuilt = apply(&ws);
        assert_eq!(rebuilt[0].style, ChartStyle::Candles);
        assert!(rebuilt[0].indicators.is_empty()); // unknown name silently dropped
    }

    /// (a) v1 forward-compat: a checked-in v1 JSON (version 1, top-level
    /// NON-default `show_volume:false`, an indicator with NO `params`/`lines`)
    /// must still load under the v2 structs — `show_volume` flows into the
    /// flattened `options.show_volume`, and every ABSENT `ChartOptions` field
    /// takes its CUSTOM `Default` value (not a type-zero). This is the
    /// flatten + struct-level `#[serde(default)]` gate.
    #[test]
    fn v1_json_loads_with_flatten_and_custom_defaults() {
        const V1: &str = r#"{
            "version": 1,
            "wins": [{
                "kind": "chart",
                "symbol": "BTCUSDT",
                "interval": "1m",
                "style": 0,
                "pos": [40.0, 30.0],
                "size": [700.0, 440.0],
                "open": true,
                "minimized": false,
                "maximized": false,
                "show_volume": false,
                "follow": false,
                "auto_y": true,
                "indicators": [{ "name": "sma", "visible": true }]
            }]
        }"#;
        let ws: Workspace =
            serde_json::from_str(V1).expect("a v1 JSON must parse under the v2 structs");
        // task A6: a pre-A6 file has no `display_tz` key — must default to "Local", never brick.
        assert_eq!(ws.display_tz, "Local");
        // SP3 Task 3: a pre-SP3 file has no `of_backfill_hours` key — must default to 2.0.
        assert_eq!(ws.of_backfill_hours, 2.0);
        // GPU Phase 2 Task 3: a pre-Task-3 file has no `gpu_render` key — must default to false.
        assert!(!ws.gpu_render);
        let win = &ws.wins[0];
        // the v1 top-level show_volume flowed into the flattened options
        assert!(!win.options.show_volume, "v1 show_volume:false must survive the flatten");
        // absent color/flag/precision fields defaulted to the CUSTOM values, not zeros
        let d = ChartOptions::default();
        assert_eq!(win.options.up, d.up, "absent `up` must default to {:?}, not [0,0,0]", d.up);
        assert_eq!(win.options.down, d.down);
        assert_eq!(win.options.cross, d.cross);
        assert_eq!(win.options.precision, d.precision);
        assert!(win.options.show_grid, "absent show_grid must default on");
        // absent scale/panes default; the indicator loads with default (empty) params
        assert_eq!(win.scale, ScaleMode::Linear);
        assert!(win.panes.is_empty());
        assert!(win.indicators[0].params.is_empty());
        assert!(win.indicators[0].lines.is_empty());
        // task B8: absent `sync_group` key defaults to None (ungrouped) — a v1 file
        // predates sync groups entirely, so this exercises the same forward-compat path.
        assert_eq!(win.sync_group, None);
        // SP2 orderflow (Task 7): absent `cvd_on`/`profile_on`/`of_tick_size` keys default to
        // false/false/None — a v1 file predates orderflow entirely, same forward-compat path.
        assert!(!win.cvd_on);
        assert!(!win.profile_on);
        assert_eq!(win.of_tick_size, None);
        // Cross-venue: a v1 file predates the `venue` key entirely, so it defaults to Binance —
        // every existing workspace restores byte-identically to the single-venue behavior.
        assert_eq!(win.venue, "binance");

        // and apply carries the custom defaults onto a real WinState
        let rebuilt = apply(&ws);
        assert!(!rebuilt[0].options.show_volume);
        assert_eq!(rebuilt[0].options.up, d.up);
        assert_eq!(rebuilt[0].scale, ScaleMode::Linear);
        assert_eq!(rebuilt[0].sync_group, None);
        assert!(!rebuilt[0].cvd_on);
        assert!(!rebuilt[0].profile_on);
        assert_eq!(rebuilt[0].of_tick_size, None);
        assert_eq!(rebuilt[0].venue, "binance"); // default venue on a real WinState too
    }

    /// (b) v2 fixed point ACROSS A UID CHANGE: a window with a non-default scale,
    /// custom `ChartOptions`, a dragged `PaneFractions` (incl. a Study pane) and an
    /// oscillator carrying non-default params + a per-line paint edit survives
    /// capture→serialize→parse→apply→capture unchanged. The `apply` step rebuilds
    /// indicators through `add_indicator` (assigning NEW uids), so an equal
    /// re-capture proves the Study-pane index↔uid translation survives a uid change
    /// — persisting uids verbatim would re-capture a DIFFERENT Study key and fail.
    #[test]
    fn v2_round_trip_is_a_fixed_point_across_a_uid_change() {
        let r = Rect::from_min_size(pos2(10.0, 10.0), vec2(800.0, 500.0));
        let mut w = WinState::new("fp", "BTCUSDT", "1m", WinKind::Chart, r);
        // non-default scale + custom options (custom up color, precision, volume off)
        w.scale = ScaleMode::Log;
        w.options.up = [1, 2, 3];
        w.options.precision = Some(4);
        w.options.show_volume = false;
        // Seed the oscillator at a NON-zero uid so a fresh rebuild (uids from 0)
        // genuinely changes it: add a throwaway overlay (uid 0), the oscillator
        // (uid 1), then drop the overlay → indicators == [rsi @ uid 1].
        w.add_indicator("sma", &[]); // overlay, uid 0
        w.add_indicator("rsi", &[]); // oscillator, uid 1
        let osc_uid = w.indicators.last().unwrap().uid;
        assert_eq!(osc_uid, 1, "precondition: oscillator seeded at uid 1");
        w.remove_indicator(0);
        assert_eq!(w.indicators.len(), 1, "only the oscillator remains");
        // non-default params + a per-line paint edit on the oscillator
        w.indicators[0].set_params(vec![9.0], &[]);
        w.indicators[0].outputs[0].color = egui::Color32::from_rgb(7, 8, 9);
        w.indicators[0].outputs[0].width = 3.25;
        // a dragged pane layout referencing the oscillator by its (uid 1) PaneKey
        let present = [PaneKey::Price, PaneKey::Study(osc_uid)];
        w.panes.layout(&present, 500.0, 44.0);
        w.panes.drag(&present, 0, 40.0, 500.0, 44.0);

        let first = capture(std::slice::from_ref(&w), DisplayTz::Local, 3.0, false);
        // the captured pane's Study is keyed by INDEX 0, NOT the uid (1)
        assert!(
            first.wins[0].panes.iter().any(|(p, _)| *p == PaneKey::Study(0)),
            "osc pane must be stored by index 0, got {:?}",
            first.wins[0].panes
        );
        assert!(
            !first.wins[0].panes.iter().any(|(p, _)| *p == PaneKey::Study(osc_uid)),
            "osc pane must NOT be stored by the volatile uid {osc_uid}"
        );

        let json = serde_json::to_string(&first).unwrap();
        let parsed: Workspace = serde_json::from_str(&json).unwrap();
        let rebuilt = apply(&parsed);
        // the rebuilt oscillator got a DIFFERENT uid than the original (0 vs 1)
        let new_uid = rebuilt[0].indicators[0].uid;
        assert_ne!(new_uid, osc_uid, "rebuild must reassign the oscillator uid");

        let second = capture(&rebuilt, DisplayTz::Local, 3.0, false);
        assert_eq!(second.wins[0].scale, first.wins[0].scale);
        assert_eq!(second.wins[0].options, first.wins[0].options);
        assert_eq!(second.wins[0].panes, first.wins[0].panes);
        assert_eq!(second.wins[0].indicators, first.wins[0].indicators);
        assert_eq!(second, first, "full v2 round-trip must be a fixed point");
    }

    /// Foreign-source study (TradingView "symbol" input): a study's `(venue, symbol)`
    /// source round-trips capture→JSON→apply, and a study WITHOUT a source captures no
    /// `source` key at all (skip_serializing_if) and rebuilds as `None` — the
    /// byte-identical same-symbol default.
    #[test]
    fn foreign_source_symbol_round_trips_and_defaults_to_none() {
        use vike_chart::indicators::SourceSymbol;
        let r = Rect::from_min_size(pos2(10.0, 10.0), vec2(800.0, 500.0));
        let mut w = WinState::new("src", "BTCUSDT", "1m", WinKind::Chart, r);
        w.add_indicator("sma", &[]); // overlay, computes off a FOREIGN symbol
        w.add_indicator("rsi", &[]); // oscillator, stays on the primary (no source)
        w.indicators[0].source_symbol =
            Some(SourceSymbol { venue: "bybit".into(), symbol: "ETHUSDT".into() });

        let cap = capture(std::slice::from_ref(&w), DisplayTz::Local, 2.0, false);
        // the sourced study carries the pair; the primary-sourced study carries None
        assert_eq!(cap.wins[0].indicators[0].source, Some(("bybit".into(), "ETHUSDT".into())));
        assert_eq!(cap.wins[0].indicators[1].source, None);

        let json = serde_json::to_string(&cap).unwrap();
        // a None source emits NO `source` key (skip_serializing_if) — save stays clean
        // and byte-identical to a pre-feature file for ordinary studies.
        assert_eq!(json.matches("\"source\"").count(), 1, "only the sourced study writes a key");

        let rebuilt = apply(&serde_json::from_str::<Workspace>(&json).unwrap());
        assert_eq!(
            rebuilt[0].indicators[0].source_symbol,
            Some(SourceSymbol { venue: "bybit".into(), symbol: "ETHUSDT".into() }),
            "foreign source survives the rebuild",
        );
        assert_eq!(rebuilt[0].indicators[1].source_symbol, None, "primary study stays sourceless");

        // an OLD file (no `source` key on any indicator) loads every study as primary.
        let legacy = json.replace(",\"source\":[\"bybit\",\"ETHUSDT\"]", "");
        let old = apply(&serde_json::from_str::<Workspace>(&legacy).unwrap());
        assert!(old[0].indicators.iter().all(|a| a.source_symbol.is_none()));
    }

    /// TradingView "Invert scale": the persisted `invert` flag round-trips, and an
    /// OLD file with no `invert` key defaults to `false` (byte-identical to
    /// pre-invert), same `#[serde(default)]` discipline as `scale`.
    #[test]
    fn invert_flag_round_trips_and_defaults_to_false_when_absent() {
        let r = Rect::from_min_size(pos2(10.0, 10.0), vec2(800.0, 500.0));
        let mut w = WinState::new("inv", "BTCUSDT", "1m", WinKind::Chart, r);
        assert!(!w.invert, "fresh window is not inverted");
        w.invert = true;
        w.scale = ScaleMode::Log; // invert is orthogonal to the mode
        let json =
            serde_json::to_string(&capture(std::slice::from_ref(&w), DisplayTz::Local, 2.0, false))
                .unwrap();
        assert!(
            serde_json::from_str::<serde_json::Value>(&json).unwrap()["wins"][0]
                .as_object()
                .unwrap()
                .contains_key("invert"),
            "invert must be a top-level key: {json}"
        );
        let parsed: Workspace = serde_json::from_str(&json).unwrap();
        let rebuilt = apply(&parsed);
        assert!(rebuilt[0].invert, "invert flag must survive the round-trip");
        assert_eq!(rebuilt[0].scale, ScaleMode::Log, "mode survives alongside invert");

        // an OLD file that predates the invert key loads false (serde default).
        let stripped = json.replace(",\"invert\":true", "");
        assert!(!stripped.contains("\"invert\""), "precondition: invert key removed");
        let old: Workspace = serde_json::from_str(&stripped).unwrap();
        assert!(!apply(&old)[0].invert, "absent invert key defaults to false");
    }

    /// (c) no duplicate/nested keys: the flattened `options.show_volume` must be
    /// emitted EXACTLY once (not also as a standalone `WinSnap` key) and never as
    /// a nested `options` object.
    #[test]
    fn v2_serialization_has_no_duplicate_or_nested_option_keys() {
        let wins = vec![chart_win("BTCUSDT", ChartStyle::Candles)];
        let json = serde_json::to_string(&capture(&wins, DisplayTz::Local, 2.0, false)).unwrap();
        // raw-string guard: one window → `show_volume` appears exactly once
        assert_eq!(
            json.matches("\"show_volume\"").count(),
            1,
            "show_volume must be emitted once (flattened), not duplicated: {json}"
        );
        let val: serde_json::Value = serde_json::from_str(&json).unwrap();
        let win = val["wins"][0].as_object().unwrap();
        assert!(win.contains_key("show_volume"), "flattened show_volume at top level");
        assert!(!win.contains_key("options"), "options must be flattened, not nested");
        // a couple of other flattened/added keys confirm the shape
        assert!(win.contains_key("up"), "flattened color key at top level");
        assert!(win.contains_key("scale"));
        assert!(win.contains_key("panes"));
        assert!(win.contains_key("sync_group"), "task B8: sync_group is a top-level key");
    }

    /// (d) task A6: `display_tz` persists by NAME and round-trips through `DisplayTz::parse`
    /// for a curated NAMED (non Local/UTC) zone — exercises the IANA-string arm, not just the
    /// two hardcoded `"Local"`/`"UTC"` arms.
    #[test]
    fn display_tz_round_trips_through_name_and_parse() {
        let tz = DisplayTz::CURATED
            .into_iter()
            .find(|tz| matches!(tz, DisplayTz::Named(_)))
            .expect("CURATED has at least one Named zone");
        let wins = vec![chart_win("BTCUSDT", ChartStyle::Candles)];
        let ws = capture(&wins, tz, 2.0, false);
        assert_eq!(ws.display_tz, tz.name());
        let json = serde_json::to_string(&ws).unwrap();
        let back: Workspace = serde_json::from_str(&json).unwrap();
        assert_eq!(DisplayTz::parse(&back.display_tz), tz);
    }

    /// (e) a pre-A6 workspace file (no `display_tz` key at all, not just v1's other-absent-
    /// fields shape) loads with the default `"Local"` — never bricks an old save.
    #[test]
    fn missing_display_tz_key_defaults_to_local() {
        let json = format!(r#"{{"version":{VERSION},"wins":[]}}"#);
        let ws: Workspace =
            serde_json::from_str(&json).expect("a workspace with no display_tz key must parse");
        assert_eq!(ws.display_tz, "Local");
        assert_eq!(DisplayTz::parse(&ws.display_tz), DisplayTz::Local);
        // SP3 Task 3: same "never bricks an old save" guard for `of_backfill_hours`.
        assert_eq!(ws.of_backfill_hours, 2.0);
        // GPU Phase 2 Task 3: same "never bricks an old save" guard for `gpu_render`.
        assert!(!ws.gpu_render);
    }

    /// (f) C1 Task 5: pane MEMBERSHIP round-trips across a uid reassignment, the
    /// same way T9/T10's heights already did. Merge three oscillators into one
    /// pane, capture→JSON→parse→apply (the rebuild reassigns fresh uids — proven
    /// below, mirroring `v2_round_trip_is_a_fixed_point_across_a_uid_change`'s
    /// throwaway-overlay trick), and assert the merge survives: all three
    /// oscillators share ONE pane again, not three.
    #[test]
    fn c1_pane_membership_round_trips_across_uid_reassignment() {
        let r = Rect::from_min_size(pos2(10.0, 10.0), vec2(800.0, 500.0));
        let mut w = WinState::new("pm", "BTCUSDT", "1m", WinKind::Chart, r);
        // A throwaway overlay first (uid 0, discarded before capture) means the
        // REBUILD — which only recreates persisted (surviving) indicators — hands
        // out uids that do NOT match this run's, proving the round-trip doesn't
        // depend on uid stability (same trick as the uid-change test above).
        w.add_indicator("sma", &[]); // overlay, uid 0
        w.add_indicator("rsi", &[]); // uid 1
        w.add_indicator("rsi", &[]); // uid 2
        w.add_indicator("rsi", &[]); // uid 3
        w.remove_indicator(0);
        let uids: Vec<u64> = w.indicators.iter().map(|a| a.uid).collect();
        assert_eq!(uids, vec![1, 2, 3], "precondition: three oscillators at uids 1..=3");

        // merge #2 and #3 into #1's pane, leaving one pane with all three members
        let p0 = w.study_pane[&uids[0]];
        w.move_study(uids[1], MoveTarget::Into(p0));
        w.move_study(uids[2], MoveTarget::Into(p0));
        assert_eq!(w.present_study_panes().len(), 1, "precondition: merged into one pane");

        let snap = capture(std::slice::from_ref(&w), DisplayTz::Local, 2.0, false);
        let json = serde_json::to_string(&snap).unwrap();
        let parsed: Workspace = serde_json::from_str(&json).unwrap();
        let rebuilt = apply(&parsed);
        let w2 = &rebuilt[0];

        // the rebuild assigns DIFFERENT uids (no throwaway overlay this time)
        let new_uids: Vec<u64> = w2.indicators.iter().map(|a| a.uid).collect();
        assert_ne!(new_uids, uids, "rebuild must reassign oscillator uids");

        // membership survives: all three oscillators share ONE pane again
        assert_eq!(w2.present_study_panes().len(), 1);
        assert_eq!(w2.study_pane.len(), 3);
        let key = w2.present_study_panes()[0];
        assert!(w2.study_pane.values().all(|k| *k == key));
    }

    /// (g) C1 Task 5: an old snapshot predating pane membership (no
    /// `pane_membership` key at all — the same "never bricks an old save" shape
    /// as `v1_json_loads_with_flatten_and_custom_defaults`, but for a v2-shaped
    /// file that already has `panes` heights from T9/T10) falls back to one pane
    /// per oscillator, in add order — today's pre-Task-5 default layout — and a
    /// re-capture reproduces the SAME ordinal-keyed heights the old file had,
    /// proving old-file heights are preserved across the fallback.
    #[test]
    fn c1_old_snapshot_without_panes_field_loads_as_one_pane_per_oscillator() {
        const JSON: &str = r#"{
            "version": 2,
            "wins": [{
                "kind": "chart",
                "symbol": "BTCUSDT",
                "interval": "1m",
                "style": 0,
                "pos": [0.0, 0.0],
                "size": [700.0, 440.0],
                "open": true,
                "minimized": false,
                "maximized": false,
                "follow": false,
                "auto_y": true,
                "panes": [[{"Study":0}, 0.5], [{"Study":2}, 0.25]],
                "indicators": [
                    { "name": "rsi", "visible": true },
                    { "name": "rsi", "visible": true },
                    { "name": "rsi", "visible": true }
                ]
            }]
        }"#;
        let ws: Workspace =
            serde_json::from_str(JSON).expect("a v2 JSON with no pane_membership key must parse");
        assert!(
            ws.wins[0].pane_membership.is_empty(),
            "precondition: no pane_membership key in this fixture"
        );

        let rebuilt = apply(&ws);
        let w = &rebuilt[0];
        // fallback: each oscillator in its own pane, in order
        assert_eq!(w.present_study_panes().len(), 3);
        assert!(w.study_pane.values().all(|k| matches!(k, PaneKey::Study(_))));
        let distinct: std::collections::HashSet<PaneKey> = w.study_pane.values().copied().collect();
        assert_eq!(distinct.len(), 3, "one DISTINCT pane per oscillator, not merged");

        // old-file heights (already Study(osc index)-keyed under the default
        // one-pane-per-oscillator layout) still land on the right pane.
        let recaptured = capture(&rebuilt, DisplayTz::Local, 2.0, false);
        let heights = &recaptured.wins[0].panes;
        assert!(heights.contains(&(PaneKey::Study(0), 0.5)));
        assert!(heights.contains(&(PaneKey::Study(2), 0.25)));
    }

    /// (h) C2b Task 8: compare symbols + own-pane placement + per-symbol scale
    /// pin all round-trip together, the same way C1's pane membership survived a
    /// uid reassignment (`c1_pane_membership_round_trips_across_uid_reassignment`).
    /// A throwaway own-pane compare symbol is added then removed FIRST, consuming
    /// `PaneKey::Series(1)`; the surviving own-pane symbol is then `Series(2)`, so
    /// `apply`'s rebuild — which restarts `next_series_pane_id` fresh at 1 — is
    /// proven to assign a DIFFERENT id, yet membership still lands correctly
    /// because it is persisted as an ORDINAL into `compare`, not the volatile id.
    #[test]
    fn c2b_compare_series_pane_and_scale_round_trip_across_pane_id_reassignment() {
        let r = Rect::from_min_size(pos2(10.0, 10.0), vec2(800.0, 500.0));
        let mut w = WinState::new("cs", "BTCUSDT", "1m", WinKind::Chart, r);

        // Throwaway own-pane symbol first — consumes PaneKey::Series(1), then is
        // removed entirely (cascades out of both `compare` and `series_pane`).
        w.add_compare("ADAUSDT");
        w.move_series_to_new_pane("ADAUSDT");
        w.remove_compare("ADAUSDT");

        // ETHUSDT: stays overlaid in the price pane (C2a default), explicitly
        // pinned Percent (also the default — proves a stored-but-default entry
        // round-trips too, not just a stored non-default one).
        w.add_compare("ETHUSDT");
        w.series_scale.insert("ETHUSDT".to_string(), ScaleAssign::Percent);
        // SOLUSDT: moved to its own pane, pinned Right.
        w.add_compare("SOLUSDT");
        w.move_series_to_new_pane("SOLUSDT");
        w.series_scale.insert("SOLUSDT".to_string(), ScaleAssign::Right);

        let original_pane = w.series_pane["SOLUSDT"];
        assert_eq!(
            original_pane,
            PaneKey::Series(2),
            "precondition: the throwaway pane consumed id 1"
        );

        let snap = capture(std::slice::from_ref(&w), DisplayTz::Local, 2.0, false);
        let win = &snap.wins[0];
        assert_eq!(win.compare, vec!["ETHUSDT".to_string(), "SOLUSDT".to_string()]);
        // SOLUSDT is compare-index 1 — the sole member of the one own-pane group.
        assert_eq!(win.series_panes, vec![vec![1]]);
        assert!(win.series_scale.contains(&("ETHUSDT".to_string(), ScaleAssign::Percent)));
        assert!(win.series_scale.contains(&("SOLUSDT".to_string(), ScaleAssign::Right)));

        let json = serde_json::to_string(&snap).unwrap();
        let parsed: Workspace = serde_json::from_str(&json).unwrap();
        let rebuilt = apply(&parsed);
        let w2 = &rebuilt[0];

        assert_eq!(w2.compare, vec!["ETHUSDT".to_string(), "SOLUSDT".to_string()]);
        assert!(!w2.series_pane.contains_key("ETHUSDT"), "ETHUSDT must stay overlaid");
        assert!(w2.series_pane.contains_key("SOLUSDT"), "SOLUSDT must be in its own pane");
        assert_eq!(w2.present_series_panes().len(), 1);
        let new_pane = w2.series_pane["SOLUSDT"];
        assert_ne!(new_pane, original_pane, "rebuild must reassign the series-pane id");
        assert_eq!(w2.series_scale_of("ETHUSDT"), ScaleAssign::Percent);
        assert_eq!(w2.series_scale_of("SOLUSDT"), ScaleAssign::Right);

        // fixed point: re-capture reproduces the same snapshot
        let second = capture(&rebuilt, DisplayTz::Local, 2.0, false);
        assert_eq!(second.wins[0].compare, win.compare);
        assert_eq!(second.wins[0].series_panes, win.series_panes);
        assert_eq!(second.wins[0].series_scale, win.series_scale);
    }

    /// (i) C2b Task 8: an old (pre-C2b) v2 snapshot — no `compare`/`series_panes`/
    /// `series_scale` keys at all — loads with zero compare overlays, the same
    /// "never bricks an old save" shape as
    /// `c1_old_snapshot_without_panes_field_loads_as_one_pane_per_oscillator`, and
    /// re-capturing it reproduces the same empty shape (byte-identical to a chart
    /// that predates the Compare feature entirely).
    #[test]
    fn c2b_old_snapshot_without_compare_fields_loads_with_no_overlays() {
        const JSON: &str = r#"{
            "version": 2,
            "wins": [{
                "kind": "chart",
                "symbol": "BTCUSDT",
                "interval": "1m",
                "style": 0,
                "pos": [0.0, 0.0],
                "size": [700.0, 440.0],
                "open": true,
                "minimized": false,
                "maximized": false,
                "follow": false,
                "auto_y": true,
                "indicators": []
            }]
        }"#;
        let ws: Workspace =
            serde_json::from_str(JSON).expect("a pre-C2b v2 JSON with no compare keys must parse");
        assert!(ws.wins[0].compare.is_empty(), "precondition: no compare key in this fixture");
        assert!(ws.wins[0].series_panes.is_empty());
        assert!(ws.wins[0].series_scale.is_empty());

        let rebuilt = apply(&ws);
        let w = &rebuilt[0];
        assert!(w.compare.is_empty(), "old file must load with zero compare overlays");
        assert!(w.series_pane.is_empty());
        assert!(w.present_series_panes().is_empty());
        assert_eq!(w.series_scale_of("ANYSYM"), ScaleAssign::Percent);

        // re-capture reproduces the same empty shape (byte-identical fixed point)
        let recaptured = capture(&rebuilt, DisplayTz::Local, 2.0, false);
        assert!(recaptured.wins[0].compare.is_empty());
        assert!(recaptured.wins[0].series_panes.is_empty());
        assert!(recaptured.wins[0].series_scale.is_empty());
    }

    /// (j) C2 tidy FIX 2: a Series-pane HEIGHT survives capture→apply, the same way a
    /// Study-pane height already does (`v2_round_trip_is_a_fixed_point_across_a_uid_change`)
    /// and the way Series-pane MEMBERSHIP already did (test (h) above). Before this fix
    /// `panes_to_snap`/`snap_to_panes` had a placeholder `PaneKey::Series(_) => None` arm (a
    /// leftover from C2 Task 5, before `chart::draw` ever fed a live `Series` pane into
    /// `w.panes`), so a dragged own-pane compare series always reloaded at the un-dragged
    /// 0.16 default share. Uses the same throwaway-pane / pane-id-reassignment trick as test
    /// (h) so this also proves the height round-trips by ORDINAL, not the volatile pane id.
    #[test]
    fn c2_series_pane_height_round_trips_across_pane_id_reassignment() {
        let r = Rect::from_min_size(pos2(10.0, 10.0), vec2(800.0, 500.0));
        let mut w = WinState::new("sph", "BTCUSDT", "1m", WinKind::Chart, r);

        // Throwaway own-pane symbol first — consumes PaneKey::Series(1), then is removed
        // entirely (mirrors test (h)'s precondition setup).
        w.add_compare("ADAUSDT");
        w.move_series_to_new_pane("ADAUSDT");
        w.remove_compare("ADAUSDT");

        w.add_compare("SOLUSDT");
        w.move_series_to_new_pane("SOLUSDT");
        let pane = w.series_pane["SOLUSDT"];
        assert_eq!(pane, PaneKey::Series(2), "precondition: the throwaway pane consumed id 1");

        // Drag the pane's height away from its 0.16 default — the same `PaneFractions`
        // `layout`/`drag` calls `chart::draw`'s live separator-drag path makes, exercised
        // directly here like `v2_round_trip_is_a_fixed_point_across_a_uid_change` does for a
        // Study pane.
        let present = [PaneKey::Price, pane];
        w.panes.layout(&present, 500.0, 44.0);
        w.panes.drag(&present, 0, 40.0, 500.0, 44.0);

        let first = capture(std::slice::from_ref(&w), DisplayTz::Local, 2.0, false);
        let stored = first.wins[0]
            .panes
            .iter()
            .find(|(p, _)| matches!(p, PaneKey::Series(_)))
            .copied()
            .expect("a Series pane height must be captured, not dropped");
        assert_eq!(stored.0, PaneKey::Series(0), "captured by ordinal, not the volatile pane id");
        assert!(
            (stored.1 - 0.16).abs() > 0.01,
            "height must be the DRAGGED value, not the 0.16 default, got {}",
            stored.1
        );

        let json = serde_json::to_string(&first).unwrap();
        let parsed: Workspace = serde_json::from_str(&json).unwrap();
        let rebuilt = apply(&parsed);
        let w2 = &rebuilt[0];
        let new_pane = w2.series_pane["SOLUSDT"];
        assert_ne!(new_pane, pane, "rebuild must reassign the series-pane id");

        // fixed point: re-capture reproduces the exact same (non-default) height
        let second = capture(&rebuilt, DisplayTz::Local, 2.0, false);
        assert_eq!(
            second.wins[0].panes, first.wins[0].panes,
            "dragged Series-pane height must survive the round trip"
        );
    }

    /// (k) Chart single-max default: the authored UNIFIED sub-pane order (Volume
    /// reordered BELOW a study pane) survives capture→apply, so the user's
    /// arrangement of Volume/CVD relative to studies is persisted — not reset to
    /// the old hardcoded Volume→CVD→studies sequence. Also proves `apply` does not
    /// let a later `sync_sub_panes` re-prepend Volume (it stays where the user put
    /// it, because it is already present in the restored order).
    #[test]
    fn sub_pane_order_round_trips_volume_below_a_study() {
        let r = Rect::from_min_size(pos2(10.0, 10.0), vec2(800.0, 500.0));
        let mut w = WinState::new("spo", "BTCUSDT", "1m", WinKind::Chart, r);
        w.options.show_volume = true;
        w.add_indicator("rsi", &[]); // oscillator → its own study pane
        w.sync_sub_panes(); // default: [Volume, Study]
        let study = w.present_study_panes()[0];
        assert_eq!(w.pane_order, vec![PaneKey::Volume, study]);
        // Move the study UP above Volume → authored order becomes [Study, Volume].
        w.reorder_pane(study, true);
        assert_eq!(w.present_sub_panes(), vec![study, PaneKey::Volume]);

        let snap = capture(std::slice::from_ref(&w), DisplayTz::Local, 2.0, false);
        // captured with the study by ordinal 0, Volume verbatim, order preserved.
        assert_eq!(snap.wins[0].sub_pane_order, vec![PaneKey::Study(0), PaneKey::Volume]);

        let json = serde_json::to_string(&snap).unwrap();
        let parsed: Workspace = serde_json::from_str(&json).unwrap();
        let mut rebuilt = apply(&parsed);
        let w2 = &mut rebuilt[0];
        // The study pane stays ABOVE Volume after reload.
        let restored = w2.present_sub_panes();
        assert_eq!(restored.len(), 2);
        assert!(matches!(restored[0], PaneKey::Study(_)), "study restored at the TOP slot");
        assert_eq!(restored[1], PaneKey::Volume, "Volume restored BELOW the study");
        // A subsequent per-frame sync must NOT re-prepend Volume to the front.
        w2.sync_sub_panes();
        let after_sync = w2.present_sub_panes();
        assert!(matches!(after_sync[0], PaneKey::Study(_)));
        assert_eq!(after_sync[1], PaneKey::Volume);

        // fixed point: re-capture reproduces the same unified order.
        let second = capture(&rebuilt, DisplayTz::Local, 2.0, false);
        assert_eq!(second.wins[0].sub_pane_order, snap.wins[0].sub_pane_order);
    }

    /// (l) Chart single-max default forward-compat: an OLD save (no
    /// `sub_pane_order` key) loads with the study-only order `pane_membership`
    /// rebuilt, and `sync_sub_panes` then re-slots Volume→CVD at the FRONT — the
    /// exact pre-reorder Volume→CVD→studies sequence, byte-identical to old saves.
    #[test]
    fn old_snapshot_without_sub_pane_order_defaults_volume_cvd_to_front() {
        const JSON: &str = r#"{
            "version": 2,
            "wins": [{
                "kind": "chart",
                "symbol": "BTCUSDT",
                "interval": "1m",
                "style": 0,
                "pos": [0.0, 0.0],
                "size": [700.0, 440.0],
                "open": true,
                "minimized": false,
                "maximized": false,
                "show_volume": true,
                "follow": false,
                "auto_y": true,
                "cvd_on": true,
                "indicators": [ { "name": "rsi", "visible": true } ]
            }]
        }"#;
        let ws: Workspace =
            serde_json::from_str(JSON).expect("a v2 JSON with no sub_pane_order key must parse");
        assert!(ws.wins[0].sub_pane_order.is_empty(), "precondition: no sub_pane_order key");

        let mut rebuilt = apply(&ws);
        let w = &mut rebuilt[0];
        // apply leaves the study-only order (no Volume/CVD spliced in yet).
        assert_eq!(w.pane_order.len(), 1);
        assert!(matches!(w.pane_order[0], PaneKey::Study(_)));
        // sync (as main.rs runs each frame) prepends Volume then CVD at the front.
        w.sync_sub_panes();
        let sub = w.present_sub_panes();
        assert_eq!(sub[0], PaneKey::Volume, "Volume defaults to the front");
        assert_eq!(sub[1], PaneKey::Cvd, "CVD defaults directly after Volume");
        assert!(matches!(sub[2], PaneKey::Study(_)), "study pane follows Volume/CVD");
    }

    // --- named / multiple saved layouts -----------------------------------

    /// (m) filename sanitization: safe stems pass through; path separators,
    /// dots, control/exotic chars become `_`; whitespace collapses + trims;
    /// empty/whitespace/all-punctuation ⇒ `""` (the "no valid name" signal); the
    /// result is bounded in length and never contains a path separator.
    #[test]
    fn sanitize_layout_name_is_path_injection_proof() {
        assert_eq!(sanitize_layout_name("My Scalping Layout"), "My Scalping Layout");
        assert_eq!(sanitize_layout_name("  trimmed  "), "trimmed");
        assert_eq!(sanitize_layout_name("a\t b   c"), "a b c"); // whitespace runs collapse
        assert_eq!(sanitize_layout_name("keep-dash_underscore"), "keep-dash_underscore");
        // path-traversal / separators / dots are neutralized, then edge-trimmed
        assert_eq!(sanitize_layout_name("../../etc/passwd"), "etc_passwd");
        assert_eq!(sanitize_layout_name("a/b\\c"), "a_b_c");
        assert_eq!(sanitize_layout_name(".hidden."), "hidden");
        // empty / whitespace / all-punctuation collapse to the "no valid name" signal
        assert_eq!(sanitize_layout_name(""), "");
        assert_eq!(sanitize_layout_name("   "), "");
        assert_eq!(sanitize_layout_name("...///"), "");
        // never yields a path separator, and stays bounded
        let s = sanitize_layout_name(&"x/".repeat(100));
        assert!(!s.contains('/') && !s.contains('\\'));
        assert!(s.chars().count() <= 64);
        // an invalid name has no layout_path; a valid one does
        assert!(layout_path("   ").is_none());
        assert!(layout_path("Valid Name").is_some());
    }

    /// (n) listing + save/load/delete round-trip through a temp `layouts/` dir,
    /// independent of the process's real workspace dir (uses the `*_in`/`*_to`/
    /// `load_from` seams the public helpers are built on, so no env juggling).
    #[test]
    fn named_layout_save_list_load_delete_round_trip() {
        let dir = std::env::temp_dir().join(format!("vike-layouts-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // empty dir ⇒ no layouts
        assert!(list_layouts_in(&dir).is_empty());

        let wins = vec![chart_win("BTCUSDT", ChartStyle::Candles)];
        let expected = capture(&wins, DisplayTz::Utc, 3.0, true);

        // save two named layouts (via the same schema the public save_layout uses)
        for name in ["Alpha", "beta view"] {
            let p = dir.join(format!("{}.json", sanitize_layout_name(name)));
            save_to(&wins, DisplayTz::Utc, 3.0, true, &[], &p).unwrap();
        }
        // listing is sorted case-insensitively and stem-named
        assert_eq!(list_layouts_in(&dir), vec!["Alpha".to_string(), "beta view".to_string()]);

        // load one back — full-fidelity round-trip through the shared load_from seam
        let loaded = load_from(&dir.join("Alpha.json")).expect("saved above");
        assert_eq!(loaded, expected);
        let rebuilt = apply(&loaded);
        assert_eq!(rebuilt.len(), 1);
        assert_eq!(rebuilt[0].symbol, "BTCUSDT");

        // delete one → gone from the listing, the other survives
        std::fs::remove_file(dir.join("Alpha.json")).unwrap();
        assert_eq!(list_layouts_in(&dir), vec!["beta view".to_string()]);
        // deleting an absent file is not fatal (idempotent contract)
        assert!(matches!(
            std::fs::remove_file(dir.join("Alpha.json")),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound
        ));
        // a missing layout file loads as None (never a panic)
        assert!(load_from(&dir.join("no-such.json")).is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The `layouts/` DIRECTORY holds every named layout, and it is the ONLY directory any of
    /// them is looked for in: listing it lists all of them, and a name resolves to exactly one
    /// file inside it whether or not that file exists yet.
    ///
    /// A layout file sitting in a SIBLING directory is invisible — the property that says the
    /// listing is a plain read of one directory rather than a merge of several. (The behavioural
    /// twin, over the directory a second rung would actually have used, is
    /// [`nothing_in_the_state_family_is_read_from_beside_the_executable`].)
    ///
    /// Env-free by construction — driven through the same `list_layouts_in`/`load_from` seams the
    /// public helpers compose, over temp directories, so no ambient `$VIKE_STATE_ROOT`/
    /// `$VIKE_WORKSPACE` can move the answer.
    #[test]
    fn the_layout_directory_is_the_only_place_a_named_layout_is_looked_for() {
        let root =
            std::env::temp_dir().join(format!("vike-lay-one-{}-{}", std::process::id(), line!()));
        let _ = std::fs::remove_dir_all(&root);
        let dir = root.join("settings/state").join(LAYOUTS_SUBDIR);
        let sibling = root.join("elsewhere").join(LAYOUTS_SUBDIR);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::create_dir_all(&sibling).unwrap();

        let wins = vec![chart_win("BTCUSDT", ChartStyle::Candles)];
        for name in ["Fresh", "Shared"] {
            save_to(&wins, DisplayTz::Utc, 9.0, true, &[], &dir.join(format!("{name}.json")))
                .unwrap();
        }
        // Same stem, different directory, and a different value so a mix-up would be visible.
        for name in ["Elsewhere", "Shared"] {
            save_to(&wins, DisplayTz::Utc, 1.0, false, &[], &sibling.join(format!("{name}.json")))
                .unwrap();
        }

        // The listing is that ONE directory: "Elsewhere" is not in it, "Shared" appears once.
        assert_eq!(
            list_layouts_in(&dir),
            vec!["Fresh".to_string(), "Shared".to_string()],
            "a layout outside the layouts directory is not a layout"
        );
        // …and the file a name resolves to is the one inside it (the 9.0 copy, not the 1.0 one).
        assert_eq!(load_from(&dir.join("Shared.json")).unwrap().of_backfill_hours, 9.0);
        // A name with no file resolves to a path inside the same directory — reported, not read.
        assert!(!dir.join("Nowhere.json").exists());
        assert!(load_from(&dir.join("Nowhere.json")).is_none());

        // Deleting the one copy takes the layout out of the listing for good.
        std::fs::remove_file(dir.join("Shared.json")).unwrap();
        assert_eq!(list_layouts_in(&dir), vec!["Fresh".to_string()]);

        let _ = std::fs::remove_dir_all(&root);
    }

    /// `workspace.json` has ONE path, and the reader and the writer agree on it — a typo on
    /// either side would silently write a file the loader never opens.
    ///
    /// Also pinned: the write CREATES its directory on the way (a first save on a fresh checkout
    /// must not fail for want of `settings/state`), and an absent file resolves to that same path
    /// rather than to a second candidate.
    #[test]
    fn the_workspace_file_has_one_path_and_the_reader_and_the_writer_agree() {
        assert_eq!(WORKSPACE_FILE, "workspace.json");

        // The composition, over a temp directory: no real `$HOME` is touched and no ambient
        // `$VIKE_STATE_ROOT` can move the answer.
        let root =
            std::env::temp_dir().join(format!("vike-ws-state-{}-{}", std::process::id(), line!()));
        let _ = std::fs::remove_dir_all(&root);
        let state = root.join("state");
        assert!(!state.exists(), "precondition: nothing has been written yet");
        let written =
            vike_model::state_path::write_path(Some(state.as_path()), WORKSPACE_FILE).unwrap();
        assert_eq!(written, state.join(WORKSPACE_FILE));
        assert!(state.is_dir(), "the directory is created on the way");
        assert!(!written.exists(), "…and only the directory: the file is the caller's to write");
        let _ = std::fs::remove_dir_all(&root);

        // …and this module's own two entry points land on that same single join.
        let read = path().expect("these tests run from inside the checkout");
        let write = save_path().expect("…whose state directory is creatable");
        assert_eq!(read, write, "a load and a save must never name different files");
        if std::env::var("VIKE_WORKSPACE").is_err() {
            let dir = state_dir().expect("these tests run from inside the checkout");
            assert_eq!(read, dir.join(WORKSPACE_FILE));
        }
    }

    /// **The state family is read from ONE directory, and it is not the executable's.**
    ///
    /// Structural half: `layouts/` and `last_layout.txt` share ONE base with `workspace.json`, so
    /// the family can never half-move, and that base is the project's `settings/state` (or, under
    /// `$VIKE_WORKSPACE`, the directory that override names — one variable moves all of it).
    ///
    /// Behavioural half, and the reason this test writes files: it plants a complete set —
    /// `workspace.json`, `layouts/<name>.json`, `last_layout.txt` — in the test binary's own
    /// directory, and asserts every reader ignores all three. That is the assertion a second
    /// read rung anywhere in this module would fail.
    #[test]
    fn nothing_in_the_state_family_is_read_from_beside_the_executable() {
        let base = base_dir().expect("these tests run from inside the checkout");
        assert_eq!(layouts_dir(), Some(base.join(LAYOUTS_SUBDIR)));
        assert_eq!(last_layout_path(), Some(base.join(LAST_LAYOUT_FILE)));
        assert_eq!(layout_path("Ghost Layout"), Some(base.join(LAYOUTS_SUBDIR).join(GHOST_FILE)));

        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.to_path_buf()))
            .expect("a test binary has a directory");
        assert_ne!(base, exe_dir, "the family's base is the state directory, not the binary's own");

        // Plant the whole family exactly where it must no longer be looked for…
        let ghost_dir = exe_dir.join(LAYOUTS_SUBDIR);
        let ghost_layout = ghost_dir.join(GHOST_FILE);
        let ghost_workspace = exe_dir.join(WORKSPACE_FILE);
        let ghost_pointer = exe_dir.join(LAST_LAYOUT_FILE);
        let wins = vec![chart_win("BTCUSDT", ChartStyle::Candles)];
        std::fs::create_dir_all(&ghost_dir).unwrap();
        save_to(&wins, DisplayTz::Utc, 7.0, false, &[], &ghost_layout).unwrap();
        save_to(&wins, DisplayTz::Utc, 7.0, false, &[], &ghost_workspace).unwrap();
        std::fs::write(&ghost_pointer, "Ghost Layout").unwrap();

        // …ask every reader, then clean up BEFORE asserting so a failure leaves nothing behind.
        let listed = list_layouts();
        let loaded = load_layout("Ghost Layout");
        let pointed = last_layout();
        let workspace = path();
        for f in [&ghost_layout, &ghost_workspace, &ghost_pointer] {
            let _ = std::fs::remove_file(f);
        }
        let _ = std::fs::remove_dir(&ghost_dir);

        assert!(!listed.iter().any(|n| n == "Ghost Layout"), "listed: {listed:?}");
        assert!(loaded.is_none(), "a layout beside the executable is not a layout");
        assert_ne!(pointed.as_deref(), Some("Ghost Layout"), "nor is a pointer at one");
        assert_ne!(workspace, Some(ghost_workspace), "nor is a workspace file");
    }

    /// The layout file the test above plants, named once so its two spellings cannot drift.
    const GHOST_FILE: &str = "Ghost Layout.json";

    // ------------------------------------------------- user indicators, saved by NAME

    /// A stand-in for a compiled user prototype: `close * scale`, one declared knob.
    #[derive(Clone)]
    struct Scaled {
        scale: f64,
        last: f64,
    }

    impl vike_chart::indicators::Indicator for Scaled {
        fn on_bar(&mut self, bar: &vike_model::Bar) -> Vec<f64> {
            self.last = bar.close * self.scale;
            vec![self.last]
        }
        fn vectorize(&self, bars: &[vike_model::Bar]) -> Vec<Vec<f64>> {
            vec![bars.iter().map(|b| b.close * self.scale).collect()]
        }
        fn value(&self) -> Vec<f64> {
            vec![self.last]
        }
        fn reset(&mut self) {
            self.last = f64::NAN;
        }
        fn name(&self) -> &str {
            "scaled"
        }
    }

    /// Named once so the fixture's two spellings (installed name / snapshot name) cannot drift.
    const USER_STUDY: &str = "ws_scaled_close";
    /// A name nothing resolves — the workspace fixture's "the file is gone" case.
    const VANISHED_STUDY: &str = "ws_study_that_no_longer_exists";

    /// The name of THE user study these tests share, installed on first use.
    ///
    /// `vike_chart::indicators::install_user_studies` is a once-per-process `OnceLock` and this
    /// crate's lib tests run as threads in ONE process, so the install sits behind a `Once` — a
    /// second bare call would be the `Err` that function exists to return.
    fn installed_user_study() -> &'static str {
        static INSTALL: std::sync::Once = std::sync::Once::new();
        INSTALL.call_once(|| {
            let meta = vike_chart::indicators::IndicatorMeta::user(
                USER_STUDY,
                "Scaled Close",
                vike_chart::indicators::RenderKind::Overlay,
                vec![vike_chart::indicators::ParamSpec {
                    name: "scale",
                    default: 2.0,
                    min: 0.0,
                    max: 10.0,
                    step: 0.5,
                }],
                &|raw: &[f64]| {
                    Box::new(Scaled { scale: raw.first().copied().unwrap_or(2.0), last: f64::NAN })
                },
            );
            vike_chart::indicators::install_user_studies(vec![meta]).expect("first install");
        });
        USER_STUDY
    }

    /// A USER indicator is added, persisted and restored exactly like a built-in — by NAME,
    /// carrying its params and its per-line paint.
    ///
    /// Non-vacuous: `add_indicator` resolved through `get` before this seam, so on a reverted tree
    /// the second assert fails — the study cannot be added at all and there is nothing to capture.
    /// The `close * 4` assertion on the rebuilt series proves the restore rebuilt the USER's
    /// recurrence through the meta's factory rather than anything from the built-in catalog.
    #[test]
    fn a_user_indicator_round_trips_through_the_workspace_by_name() {
        let name = installed_user_study();
        let r = Rect::from_min_size(pos2(10.0, 10.0), vec2(800.0, 500.0));
        let mut w = WinState::new("usr", "BTCUSDT", "1m", WinKind::Chart, r);
        w.add_indicator("rsi", &[]); // a built-in neighbour, to prove nothing shifts onto it
        w.add_indicator(name, &[]);
        assert_eq!(w.indicators.len(), 2, "the user study must actually be addable");
        assert_eq!(w.indicators[1].spec.name, name);
        w.indicators[1].set_params(vec![4.0], &[]);
        w.indicators[1].outputs[0].color = egui::Color32::from_rgb(9, 9, 9);

        let cap = capture(std::slice::from_ref(&w), DisplayTz::Local, 2.0, false);
        assert_eq!(cap.wins[0].indicators[1].name, name, "persisted by NAME, like every study");
        assert_eq!(cap.wins[0].indicators[1].params, vec![4.0]);

        let json = serde_json::to_string(&cap).unwrap();
        let mut rebuilt = apply(&serde_json::from_str::<Workspace>(&json).unwrap());
        assert_eq!(rebuilt[0].indicators.len(), 2);
        {
            let restored = &rebuilt[0].indicators[1];
            assert_eq!(restored.spec.name, name);
            assert!(restored.spec.is_user());
            assert_eq!(restored.params, vec![4.0], "the user knob survived the round trip");
            assert_eq!(restored.outputs[0].color, egui::Color32::from_rgb(9, 9, 9));
        }

        // …and it computes: fold one real bar and read the user prototype's own formula back.
        let bars =
            [vike_chart::model::Bar { t: 0.0, ot: 0, o: 5.0, h: 5.0, l: 5.0, c: 5.0, v: 1.0 }];
        rebuilt[0].indicators[1].recompute_full(&bars);
        assert_eq!(
            rebuilt[0].indicators[1].outputs[0].series,
            vec![20.0],
            "close 5 * the restored scale 4"
        );
    }

    /// ⚠ THE DEGRADATION CASE. A workspace saved with a user indicator whose file has since been
    /// deleted, renamed or broken must restore the way an unknown BUILT-IN already does: the study
    /// is silently skipped, everything else comes back untouched, and nothing panics.
    ///
    /// Non-vacuous in the way that matters — the claim is not the bare "an unknown name is a
    /// no-op" (already true). The specific hazard is `apply`'s restore loop: it applies each
    /// snapshot entry's params and per-line paint to `indicators.last_mut()`, so a skipped entry
    /// that failed to `continue` would write the VANISHED study's `params: [4.0]` and its colour
    /// onto the PRECEDING study — silent corruption of a study that did restore. The two asserts
    /// on `neighbour` fail if that guard goes; the length assert alone would not notice.
    #[test]
    fn a_vanished_user_indicator_is_skipped_without_disturbing_its_neighbour() {
        let name = installed_user_study();
        assert!(vike_chart::indicators::get_any(VANISHED_STUDY).is_none(), "precondition");

        let r = Rect::from_min_size(pos2(10.0, 10.0), vec2(800.0, 500.0));
        let mut w = WinState::new("gone", "BTCUSDT", "1m", WinKind::Chart, r);
        w.add_indicator("rsi", &[]);
        w.add_indicator(name, &[]);
        w.indicators[0].set_params(vec![9.0], &[]);
        w.indicators[1].set_params(vec![4.0], &[]);
        w.indicators[1].outputs[0].color = egui::Color32::from_rgb(1, 2, 3);

        let cap = capture(std::slice::from_ref(&w), DisplayTz::Local, 2.0, false);
        let json = serde_json::to_string(&cap).unwrap();
        // The file is gone: the SAME snapshot, with the study's name no longer resolving.
        let orphaned = json.replace(name, VANISHED_STUDY);
        assert!(orphaned.contains(VANISHED_STUDY), "the fixture must really name the dead study");

        let rebuilt = apply(&serde_json::from_str::<Workspace>(&orphaned).unwrap());
        assert_eq!(rebuilt[0].indicators.len(), 1, "the vanished study is silently dropped");
        let neighbour = &rebuilt[0].indicators[0];
        assert_eq!(neighbour.spec.name, "rsi");
        assert_eq!(neighbour.params, vec![9.0], "the dead study's params must not land here");
        assert_ne!(
            neighbour.outputs[0].color,
            egui::Color32::from_rgb(1, 2, 3),
            "nor its per-line paint"
        );
    }
}
