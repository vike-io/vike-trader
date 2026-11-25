//! Per-window state (`WindowState`, `WindowKind`) + the window chrome loop
//! (`show_window`).
//!
//! Each window is a floating `egui::Window`. We let egui own the live geometry
//! (so the user can drag/resize), reading the rect back each frame; for an
//! arrange/maximize action geometry is FORCED for a single frame via `pending`,
//! then released (see [`super::arrange`]).
//!
//! `WindowState` deliberately carries only window chrome + chart content state
//! (vike-chart types) — tool-window view state lives in `crate::ui::windows::WindowView`,
//! held by the App keyed by window id, so this module stays free of app content.

use egui::{Id, Pos2, Rect, Vec2};
use vike_orderflow::tickvol::BarKind;
use vike_ui_theme::metrics::space;

mod chrome;
mod kind;

#[cfg(doc)]
use chrome::window_resize_handles;
pub use chrome::{WindowBodyBounds, focused_chart, show_window};
pub use kind::WindowKind;

/// What a chart that names no symbol yet shows on its title bar's symbol button and in its title.
pub(crate) const NO_SYMBOL_LABEL: &str = "Pick a symbol";

/// One Compare overlay of a chart ([`WindowState::compare`]): the series it overlays is
/// `(venue, symbol)` at the chart's own interval. The venue is the overlay's own, so an overlay
/// keeps its feed when the chart's primary moves to another venue.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChartCompareSymbol {
    pub venue: String,
    pub symbol: String,
}

pub struct WindowState {
    pub id: Id,
    pub title: String,
    pub symbol: String,   // e.g. "BTCUSDT" (chart windows)
    pub interval: String, // e.g. "1m" (chart windows)
    /// Data venue the chart's live bar feed subscribes to (`"binance"`, `"bybit"`, `"okx"`).
    /// Cross-exchange symbol search: a search hit carries its venue, and selecting it sets this
    /// alongside `symbol` so `App::ensure_feed_on` routes to the right feed. Every opener of a chart
    /// names it; [`WindowState::new`] starts EMPTY (a chart that names no series yet), never on a
    /// compiled-in venue (the owner's ruling of 2026-11-13, "remove all hardcoded symbols"). The
    /// key spells it for every venue (see [`Self::key`]).
    pub venue: String,
    /// Asset class of the picked instrument (from the Symbol picker's `Instrument.asset_class`),
    /// threaded through so `App::ensure_feed_on` can route non-spot symbols to their venue's
    /// native product feed (e.g. OKX derivatives) instead of the spot bar feed. `None` is the
    /// legacy/spot default — quick-picks, plain-symbol paths, and every pre-feature window leave
    /// this unset, so `ensure_feed_on`'s spot routing is unchanged. NOT serialized as a bare
    /// `WindowState` field (`WindowState` itself is never the serde type — see `super::workspace::WindowSnapshot`,
    /// which carries the persisted twin of this field the same way it already does for `venue`).
    pub asset_class: Option<vike_model::AssetClass>,
    pub kind: WindowKind,
    pub open: bool,            // false => hidden off-desktop (rail can unhide)
    pub minimized: bool,       // true => not drawn as a Window; shows in the left rail
    pub screen_position: Pos2, // our tracked copy of the live geometry
    pub size: Vec2,
    pub pending: Option<Rect>, // one-frame forced geometry (arrange/maximize)
    pub(crate) pre_maximize: Option<Rect>, // rect to restore after un-maximize
    pub maximized: bool,
    pub(crate) force_frames: u8, // N settle frames after open/arrange/move: the edge bands stay off
    /// RECORD: the user has moved one of this window's own edge bands at least once, so its size
    /// is one the user CHOSE. Set by [`show_window`] the first time `window_resize_handles`
    /// moves an edge of a NON-`fills` kind, and never cleared for the life of the window. It
    /// switches no geometry: every window is app-owned from the frame it opens (`current_pos` +
    /// `fixed_size` at `pos`/`size`, the body bounding itself through [`WindowBodyBounds`]), so it does
    /// not grow to its body before or after.
    ///
    /// PERSISTED, as `super::workspace::WindowSnapshot::user_sized`, so a reloaded layout can tell a size
    /// the user chose from one it was given. ⚠ Only `WindowKind::Chart` windows are captured today
    /// (`workspace::capture` filters on it) and a chart is a `fills` kind that never takes it, so
    /// every key written today is `false`; the field carries the plumbing for the day tool windows
    /// join that capture.
    pub user_sized: bool,
    pub hover: Option<[f64; 4]>, // last hovered bar's OHLC (for the header legend)
    pub style: vike_chart::chart::style::ChartStyle, // chart style (candles, line, …)
    pub navigation: Option<vike_chart::ChartNavigation>, // pending nav-button action
    pub indicators: Vec<vike_chart::ChartStudy>,
    pub(crate) next_uid: u64,
    pub picker_open: bool,
    pub(crate) picker_query: String,
    pub(crate) picker_tab: Option<vike_indicators::Category>,
    /// Indicator-target feature (part a): where the NEXT ƒx-picker add should land.
    /// `ChartPaneAddTarget::Auto` (the default) reproduces today's `RenderKind` routing
    /// byte-for-byte; the picker's "Add to" selector writes the other variants.
    /// Runtime picker state only (like `picker_query`/`picker_tab`) — not persisted.
    pub picker_target: vike_chart::chart::pane::ChartPaneAddTarget,
    pub follow: vike_chart::ChartFollowLive, // follow-live-edge state (chart windows)
    /// Committed chart appearance/behavior (chart-UX bundle T6). `options.show_volume`
    /// is the single in-memory source of truth for the volume pane (moved off
    /// the old `WindowState::show_volume`); the "Chart settings" dialog edits a
    /// working copy and commits here on OK. `settings` holds that dialog's
    /// per-window open/working state (cross-frame).
    pub options: vike_chart::ChartSettings,
    pub settings: vike_chart::ChartSettingsDialog,
    /// Per-window "Indicator settings" dialog state (chart-UX bundle T8): which
    /// active indicator's settings window is open (at most one) + its working /
    /// snapshot edit copies. Opened from an oscillator pane-header ⚙ (inside
    /// `chart::draw`) or a ƒx-picker per-row ⚙ (main.rs); the resulting live
    /// edits are applied to `indicators` via `chart::ChartActions::indicator_edit`.
    pub indicator_dialog: vike_chart::ChartStudyDialog,
    /// Requested price-scale mode (chart-UX bundle T3): the persisted twin of
    /// `chart::ChartInputs::scale` — main.rs reads it in, then applies
    /// `chart::ChartActions::scale_change` (toggle click / Alt+L) back onto
    /// it after each frame.
    pub scale: vike_chart::ChartScaleMode,
    /// TradingView "Invert scale" flag: the persisted twin of
    /// `chart::ChartInputs::invert` — an orthogonal modifier (flips the y-axis
    /// vertically) usable atop any `scale`. main.rs reads it in and applies
    /// `chart::ChartActions::invert_change` (the price-axis right-click toggle)
    /// back onto it after each frame.
    pub invert: bool,
    /// Per-pane sub-pane height fractions (chart-UX bundle T9): the persisted
    /// twin of `chart::ChartInputs::panes` — identity-keyed (by `ChartPaneKey`, an
    /// oscillator's stable `ChartStudy::uid`) so a hidden/re-shown pane or a
    /// reordered indicator list never scrambles which stored share belongs to
    /// which pane. Mutated in place by `chart::draw` (layout reads + resolves
    /// new-pane defaults; separator drags write new shares); main.rs takes it
    /// out via `mem::take` before `show_window` (same dance as `follow`/
    /// `settings`/`indicator_dialog` — `w` itself is borrowed by that call) and
    /// writes it back after.
    pub panes: vike_chart::ChartPaneFractions,
    /// Cross-window chart sync group membership (chart sync seam, task B8): `1..=4`, or
    /// `None` when ungrouped. Windows sharing a group broadcast/receive a ghost crosshair
    /// and a sticky-leader visible range through `App::{sync_previous,sync_next,range_leader}`
    /// (main.rs) — this field is only the persisted membership tag; the title-bar chip
    /// (main.rs `title_bar`) reads/cycles it. `None` is the zero-visual-change default.
    pub sync_group: Option<u8>,
    /// SP2 orderflow (Task 7): show the CVD sub-pane this frame. Default `false` — zero
    /// behavior change until the user opts in via the title-bar Orderflow popup (`main.rs
    /// title_bar`). Harvested back to `false` when the CVD pane's own ✕ is clicked
    /// (`chart::ChartActions::cvd_toggle`, see the window loop in `main.rs`).
    pub cumulative_volume_delta_on: bool,
    /// SP2 orderflow (Task 7): show the volume-profile overlay this frame. Default `false`.
    pub profile_on: bool,
    /// SP2 orderflow (Task 7): volume-profile / footprint bucket width override in raw price
    /// units. `None` (default) defers to the aggregator's own default (see
    /// `vike_orderflow::bar_agg::OrderflowAgg::new`); `Some(x)` pins it. Set from the title-bar Orderflow
    /// popup's "Tick size" field.
    pub orderflow_tick_size: Option<f64>,
    /// C1 Task 2: authored order of STUDY (oscillator) panes, top→bottom, below the
    /// price/volume/CVD panes. Populated by `WindowState::assign_default_pane` (on
    /// `WindowState::add_indicator`) and [`WindowState::move_study`]; pruned of empty
    /// panes by `drop_empty_study_panes`. NOT YET read by rendering — Task 3 wires
    /// the read side (`WindowState::present_study_panes` is the intended read seam),
    /// so populating this field this task is a pure-model change with zero runtime
    /// effect.
    pub pane_order: Vec<vike_chart::chart::pane::ChartPaneKey>,
    /// C1 Task 2: which pane a given oscillator study (`ChartStudy::uid`) lives in.
    /// Overlay studies are absent (they always render in the price pane and never
    /// enter this map). `IndexMap` for the repo-wide insertion-order convention
    /// (not load-bearing for f64 sums here, just consistency — see root
    /// `Cargo.toml`'s indexmap rationale comment).
    pub study_pane: indexmap::IndexMap<u64, vike_chart::chart::pane::ChartPaneKey>,
    /// C1 Task 2: allocator for fresh `ChartPaneKey::Study(id)` panes — a counter
    /// INDEPENDENT of `next_uid` (pane identity, not indicator identity: merging
    /// two studies into one pane, or moving a study to a new pane, must not
    /// consume/collide with indicator uids). Starts at 1.
    pub(crate) next_pane_id: u64,
    /// C2a Task 4: series overlaid in the price pane as %-normalized "Compare"
    /// series (same `interval` as this window), rendered on top of the primary.
    /// Add-order IS color-order (`main.rs`'s `COMPARE_COLORS` gather indexes by
    /// position); deduped by symbol and order-preserving; never contains the window's
    /// OWN `symbol`. Each entry carries its OWN venue (a new one takes the chart's, see
    /// [`WindowState::add_compare`]), and its `ChartState` is looked up (never owned) in
    /// `App.charts` by `chart_key::series_key(venue, symbol, interval)` and ensure-synced
    /// like the primary. EMPTY is the zero-visual-change default — an empty overlay list
    /// feeds `chart::draw` the same `&[]` the pre-Task-4 single-series render did.
    pub compare: Vec<ChartCompareSymbol>,
    /// C2b Task 5: authored top→bottom order of panes each holding a compare
    /// symbol moved OUT of the price-pane overlay (mirrors `pane_order`, but
    /// for per-symbol "own pane" placement rather than per-study). Populated
    /// by [`WindowState::move_series_to_new_pane`] (a `move_series` reorder verb stood beside it with
    /// no caller and was deleted); pruned of empty panes by `drop_empty_series_panes`. Read by rendering
    /// via [`WindowState::present_series_panes`] (its non-empty panes become the
    /// compare-series sub-panes below price, threaded into `chart::draw` as
    /// `ChartInputs::series_panes` in main.rs).
    pub(crate) series_pane_order: Vec<vike_chart::chart::pane::ChartPaneKey>,
    /// C2b Task 5: which pane a given compare symbol (an entry of `compare`)
    /// lives in. ABSENT means the symbol is overlaid in the price pane as a
    /// %-normalized line — `compare`'s only behavior pre-C2b, and every
    /// compare symbol's default here too. Unlike C1's `study_pane`, there is
    /// NO assign-on-add: `add_compare` never touches this map; only
    /// `move_series_to_new_pane` inserts an entry. `IndexMap` for the
    /// repo-wide insertion-order convention (see root `Cargo.toml`'s indexmap
    /// rationale comment), keyed by the symbol string since compare symbols
    /// have no stable integer uid the way indicators do.
    pub series_pane: indexmap::IndexMap<String, vike_chart::chart::pane::ChartPaneKey>,
    /// C2b Task 7 ("Pin to scale ▸ Right"): per-compare-symbol secondary-axis
    /// assignment. ABSENT ⇒ [`vike_chart::chart::scale::ChartScaleAssignment::Percent`] (the default):
    /// the symbol is a %-line on the shared % axis (C2a). `Right` pins it to its
    /// OWN absolute price axis on the right — its real price range remapped into
    /// the price pane's plot-space, filling the pane on its own scale. Set by the
    /// Task-9 "Pin to scale" menu; read via `WindowState::series_scale_of` and
    /// threaded into `chart::draw` as `ChartInputs::series_scale`. `IndexMap` for
    /// the repo-wide insertion-order convention, keyed by the compare symbol
    /// string (like `series_pane`). EMPTY by default ⇒ every overlay stays a
    /// %-line ⇒ byte-identical to the C2a render.
    pub series_scale: indexmap::IndexMap<String, vike_chart::chart::scale::ChartScaleAssignment>,
    /// C2b Task 5: allocator for fresh `ChartPaneKey::Series(id)` panes — a
    /// counter INDEPENDENT of `next_uid`/`next_pane_id` (its own namespace: a
    /// `ChartPaneKey::Series(3)` and a `ChartPaneKey::Study(3)` are unrelated pane keys,
    /// so the counters never need to agree). Starts at 1.
    pub(crate) next_series_pane_id: u64,
    /// **THE OPERATOR CHOSE THIS WINDOW'S SERIES**, so
    /// [`series_follow::follow_backend`](crate::ui::series_follow::follow_backend) may not retarget it.
    ///
    /// `false` from [`WindowState::new`] — the startup default chart
    /// ([`startup::plan`](crate::ui::startup::plan)'s) and a `New chart window` with no default open
    /// EMPTY, naming no series anybody asked for. `true` from [`super::workspace::apply`] (a saved
    /// layout IS a choice), from the symbol picker's apply in `crates/vike-desktop/src/app_ui/mod.rs`'s
    /// `draw_windows`, and from `crate::ui::window::spawn::chart_on` for every window OPENED on a
    /// series, its history loaded or not.
    ///
    /// ⚠ **Deliberately NOT persisted** — there is no `super::workspace::WindowSnapshot` key for it, and
    /// none is wanted: restore sets it unconditionally, so every window a workspace file can
    /// produce is pinned whatever version wrote the file. Adding a key would create a second
    /// spelling that could disagree with that rule, for a value the rule already determines.
    ///
    /// ⚠ **It no longer covers the INTERVAL** — see [`Self::interval_pinned`], which the interval
    /// menu sets instead.
    pub series_pinned: bool,
    /// **THE OPERATOR CHOSE THIS WINDOW'S RESOLUTION** — the interval menu was used on it, so
    /// [`series_follow::follow_backend`](crate::ui::series_follow::follow_backend) may retarget its
    /// venue and symbol but must COPY this window's own interval rather than the candidate's.
    ///
    /// # ⚠ Why this is a SECOND flag rather than a wider reading of [`Self::series_pinned`]
    ///
    /// The interval menu used to set `series_pinned`, which disabled adoption for that window
    /// PERMANENTLY — nothing in the tree ever clears either flag, so one click on `5m` cost the
    /// window every later chance to follow the node. And the naive repair, dropping the write
    /// altogether, is WORSE and was measured as such: un-pinned, the new interval's `ChartState`
    /// is empty, so `has_bars` is false, `plan_follow` adopts, and the chart silently snaps back
    /// to whatever interval the daemon publishes — a deliberate act reverted with no explanation.
    ///
    /// The two acts are genuinely different choices about different things. Picking a SYMBOL says
    /// "show me this instrument", which is a statement about which series; picking an INTERVAL
    /// says "show me this resolution", which is a statement about how, and is entirely compatible
    /// with the node moving the window onto the venue it actually mounts.
    ///
    /// `false` from [`WindowState::new`]; `true` from the interval picker's apply in
    /// `crates/vike-desktop/src/app_ui/mod.rs`'s `draw_windows` and from [`super::workspace::apply`] (a
    /// saved layout chose both halves). NOT persisted, for the same reason its sibling is not.
    pub interval_pinned: bool,
}

/// Settle frames right after a window is created or restored from a workspace (`force_frames`,
/// exactly like the arrange/move path): the edge bands stay off while the first frames lay the
/// window out, because `chart::draw`'s `avail` clamp under-reports height until the clip-rect
/// settles. The window itself is pinned at its `size` every frame (see `show_window`).
const OPEN_PIN_FRAMES: u8 = 8;

// `workspace::MIN_WIDTH` × `workspace::MIN_HEIGHT` (rows of `ui-theme.toml`) are the FLOOR a band drag may
// drive a window's size to, on each axis. They are used at module scope rather than inside
// [`show_window`]'s resize block — which is where both constants lived, and where every comment in
// this file still cites them from — because the ARENA CEILING at the top of that same function has
// to spell the same pair: a clamp that can drive a window BELOW the floor its own drags are floored
// at would fight the band arithmetic every frame, on a box whose arena is smaller than one of these.

impl WindowState {
    pub fn new(
        id_source: &str,
        symbol: &str,
        interval: &str,
        kind: WindowKind,
        rect: Rect,
    ) -> Self {
        Self {
            id: Id::unique(id_source),
            title: title_for("", symbol, interval),
            symbol: symbol.to_string(),
            interval: interval.to_string(),
            venue: String::new(),
            asset_class: None,
            kind,
            open: true,
            minimized: false,
            screen_position: rect.min,
            size: rect.size(),
            pending: None,
            pre_maximize: None,
            maximized: false,
            force_frames: OPEN_PIN_FRAMES, // pin the intended size on open so egui can't auto-shrink it
            user_sized: false,             // set on the first edge-band drag; see the field doc
            hover: None,
            style: vike_chart::chart::style::ChartStyle::Candles,
            navigation: None,
            indicators: Vec::new(),
            next_uid: 0,
            picker_open: false,
            picker_query: String::new(),
            picker_tab: None,
            picker_target: vike_chart::chart::pane::ChartPaneAddTarget::Auto,
            follow: vike_chart::ChartFollowLive::default(),
            options: vike_chart::ChartSettings::default(),
            settings: vike_chart::ChartSettingsDialog::default(),
            indicator_dialog: vike_chart::ChartStudyDialog::default(),
            scale: vike_chart::ChartScaleMode::Linear,
            invert: false,
            panes: vike_chart::ChartPaneFractions::default(),
            sync_group: None,
            cumulative_volume_delta_on: false,
            profile_on: false,
            orderflow_tick_size: None,
            pane_order: Vec::new(),
            study_pane: indexmap::IndexMap::new(),
            next_pane_id: 1,
            compare: Vec::new(),
            series_pane_order: Vec::new(),
            series_pane: indexmap::IndexMap::new(),
            series_scale: indexmap::IndexMap::new(),
            next_series_pane_id: 1,
            // Nobody chose this series yet — see the field doc. `workspace::apply` and the symbol
            // picker's apply are the two sites that set it.
            series_pinned: false,
            // …and nobody chose this RESOLUTION yet. The interval picker's apply and
            // `workspace::apply` are this one's two sites.
            interval_pinned: false,
        }
    }

    /// Add study `name` to this window, or do NOTHING if the name resolves to no indicator.
    ///
    /// ⚠ `get_any`, not `get`: the lookup is the union of the built-in catalog and the USER
    /// studies a binary installed at startup, so a `user_data/indicators/<name>.rhai` is added
    /// exactly like a built-in. The silent no-op for an unresolvable name is LOAD-BEARING and
    /// unchanged — a workspace persists studies by NAME, so this is also the path a restore takes
    /// for a user indicator whose file has since been deleted, renamed or broken.
    pub(crate) fn add_indicator(
        &mut self,
        name: &str,
        bars: &[vike_chart::chart::state::bar::ChartBar],
    ) {
        if let Some(spec) = vike_chart::chart::study::get_any(name) {
            let uid = self.next_uid;
            self.next_uid += 1;
            let active = vike_chart::ChartStudy::new(uid, spec, bars);
            // C1 Task 2: an oscillator study defaults to its own fresh pane (today's
            // one-osc-per-pane layout); overlays live on the price pane and never
            // enter `study_pane`/`pane_order`.
            if !active.is_overlay() {
                self.assign_default_pane(uid);
            }
            self.indicators.push(active);
        }
    }

    /// Indicator-target feature (part a): add indicator `name`, THEN place it at the
    /// picker-chosen `target` by reusing the EXISTING [`Self::move_study`] machinery.
    /// [`vike_chart::chart::pane::resolve_add_target`] maps `(RenderKind, target)` to an
    /// optional relocation — `None` (its answer for `ChartPaneAddTarget::Auto`, every overlay,
    /// and an oscillator's default new/price pane) leaves `add_indicator`'s placement
    /// untouched, so `add_indicator_to(name, bars, Auto)` is BYTE-IDENTICAL to
    /// `add_indicator(name, bars)`. Only an oscillator + `Existing(pane)` relocates
    /// (a merge into that study pane). A no-op for an unknown indicator name.
    pub fn add_indicator_to(
        &mut self,
        name: &str,
        bars: &[vike_chart::chart::state::bar::ChartBar],
        target: vike_chart::chart::pane::ChartPaneAddTarget,
    ) {
        let uid = self.next_uid; // the uid `add_indicator` will assign IF `name` is known
        self.add_indicator(name, bars);
        if self.next_uid == uid {
            return; // unknown name → nothing added (add_indicator left next_uid untouched)
        }
        if let Some(spec) = vike_chart::chart::study::get_any(name)
            && let Some(mt) = vike_chart::chart::pane::resolve_add_target(spec.kind, target)
        {
            self.move_study(uid, mt);
        }
    }

    pub fn remove_indicator(&mut self, uid: u64) {
        self.indicators.retain(|a| a.uid != uid);
        self.study_pane.shift_remove(&uid);
        self.drop_empty_study_panes();
    }

    /// C1 Task 2: allocate a fresh `Study(id)` pane for `uid`, appended to the end
    /// of `pane_order` — called from `add_indicator` for oscillator-kind studies
    /// only. `next_pane_id` is a pane-identity counter independent of `next_uid`
    /// (see the field doc).
    fn assign_default_pane(&mut self, uid: u64) {
        let pane = vike_chart::chart::pane::ChartPaneKey::Study(self.next_pane_id);
        self.next_pane_id += 1;
        self.pane_order.push(pane);
        self.study_pane.insert(uid, pane);
    }

    /// C1 Task 2: move oscillator study `uid` per `target`. `Into(pane)` re-points
    /// `study_pane[uid]` at an existing pane (merging it in); `NewAbove`/`NewBelow`
    /// allocate a fresh pane and insert it into `pane_order` immediately adjacent to
    /// `anchor`. Either way, the study's PREVIOUS pane may end up with no members —
    /// `drop_empty_study_panes` prunes it from `pane_order` afterward. A no-op if
    /// `uid` is not a currently-tracked study (unknown uid, or an overlay).
    pub fn move_study(&mut self, uid: u64, target: vike_chart::chart::pane::ChartPaneMoveTarget) {
        use vike_chart::chart::pane::ChartPaneMoveTarget;
        if !self.study_pane.contains_key(&uid) {
            return;
        }
        match target {
            ChartPaneMoveTarget::Into(pane) => {
                // Only move into a pane that is actually live in `pane_order`. Without
                // this guard a stale `ChartPaneKey` (e.g. a pane dropped earlier, or a key
                // from another window — `Study` ids are per-WindowState, not global) would
                // point `study_pane[uid]` at a pane that `present_study_panes()` never
                // yields, silently rendering the study in zero panes once Task 3 wires
                // the read side. Mirrors the `NewAbove/NewBelow` anchor-not-present guard.
                if self.pane_order.contains(&pane) {
                    self.study_pane.insert(uid, pane);
                }
            }
            ChartPaneMoveTarget::NewAbove(anchor) | ChartPaneMoveTarget::NewBelow(anchor) => {
                let pane = vike_chart::chart::pane::ChartPaneKey::Study(self.next_pane_id);
                self.next_pane_id += 1;
                let at = match self.pane_order.iter().position(|&p| p == anchor) {
                    Some(i) if matches!(target, ChartPaneMoveTarget::NewAbove(_)) => i,
                    Some(i) => i + 1,              // NewBelow
                    None => self.pane_order.len(), // anchor not present (defensive) — append
                };
                self.pane_order.insert(at, pane);
                self.study_pane.insert(uid, pane);
            }
        }
        self.drop_empty_study_panes();
    }

    /// C1 Task 2: remove any pane from `pane_order` with no remaining `study_pane`
    /// member. Called after every mutation that can empty a pane (`remove_indicator`,
    /// `move_study`).
    fn drop_empty_study_panes(&mut self) {
        self.pane_order.retain(|p| self.study_pane.values().any(|v| v == p));
    }

    /// C1 Task 2: `pane_order` filtered to STUDY panes that still have at least
    /// one `study_pane` member — the read seam persistence uses (`panes_to_snapshot`/
    /// `pane_membership_snapshot`). Volume/CVD entries in `pane_order` (chart
    /// single-max default) are skipped: they are never in `study_pane.values()`.
    /// Defensive filter on top of `drop_empty_study_panes` already being called
    /// after every mutation, so this is never stale.
    pub(crate) fn present_study_panes(&self) -> Vec<vike_chart::chart::pane::ChartPaneKey> {
        self.pane_order
            .iter()
            .copied()
            .filter(|p| self.study_pane.values().any(|v| v == p))
            .collect()
    }

    /// Chart single-max default: reconcile the authored `pane_order` membership
    /// with the live Volume/CVD toggles. Volume and CVD are now normal,
    /// reorderable sub-panes stored INLINE in `pane_order` alongside study panes
    /// — so when a toggle turns ON they must enter `pane_order` (at their default
    /// slot: Volume at the FRONT, CVD directly after Volume, matching the
    /// pre-reorder Volume→CVD→studies sequence), and when it turns OFF they leave
    /// (their authored position is not retained — re-enabling re-slots at the
    /// default, same "front" contract the brief specifies). Study membership is
    /// managed separately (`assign_default_pane`/`drop_empty_study_panes`). Call
    /// this once per frame BEFORE reading [`Self::present_sub_panes`] so a freshly
    /// toggled pane is placed exactly like a study pane. Idempotent.
    pub fn sync_sub_panes(&mut self) {
        use vike_chart::chart::pane::ChartPaneKey;
        let show_volume = self.options.show_volume;
        let cumulative_volume_delta_on = self.cumulative_volume_delta_on;
        let has_volume = self.pane_order.contains(&ChartPaneKey::Volume);
        if show_volume && !has_volume {
            self.pane_order.insert(0, ChartPaneKey::Volume);
        } else if !show_volume && has_volume {
            self.pane_order.retain(|p| *p != ChartPaneKey::Volume);
        }
        let has_cvd = self.pane_order.contains(&ChartPaneKey::CumulativeVolumeDelta);
        if cumulative_volume_delta_on && !has_cvd {
            let at = self
                .pane_order
                .iter()
                .position(|p| *p == ChartPaneKey::Volume)
                .map(|i| i + 1)
                .unwrap_or(0);
            self.pane_order.insert(at, ChartPaneKey::CumulativeVolumeDelta);
        } else if !cumulative_volume_delta_on && has_cvd {
            self.pane_order.retain(|p| *p != ChartPaneKey::CumulativeVolumeDelta);
        }
    }

    /// Chart single-max default: the authored UNIFIED sub-pane order — Volume,
    /// CVD, and study panes as PEERS, in `pane_order` order, filtered to those
    /// logically present (Volume iff `show_volume`, CVD iff `cumulative_volume_delta_on`, a study
    /// pane iff it still holds a member). The read seam threaded into
    /// `chart::draw` as `ChartInputs::sub_panes`; `resolve_pane_layout` applies
    /// the finer style/footprint/visibility gating on top. Call
    /// [`sync_sub_panes`](Self::sync_sub_panes) first so Volume/CVD membership is
    /// up to date.
    pub fn present_sub_panes(&self) -> Vec<vike_chart::chart::pane::ChartPaneKey> {
        use vike_chart::chart::pane::ChartPaneKey;
        self.pane_order
            .iter()
            .copied()
            .filter(|p| match p {
                ChartPaneKey::Volume => self.options.show_volume,
                ChartPaneKey::CumulativeVolumeDelta => self.cumulative_volume_delta_on,
                ChartPaneKey::Study(_) => self.study_pane.values().any(|v| v == p),
                ChartPaneKey::Price | ChartPaneKey::Series(_) => false,
            })
            .collect()
    }

    /// Feature #1 (TradingView parity) — chart single-max default: move a sub-pane
    /// (a study pane, Volume, or CVD — all peers now) one slot up (`up = true`) or
    /// down within the PRESENT sub-pane group, driven by the pane header's ↑/↓
    /// controls. Adjacency is resolved over [`Self::present_sub_panes`] (so a
    /// pruned/empty/absent entry never traps the move), then the two panes are
    /// swapped in the authored `pane_order` — the render order reflects it next
    /// frame. A no-op at the group's ends or for an unknown/absent `pane`.
    pub fn reorder_pane(&mut self, pane: vike_chart::chart::pane::ChartPaneKey, up: bool) {
        let present = self.present_sub_panes();
        let Some(index) = present.iter().position(|&p| p == pane) else { return };
        let swap_with = if up {
            if index == 0 {
                return;
            }
            present[index - 1]
        } else {
            if index + 1 >= present.len() {
                return;
            }
            present[index + 1]
        };
        if let (Some(i), Some(j)) = (
            self.pane_order.iter().position(|&p| p == pane),
            self.pane_order.iter().position(|&p| p == swap_with),
        ) {
            self.pane_order.swap(i, j);
        }
    }

    /// C2a Task 4: overlay `symbol` as a %-normalized Compare series in the price
    /// pane. Order-preserving (add-order == color-order in `main.rs`'s
    /// `COMPARE_COLORS` gather), deduped (no-op if already overlaid), and it
    /// NEVER adds the window's own `symbol` (case-insensitive) — a chart doesn't
    /// overlay itself. The overlay is on the chart's OWN venue (`self.venue`): the Compare
    /// search and quick-picks offer that venue's symbols, and the entry keeps the venue after.
    ///
    /// On the FIRST overlay (list was empty → now non-empty) it forces
    /// `self.scale` to [`ChartScaleMode::Percent`](vike_chart::ChartScaleMode::Percent): the
    /// vike-chart overlay render (Task 3) only draws overlays in Percent mode (a
    /// raw %-line can't share a Linear/Log price axis), so without this flip the
    /// freshly-added overlay would be invisible. An IGNORED add (own symbol) does
    /// NOT count as the first overlay, so it never flips the scale. Restoring the
    /// pre-overlay scale on removal is intentionally out of scope — the user
    /// toggles the scale back manually.
    pub fn add_compare(&mut self, symbol: &str) {
        if symbol.eq_ignore_ascii_case(&self.symbol) {
            return; // never overlay the window's own series
        }
        if self.compare.iter().any(|c| c.symbol == symbol) {
            return; // already overlaid
        }
        let was_empty = self.compare.is_empty();
        let venue = self.venue.clone();
        self.compare.push(ChartCompareSymbol { venue, symbol: symbol.to_string() });
        if was_empty && self.scale != vike_chart::ChartScaleMode::Percent {
            // FIRST overlay: auto-switch so it's visible (Task 3 gates on Percent).
            self.scale = vike_chart::ChartScaleMode::Percent;
        }
    }

    /// C2a Task 4: remove `symbol` from the Compare overlays (exact match; no-op if
    /// absent). Does NOT restore the pre-`add_compare` scale (see that method).
    /// Also cascades into the C2b series-pane model — mirrors how `remove_indicator`
    /// cleans up `study_pane` — so removing a symbol that had its own pane doesn't
    /// leave an orphaned `series_pane` entry / empty pane behind. C2b Task 7 review
    /// Minor-1: the secondary-axis pin (`series_scale`) is dropped too, so re-adding
    /// the same symbol later defaults back to `Percent` rather than resurrecting a
    /// stale `Right` pin.
    pub fn remove_compare(&mut self, symbol: &str) {
        self.compare.retain(|c| c.symbol != symbol);
        self.series_pane.shift_remove(symbol);
        self.series_scale.shift_remove(symbol);
        self.drop_empty_series_panes();
    }

    /// C2b Task 5: move `symbol` (a compare series, see `compare`) from the
    /// price-pane %-overlay into its OWN pane — the "Move to own pane"
    /// action. Allocates a fresh `ChartPaneKey::Series(next_series_pane_id)`,
    /// appended to `series_pane_order`. Unlike C1's `assign_default_pane`,
    /// there is NO assign-on-add: `add_compare` never touches `series_pane`,
    /// so this is the ONLY way a symbol enters it. No-op if `symbol` is
    /// already in its own pane.
    pub fn move_series_to_new_pane(&mut self, symbol: &str) {
        if self.series_pane.contains_key(symbol) {
            return; // already in its own pane
        }
        let pane = vike_chart::chart::pane::ChartPaneKey::Series(self.next_series_pane_id);
        self.next_series_pane_id += 1;
        self.series_pane_order.push(pane);
        self.series_pane.insert(symbol.to_string(), pane);
    }

    /// C2b Task 5: return `symbol` from its own pane back to the price-pane
    /// %-overlay (removes it from `series_pane`; a no-op if it's already
    /// overlaid or unknown). `drop_empty_series_panes` then prunes the
    /// vacated pane from `series_pane_order` if `symbol` was its last member —
    /// mirrors `remove_indicator`'s pane-side cleanup.
    pub fn overlay_series(&mut self, symbol: &str) {
        self.series_pane.shift_remove(symbol);
        self.drop_empty_series_panes();
    }

    /// C2b Task 5: remove any pane from `series_pane_order` with no
    /// remaining `series_pane` member. Mirrors `drop_empty_study_panes` —
    /// called after every mutation that can empty a pane (`overlay_series`).
    fn drop_empty_series_panes(&mut self) {
        self.series_pane_order.retain(|p| self.series_pane.values().any(|v| v == p));
    }

    /// C2b Task 5: `series_pane_order` filtered to panes that still have at
    /// least one `series_pane` member — the read seam the render/present build
    /// uses (threaded into `chart::draw` as `ChartInputs::series_panes` in
    /// main.rs). Mirrors `present_study_panes`.
    pub fn present_series_panes(&self) -> Vec<vike_chart::chart::pane::ChartPaneKey> {
        self.series_pane_order
            .iter()
            .copied()
            .filter(|p| self.series_pane.values().any(|v| v == p))
            .collect()
    }

    /// C2b Task 7 ("Pin to scale"): the [`vike_chart::chart::scale::ChartScaleAssignment`] for a
    /// compare symbol — its stored pin, or the `Percent` default when unpinned
    /// (the C2a shared-% behavior). The read seam the render path uses to route
    /// each overlay to the shared % axis vs its own secondary (Right) price axis.
    #[cfg(test)]
    pub fn series_scale_of(&self, symbol: &str) -> vike_chart::chart::scale::ChartScaleAssignment {
        self.series_scale.get(symbol).copied().unwrap_or_default()
    }

    /// A non-chart tool window (Options/News/Calendar/Data).
    pub fn tool(id_source: &str, kind: WindowKind, rect: Rect) -> Self {
        let mut s = Self::new(id_source, "", "", kind, rect);
        s.title = kind.label().to_string();
        s
    }

    /// Feed/chart key: `"venue:SYMBOL@interval"` for every venue, so the same symbol on two
    /// venues (e.g. Binance vs Bybit `BTCUSDT`) never collides in the `charts`/`subscriptions`/`spawned`
    /// maps. See [`crate::data::chart_key::series_key`].
    pub fn key(&self) -> String {
        crate::data::chart_key::series_key(&self.venue, &self.symbol, &self.interval)
    }

    /// Recompute the title from venue+symbol+interval (after a dropdown/search change).
    pub fn retitle(&mut self) {
        self.title = title_for(&self.venue, &self.symbol, &self.interval);
    }

    /// SP2 orderflow (Task 7): does this window need the live trade feed + per-bar
    /// footprint aggregation? True when either toggle is on, OR the chart style is
    /// `Footprint` on a Kline interval (tick/volume charts build bars from the trade tape
    /// already but are otherwise out of scope for the Footprint STYLE — see
    /// `global-constraints.md`'s crypto-only/Binance-kline-only note). The caller
    /// (`main.rs`'s window loop) uses this to lazily subscribe the trade feed and register
    /// an `vike_orderflow::bar_agg::OrderflowAgg` — never on a fresh, all-default window.
    pub fn orderflow_on(&self) -> bool {
        self.cumulative_volume_delta_on
            || self.profile_on
            || (matches!(BarKind::parse(&self.interval), BarKind::Kline(_))
                && self.style == vike_chart::chart::style::ChartStyle::Footprint)
    }
}

fn title_for(venue: &str, symbol: &str, interval: &str) -> String {
    // Every venue, Binance included, gets an upper-cased venue prefix so a chart is unambiguous
    // in the title bar and taskbar. A chart with no symbol yet says so rather than printing a
    // blank; one with no venue yet (only `WindowState::new`, before its opener names one) has none.
    if symbol.is_empty() {
        format!("{NO_SYMBOL_LABEL} · {interval}")
    } else if venue.is_empty() {
        format!("{symbol} · {interval}")
    } else {
        format!("{} {symbol} · {interval}", venue.to_uppercase())
    }
}

/// The chart title bar's symbol button: `VENUE:SYMBOL` for every venue, Binance included,
/// `SYMBOL` for a chart with no venue yet, and `NO_SYMBOL_LABEL` while the chart names no symbol, so the button
/// that opens the picker is never blank.
#[must_use]
pub fn symbol_label(venue: &str, symbol: &str) -> String {
    if symbol.is_empty() {
        NO_SYMBOL_LABEL.to_string()
    } else if venue.is_empty() {
        symbol.to_string()
    } else {
        format!("{}:{symbol}", venue.to_uppercase())
    }
}

/// The inset a TOOL window's body is drawn at, below its flush custom title bar. Lives here, not
/// at the vike-desktop call site that applies it, so the headless harness in this file models the
/// SHIPPED inset rather than a second copy of it — the margin is geometry the resize bands depend
/// on, and a harness with no margin is the one configuration
/// `window_resize_handles` says must never ship.
///
/// ⚠ The SIDE inset (8) equals `window_resize_handles`' `BAND`, and that is load-bearing rather
/// than cosmetic: it puts a floating vertical scrollbar's 10pt interact strip
/// (`egui-0.37.0/src/style.rs`, `ScrollStyle::floating()` — `bar_width: 10.0`,
/// `floating_allocated_width: 0.0`, so the strip is drawn over content and reserves no space)
/// entirely INBOARD of the band, adjacent and disjoint.
///
/// ⚠ The BOTTOM inset is 6, NOT 8, so a floating HORIZONTAL scrollbar's strip and the 8pt bottom
/// band overlap by exactly 2pt. That is ACCEPTED, and the direction matters: the bands are
/// allocated LAST, and `egui-0.37.0/src/hit_test.rs`'s `find_closest_within` breaks a distance tie
/// by taking "the last one = the one on top", so the BAND wins those 2 points and the horizontal
/// bar's grab area is 2pt thinner. The resize edge is never captured — which is the opposite of
/// what an earlier version of this note claimed. Gated by
/// `the_side_bands_clear_the_scrollbars_and_the_bottom_band_overlaps_by_the_measured_2pt`.
pub const TOOL_BODY_MARGIN: egui::Margin = egui::Margin {
    left: space::LG as i8,
    right: space::LG as i8,
    top: space::NONE as i8,
    bottom: space::MD as i8,
};

#[path = "orderflow_on_tests.rs"]
#[cfg(test)]
mod orderflow_on_tests;

/// The edge-band resize path (`window_resize_handles` -> `show_window`), the `user_sized` latch
/// and the body wrapper [`WindowBodyBounds`], driven headlessly through a real `egui::Context`.
///
/// ⚠ No GPU and no `egui_kittest` here. A `#[cfg(test)]` module inside `src/` can only reach the
/// crate's OWN dependencies, and it does not need more than that: `Context::begin_pass` /
/// `end_pass` is pure CPU layout, and every assertion below is "where did this rect land", which
/// is arithmetic rather than rendering.
///
/// ⚠ Every scenario runs several frames on ONE persistent `Context`, and the step-per-frame
/// scripting in [`drag_band`] is load-bearing rather than tidiness: egui resolves interaction from
/// the PREVIOUS frame's widget geometry (`ContextImpl::begin_pass` hit-tests `prev_pass.widgets`),
/// so a band cannot be pressed on the frame it was first allocated.
///
/// ⚠ The harness calls the PRODUCTION wrapper ([`WindowBodyBounds::show`]) and the PRODUCTION inset
/// ([`TOOL_BODY_MARGIN`]). It modelled both itself until 2026-09-13, which meant the scroll half
/// of this fix was gated by mutations of the HARNESS rather than of shipped code — `vike-desktop`
/// is in `xtask::ci::tables::roster`'s `EXCLUDE_FROM_CI`, so deleting `auto_shrink(false)` from the call
/// site left every test here green.
///
/// ⚠ It models BOTH call sites, and the split between them is not by kind — see [`Site`]. Only
/// Chart uses the wrapper-less chart site; the other two `fills` kinds are dispatched from the
/// TOOL site, so a harness with one `fills` fixture modelled the wrong site for two of the three.
///
/// Each test names the MUTATION that must redden it — a gate nobody has mutated is not known to
/// gate anything.
#[path = "resize_tests.rs"]
#[cfg(test)]
mod resize_tests;

#[path = "state_key_tests.rs"]
#[cfg(test)]
mod state_key_tests;

#[cfg(test)]
impl Default for WindowState {
    /// Test-only baseline window (C1 Task 2's `pane_model_tests` needs a `WindowState`
    /// with no real geometry/symbol) — delegates to `new` so every field's actual
    /// default-construction logic (`ChartFollowLive::default()`, `ChartSettings::default()`,
    /// …) stays in the ONE place that already owns it.
    fn default() -> Self {
        let r = Rect::from_min_size(Pos2::ZERO, Vec2::splat(100.0));
        Self::new("default", "", "", WindowKind::Chart, r)
    }
}

/// Test-only: the chart window most tests build — id `"t"`, 100x100 at the origin, on binance
/// (`WindowState::new` names no venue; a chart's opener always does). A test that needs another
/// venue or id sets the field on the result.
#[cfg(test)]
pub(crate) fn chart_window(symbol: &str, interval: &str) -> WindowState {
    let r = Rect::from_min_size(Pos2::ZERO, Vec2::splat(100.0));
    let mut w = WindowState::new("t", symbol, interval, WindowKind::Chart, r);
    w.venue = "binance".to_string();
    w.retitle();
    w
}

#[cfg(test)]
impl WindowState {
    /// Test-only (C1 Task 2): add a known Oscillator-kind registry indicator (RSI)
    /// through the REAL `add_indicator` path, so the default-pane-assignment side
    /// effect (`assign_default_pane`) is exercised exactly as production code
    /// triggers it, rather than poking `study_pane`/`pane_order` directly. `name`
    /// only tags the assertion messages below for a friendlier failure — RSI is the
    /// only Oscillator-kind indicator this helper wires up; the REGISTRY (not
    /// `name`) decides `RenderKind`.
    fn add_test_oscillator(&mut self, name: &str) -> u64 {
        let before = self.indicators.len();
        self.add_indicator("rsi", &[]);
        assert_eq!(self.indicators.len(), before + 1, "{name}: rsi must be registered");
        let a = self.indicators.last().expect("just pushed above");
        assert!(!a.is_overlay(), "{name}: rsi must be Oscillator-kind (registry drift?)");
        a.uid
    }
}

#[path = "pane_model_tests.rs"]
#[cfg(test)]
mod pane_model_tests;

#[path = "compare_tests.rs"]
#[cfg(test)]
mod compare_tests;

#[path = "series_pane_model_tests.rs"]
#[cfg(test)]
mod series_pane_model_tests;

/// [`WindowKind::from_slug`] exhaustiveness: every kind round-trips through its slug, slugs are
/// distinct, unknowns parse to `None`, and the legacy `VIKE_TOOL` vocabulary (the silent
/// `_ => Calendar` match in `vike-app`'s `main.rs` this API replaced) still resolves identically.
#[path = "kind_slug_tests.rs"]
#[cfg(test)]
mod kind_slug_tests;
