//! Per-window state (`WinState`, `WinKind`) + the window chrome loop
//! (`show_window`).
//!
//! Each window is a floating `egui::Window`. We let egui own the live geometry
//! (so the user can drag/resize), reading the rect back each frame; for an
//! arrange/maximize action geometry is FORCED for a single frame via `pending`,
//! then released (see [`super::arrange`]).
//!
//! `WinState` deliberately carries only window chrome + chart content state
//! (vike-chart types) — tool-window view state lives in `crate::tools::ToolView`,
//! held by the App keyed by window id, so this module stays free of app content.

use crate::tickvol::BarKind;
use egui::{Id, LayerId, Order, Pos2, Rect, Vec2};

/// Default data venue for a chart window — Binance, the historical single-venue behavior.
/// Cross-exchange symbol search introduces per-window venues (`"bybit"`/`"okx"`); this is the
/// value that keeps every pre-existing path (and every v1/v2 workspace file) byte-identical.
pub const DEFAULT_VENUE: &str = "binance";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WinKind {
    Chart,
    Trade,
    Dom,
    Options,
    Greeks,
    News,
    Calendar,
    Data,
    Studio,
    Connections,
    Tearsheet,
    /// The Polymarket scalp cockpit — mounts the `vike-cockpit` widgets (window-chain rail,
    /// Price-to-Beat header, probability ladder, one-click ticket) for the rolling up/down markets.
    Polymarket,
}

impl WinKind {
    pub fn label(self) -> &'static str {
        match self {
            WinKind::Chart => "Chart",
            WinKind::Trade => "Trade",
            WinKind::Dom => "DOM",
            WinKind::Options => "Options",
            WinKind::Greeks => "Greeks",
            WinKind::News => "News",
            WinKind::Calendar => "Calendar",
            WinKind::Data => "Data Manager",
            WinKind::Studio => "Studio",
            WinKind::Connections => "Connections",
            WinKind::Tearsheet => "Tearsheet",
            WinKind::Polymarket => "Polymarket",
        }
    }
    /// The stable lowercase slug of each kind — the `VIKE_TOOL=<slug>` QA vocabulary and the
    /// inverse of [`Self::from_slug`]. NOT the display [`Self::label`] (`Data` ⇒ `"data"`, never
    /// `"data manager"`). Exhaustive match: a new variant fails to compile until it names a slug.
    pub fn slug(self) -> &'static str {
        match self {
            WinKind::Chart => "chart",
            WinKind::Trade => "trade",
            WinKind::Dom => "dom",
            WinKind::Options => "options",
            WinKind::Greeks => "greeks",
            WinKind::News => "news",
            WinKind::Calendar => "calendar",
            WinKind::Data => "data",
            WinKind::Studio => "studio",
            WinKind::Connections => "connections",
            WinKind::Tearsheet => "tearsheet",
            WinKind::Polymarket => "polymarket",
        }
    }
    /// Parse a [`Self::slug`] back to its kind — the explicit inverse that replaced the silent
    /// `_ => Calendar` fallback match at `vike-app`'s `VIKE_TOOL` read (the CALLER now chooses the
    /// fallback via `unwrap_or`). `None` for anything that is not exactly a known slug
    /// (case-sensitive, like the match it replaced). Resolved over [`Self::ALL`] against
    /// [`Self::slug`], so the two directions can never disagree.
    pub fn from_slug(s: &str) -> Option<WinKind> {
        Self::ALL.into_iter().find(|k| k.slug() == s)
    }
    pub fn icon(self) -> &'static str {
        match self {
            WinKind::Chart => "📈",
            WinKind::Trade => "💹",
            WinKind::Dom => "🪜",
            WinKind::Options => "🎯",
            WinKind::Greeks => "🧮",
            WinKind::News => "📰",
            WinKind::Calendar => "📅",
            WinKind::Data => "🗄",
            WinKind::Studio => "🧪",
            WinKind::Connections => "🔌",
            WinKind::Tearsheet => "📊",
            WinKind::Polymarket => "🎲",
        }
    }
    /// Per-tool accent for the launcher icon (vike uses colored icons, not mono).
    pub fn color(self) -> egui::Color32 {
        match self {
            WinKind::Chart => egui::Color32::from_rgb(91, 190, 145), // green
            WinKind::Trade => egui::Color32::from_rgb(62, 224, 138), // accent green
            WinKind::Dom => egui::Color32::from_rgb(240, 180, 41),   // amber (last-trade cell)
            WinKind::Options => egui::Color32::from_rgb(245, 184, 64), // gold
            WinKind::Greeks => egui::Color32::from_rgb(94, 208, 187), // teal
            WinKind::News => egui::Color32::from_rgb(87, 165, 255),  // blue
            WinKind::Calendar => egui::Color32::from_rgb(236, 64, 122), // pink
            WinKind::Data => egui::Color32::from_rgb(38, 198, 218),  // cyan
            WinKind::Studio => egui::Color32::from_rgb(170, 130, 250), // violet
            WinKind::Connections => egui::Color32::from_rgb(255, 138, 101), // orange
            WinKind::Tearsheet => egui::Color32::from_rgb(120, 200, 140), // green (performance)
            WinKind::Polymarket => egui::Color32::from_rgb(46, 189, 133), // Polymarket YES green
        }
    }
    /// Launcher row order (left→right).
    pub const LAUNCHERS: [WinKind; 9] = [
        WinKind::Chart,
        WinKind::Trade,
        WinKind::Dom,
        WinKind::Options,
        WinKind::Greeks,
        WinKind::News,
        WinKind::Calendar,
        WinKind::Data,
        WinKind::Tearsheet,
    ];
    /// Every kind, in declaration order — the roster [`Self::from_slug`] resolves over and the
    /// exhaustiveness test iterates (unlike [`Self::LAUNCHERS`], which is the 9-entry launcher
    /// row). Grow it with the enum: the exhaustive [`Self::slug`] match already fails to compile
    /// on a new variant, which puts you in this impl block next to this list.
    pub const ALL: [WinKind; 12] = [
        WinKind::Chart,
        WinKind::Trade,
        WinKind::Dom,
        WinKind::Options,
        WinKind::Greeks,
        WinKind::News,
        WinKind::Calendar,
        WinKind::Data,
        WinKind::Studio,
        WinKind::Connections,
        WinKind::Tearsheet,
        WinKind::Polymarket,
    ];
}

pub struct WinState {
    pub id: Id,
    pub title: String,
    pub symbol: String,   // e.g. "BTCUSDT" (chart windows)
    pub interval: String, // e.g. "1m" (chart windows)
    /// Data venue the chart's live bar feed subscribes to (`"binance"`, `"bybit"`, `"okx"`).
    /// Cross-exchange symbol search: a search hit carries its venue, and selecting it sets this
    /// alongside `symbol` so `App::ensure_feed_on` routes to the right feed. `"binance"` is the
    /// zero-behavior-change default — every pre-existing path (quick-picks, workspace v1/v2 files
    /// without a `venue` key, tool windows) stays exactly on Binance, and `key()` keeps its
    /// byte-identical `"SYMBOL@interval"` form for it (see [`Self::key`]).
    pub venue: String,
    /// Asset class of the picked instrument (from the Symbol picker's `Instrument.asset_class`),
    /// threaded through so `App::ensure_feed_on` can route non-spot symbols to their venue's
    /// native product feed (e.g. OKX derivatives) instead of the spot bar feed. `None` is the
    /// legacy/spot default — quick-picks, plain-symbol paths, and every pre-feature window leave
    /// this unset, so `ensure_feed_on`'s spot routing is unchanged. NOT serialized as a bare
    /// `WinState` field (`WinState` itself is never the serde type — see `super::persist::WinSnap`,
    /// which carries the persisted twin of this field the same way it already does for `venue`).
    pub asset_class: Option<vike_catalog::AssetClass>,
    pub kind: WinKind,
    pub open: bool,      // false => hidden off-desktop (rail can unhide)
    pub minimized: bool, // true => not drawn as a Window; shows in the left rail
    pub pos: Pos2,       // our tracked copy of the live geometry
    pub size: Vec2,
    pub pending: Option<Rect>, // one-frame forced geometry (arrange/maximize)
    pub pre_max: Option<Rect>, // rect to restore after un-maximize
    pub maximized: bool,
    pub force_frames: u8, // pin geometry for N frames after arrange/move, then egui owns it
    pub hover: Option<[f64; 4]>, // last hovered bar's OHLC (for the header legend)
    pub style: vike_chart::chart::ChartStyle, // chart style (candles, line, …)
    pub nav: Option<vike_chart::chart::Nav>, // pending nav-button action
    pub indicators: Vec<vike_chart::indicators::Active>,
    pub next_uid: u64,
    pub picker_open: bool,
    pub picker_query: String,
    pub picker_tab: Option<vike_chart::indicators::Category>,
    /// Indicator-target feature (part a): where the NEXT ƒx-picker add should land.
    /// `PaneTarget::Auto` (the default) reproduces today's `RenderKind` routing
    /// byte-for-byte; the picker's "Add to" selector writes the other variants.
    /// Runtime picker state only (like `picker_query`/`picker_tab`) — not persisted.
    pub picker_target: vike_chart::chart::PaneTarget,
    pub follow: vike_chart::chart::FollowLive, // follow-live-edge state (chart windows)
    /// Committed chart appearance/behavior (chart-UX bundle T6). `options.show_volume`
    /// is the single in-memory source of truth for the volume pane (moved off
    /// the old `WinState::show_volume`); the "Chart settings" dialog edits a
    /// working copy and commits here on OK. `settings` holds that dialog's
    /// per-window open/working state (cross-frame).
    pub options: vike_chart::chart::ChartOptions,
    pub settings: vike_chart::chart::SettingsDialog,
    /// Per-window "Indicator settings" dialog state (chart-UX bundle T8): which
    /// active indicator's settings window is open (at most one) + its working /
    /// snapshot edit copies. Opened from an oscillator pane-header ⚙ (inside
    /// `chart::draw`) or a ƒx-picker per-row ⚙ (main.rs); the resulting live
    /// edits are applied to `indicators` via `chart::ChartActions::indicator_edit`.
    pub indicator_dialog: vike_chart::chart::IndicatorDialog,
    /// Requested price-scale mode (chart-UX bundle T3): the persisted twin of
    /// `chart::ChartInputs::scale` — main.rs reads it in, then applies
    /// `chart::ChartActions::scale_change` (toggle click / Alt+L) back onto
    /// it after each frame.
    pub scale: vike_chart::ScaleMode,
    /// TradingView "Invert scale" flag: the persisted twin of
    /// `chart::ChartInputs::invert` — an orthogonal modifier (flips the y-axis
    /// vertically) usable atop any `scale`. main.rs reads it in and applies
    /// `chart::ChartActions::invert_change` (the price-axis right-click toggle)
    /// back onto it after each frame.
    pub invert: bool,
    /// Per-pane sub-pane height fractions (chart-UX bundle T9): the persisted
    /// twin of `chart::ChartInputs::panes` — identity-keyed (by `PaneKey`, an
    /// oscillator's stable `Active::uid`) so a hidden/re-shown pane or a
    /// reordered indicator list never scrambles which stored share belongs to
    /// which pane. Mutated in place by `chart::draw` (layout reads + resolves
    /// new-pane defaults; separator drags write new shares); main.rs takes it
    /// out via `mem::take` before `show_window` (same dance as `follow`/
    /// `settings`/`indicator_dialog` — `w` itself is borrowed by that call) and
    /// writes it back after.
    pub panes: vike_chart::chart::PaneFractions,
    /// Cross-window chart sync group membership (chart sync seam, task B8): `1..=4`, or
    /// `None` when ungrouped. Windows sharing a group broadcast/receive a ghost crosshair
    /// and a sticky-leader visible range through `App::{sync_prev,sync_next,range_leader}`
    /// (main.rs) — this field is only the persisted membership tag; the title-bar chip
    /// (main.rs `title_bar`) reads/cycles it. `None` is the zero-visual-change default.
    pub sync_group: Option<u8>,
    /// SP2 orderflow (Task 7): show the CVD sub-pane this frame. Default `false` — zero
    /// behavior change until the user opts in via the title-bar Orderflow popup (`main.rs
    /// title_bar`). Harvested back to `false` when the CVD pane's own ✕ is clicked
    /// (`chart::ChartActions::cvd_toggle`, see the window loop in `main.rs`).
    pub cvd_on: bool,
    /// SP2 orderflow (Task 7): show the volume-profile overlay this frame. Default `false`.
    pub profile_on: bool,
    /// SP2 orderflow (Task 7): volume-profile / footprint bucket width override in raw price
    /// units. `None` (default) defers to the aggregator's own default (see
    /// `orderflow::OrderflowAgg::new`); `Some(x)` pins it. Set from the title-bar Orderflow
    /// popup's "Tick size" field.
    pub of_tick_size: Option<f64>,
    /// C1 Task 2: authored order of STUDY (oscillator) panes, top→bottom, below the
    /// price/volume/CVD panes. Populated by [`WinState::assign_default_pane`] (on
    /// [`WinState::add_indicator`]) and [`WinState::move_study`]; pruned of empty
    /// panes by `drop_empty_study_panes`. NOT YET read by rendering — Task 3 wires
    /// the read side (`WinState::present_study_panes` is the intended read seam),
    /// so populating this field this task is a pure-model change with zero runtime
    /// effect.
    pub pane_order: Vec<vike_chart::chart::PaneKey>,
    /// C1 Task 2: which pane a given oscillator study (`Active::uid`) lives in.
    /// Overlay studies are absent (they always render in the price pane and never
    /// enter this map). `IndexMap` for the repo-wide insertion-order convention
    /// (not load-bearing for f64 sums here, just consistency — see root
    /// `Cargo.toml`'s indexmap rationale comment).
    pub study_pane: indexmap::IndexMap<u64, vike_chart::chart::PaneKey>,
    /// C1 Task 2: allocator for fresh `PaneKey::Study(id)` panes — a counter
    /// INDEPENDENT of `next_uid` (pane identity, not indicator identity: merging
    /// two studies into one pane, or moving a study to a new pane, must not
    /// consume/collide with indicator uids). Starts at 1.
    pub next_pane_id: u64,
    /// C2a Task 4: symbols overlaid in the price pane as %-normalized "Compare"
    /// series (same `interval` as this window), rendered on top of the primary.
    /// Add-order IS color-order (`main.rs`'s `COMPARE_COLORS` gather indexes by
    /// position); deduped and order-preserving; never contains the window's OWN
    /// `symbol`. Each entry's `ChartState` is looked up (never owned) in
    /// `App.charts` by `"{sym}@{interval}"` and ensure-synced like the primary.
    /// EMPTY is the zero-visual-change default — an empty overlay list feeds
    /// `chart::draw` the same `&[]` the pre-Task-4 single-series render did. Not
    /// persisted (workspace round-trip is C2b). See [`WinState::add_compare`].
    pub compare: Vec<String>,
    /// C2b Task 5: authored top→bottom order of panes each holding a compare
    /// symbol moved OUT of the price-pane overlay (mirrors `pane_order`, but
    /// for per-symbol "own pane" placement rather than per-study). Populated
    /// by [`WinState::move_series_to_new_pane`]/[`WinState::move_series`];
    /// pruned of empty panes by `drop_empty_series_panes`. Read by rendering
    /// via [`WinState::present_series_panes`] (its non-empty panes become the
    /// compare-series sub-panes below price, threaded into `chart::draw` as
    /// `ChartInputs::series_panes` in main.rs).
    pub series_pane_order: Vec<vike_chart::chart::PaneKey>,
    /// C2b Task 5: which pane a given compare symbol (an entry of `compare`)
    /// lives in. ABSENT means the symbol is overlaid in the price pane as a
    /// %-normalized line — `compare`'s only behavior pre-C2b, and every
    /// compare symbol's default here too. Unlike C1's `study_pane`, there is
    /// NO assign-on-add: `add_compare` never touches this map; only
    /// `move_series_to_new_pane` inserts an entry. `IndexMap` for the
    /// repo-wide insertion-order convention (see root `Cargo.toml`'s indexmap
    /// rationale comment), keyed by the symbol string since compare symbols
    /// have no stable integer uid the way indicators do.
    pub series_pane: indexmap::IndexMap<String, vike_chart::chart::PaneKey>,
    /// C2b Task 7 ("Pin to scale ▸ Right"): per-compare-symbol secondary-axis
    /// assignment. ABSENT ⇒ [`vike_chart::chart::ScaleAssign::Percent`] (the default):
    /// the symbol is a %-line on the shared % axis (C2a). `Right` pins it to its
    /// OWN absolute price axis on the right — its real price range remapped into
    /// the price pane's plot-space, filling the pane on its own scale. Set by the
    /// Task-9 "Pin to scale" menu; read via [`WinState::series_scale_of`] and
    /// threaded into `chart::draw` as `ChartInputs::series_scale`. `IndexMap` for
    /// the repo-wide insertion-order convention, keyed by the compare symbol
    /// string (like `series_pane`). EMPTY by default ⇒ every overlay stays a
    /// %-line ⇒ byte-identical to the C2a render.
    pub series_scale: indexmap::IndexMap<String, vike_chart::chart::ScaleAssign>,
    /// C2b Task 5: allocator for fresh `PaneKey::Series(id)` panes — a
    /// counter INDEPENDENT of `next_uid`/`next_pane_id` (its own namespace: a
    /// `PaneKey::Series(3)` and a `PaneKey::Study(3)` are unrelated pane keys,
    /// so the counters never need to agree). Starts at 1.
    pub next_series_pane_id: u64,
}

/// Frames to pin a window's geometry to its intended `size` right after it is
/// created or restored from a workspace. Without this, a freshly-shown
/// `egui::Window` (resizable, unpinned) auto-sizes DOWN to its content's natural
/// height over the first few frames — the same auto-shrink the maximized path
/// already pins against (see `show_window`) — because `chart::draw`'s `avail`
/// clamp under-reports height until the clip-rect settles. Pinning briefly on
/// open (via `force_frames`, exactly like the arrange/move path) holds the
/// intended size through those unsettled frames, then releases so every-edge
/// resize still works. Applies to ALL window kinds (chart, DOM, tools).
const OPEN_PIN_FRAMES: u8 = 8;

impl WinState {
    pub fn new(id_src: &str, symbol: &str, interval: &str, kind: WinKind, rect: Rect) -> Self {
        Self {
            id: Id::new(id_src),
            title: title_for(DEFAULT_VENUE, symbol, interval),
            symbol: symbol.to_string(),
            interval: interval.to_string(),
            venue: DEFAULT_VENUE.to_string(),
            asset_class: None,
            kind,
            open: true,
            minimized: false,
            pos: rect.min,
            size: rect.size(),
            pending: None,
            pre_max: None,
            maximized: false,
            force_frames: OPEN_PIN_FRAMES, // pin the intended size on open so egui can't auto-shrink it
            hover: None,
            style: vike_chart::chart::ChartStyle::Candles,
            nav: None,
            indicators: Vec::new(),
            next_uid: 0,
            picker_open: false,
            picker_query: String::new(),
            picker_tab: None,
            picker_target: vike_chart::chart::PaneTarget::Auto,
            follow: vike_chart::chart::FollowLive::default(),
            options: vike_chart::chart::ChartOptions::default(),
            settings: vike_chart::chart::SettingsDialog::default(),
            indicator_dialog: vike_chart::chart::IndicatorDialog::default(),
            scale: vike_chart::ScaleMode::Linear,
            invert: false,
            panes: vike_chart::chart::PaneFractions::default(),
            sync_group: None,
            cvd_on: false,
            profile_on: false,
            of_tick_size: None,
            pane_order: Vec::new(),
            study_pane: indexmap::IndexMap::new(),
            next_pane_id: 1,
            compare: Vec::new(),
            series_pane_order: Vec::new(),
            series_pane: indexmap::IndexMap::new(),
            series_scale: indexmap::IndexMap::new(),
            next_series_pane_id: 1,
        }
    }

    /// Add study `name` to this window, or do NOTHING if the name resolves to no indicator.
    ///
    /// ⚠ `get_any`, not `get`: the lookup is the union of the built-in catalog and the USER
    /// studies a binary installed at startup, so a `user_data/indicators/<name>.rhai` is added
    /// exactly like a built-in. The silent no-op for an unresolvable name is LOAD-BEARING and
    /// unchanged — a workspace persists studies by NAME, so this is also the path a restore takes
    /// for a user indicator whose file has since been deleted, renamed or broken.
    pub fn add_indicator(&mut self, name: &str, bars: &[vike_chart::model::Bar]) {
        if let Some(spec) = vike_chart::indicators::get_any(name) {
            let uid = self.next_uid;
            self.next_uid += 1;
            let active = vike_chart::indicators::Active::new(uid, spec, bars);
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
    /// [`vike_chart::chart::resolve_add_target`] maps `(RenderKind, target)` to an
    /// optional relocation — `None` (its answer for `PaneTarget::Auto`, every overlay,
    /// and an oscillator's default new/price pane) leaves `add_indicator`'s placement
    /// untouched, so `add_indicator_to(name, bars, Auto)` is BYTE-IDENTICAL to
    /// `add_indicator(name, bars)`. Only an oscillator + `Existing(pane)` relocates
    /// (a merge into that study pane). A no-op for an unknown indicator name.
    pub fn add_indicator_to(
        &mut self,
        name: &str,
        bars: &[vike_chart::model::Bar],
        target: vike_chart::chart::PaneTarget,
    ) {
        let uid = self.next_uid; // the uid `add_indicator` will assign IF `name` is known
        self.add_indicator(name, bars);
        if self.next_uid == uid {
            return; // unknown name → nothing added (add_indicator left next_uid untouched)
        }
        if let Some(spec) = vike_chart::indicators::get_any(name) {
            if let Some(mt) = vike_chart::chart::resolve_add_target(spec.kind, target) {
                self.move_study(uid, mt);
            }
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
        let pane = vike_chart::chart::PaneKey::Study(self.next_pane_id);
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
    pub fn move_study(&mut self, uid: u64, target: vike_chart::chart::MoveTarget) {
        use vike_chart::chart::MoveTarget;
        if !self.study_pane.contains_key(&uid) {
            return;
        }
        match target {
            MoveTarget::Into(pane) => {
                // Only move into a pane that is actually live in `pane_order`. Without
                // this guard a stale `PaneKey` (e.g. a pane dropped earlier, or a key
                // from another window — `Study` ids are per-WinState, not global) would
                // point `study_pane[uid]` at a pane that `present_study_panes()` never
                // yields, silently rendering the study in zero panes once Task 3 wires
                // the read side. Mirrors the `NewAbove/NewBelow` anchor-not-present guard.
                if self.pane_order.contains(&pane) {
                    self.study_pane.insert(uid, pane);
                }
            }
            MoveTarget::NewAbove(anchor) | MoveTarget::NewBelow(anchor) => {
                let pane = vike_chart::chart::PaneKey::Study(self.next_pane_id);
                self.next_pane_id += 1;
                let at = match self.pane_order.iter().position(|&p| p == anchor) {
                    Some(i) if matches!(target, MoveTarget::NewAbove(_)) => i,
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
    /// one `study_pane` member — the read seam persistence uses (`panes_to_snap`/
    /// `pane_membership_snap`). Volume/CVD entries in `pane_order` (chart
    /// single-max default) are skipped: they are never in `study_pane.values()`.
    /// Defensive filter on top of `drop_empty_study_panes` already being called
    /// after every mutation, so this is never stale.
    pub fn present_study_panes(&self) -> Vec<vike_chart::chart::PaneKey> {
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
    /// this once per frame BEFORE reading [`present_sub_panes`] so a freshly
    /// toggled pane is placed exactly like a study pane. Idempotent.
    pub fn sync_sub_panes(&mut self) {
        use vike_chart::chart::PaneKey;
        let show_volume = self.options.show_volume;
        let cvd_on = self.cvd_on;
        let has_vol = self.pane_order.contains(&PaneKey::Volume);
        if show_volume && !has_vol {
            self.pane_order.insert(0, PaneKey::Volume);
        } else if !show_volume && has_vol {
            self.pane_order.retain(|p| *p != PaneKey::Volume);
        }
        let has_cvd = self.pane_order.contains(&PaneKey::Cvd);
        if cvd_on && !has_cvd {
            let at = self
                .pane_order
                .iter()
                .position(|p| *p == PaneKey::Volume)
                .map(|i| i + 1)
                .unwrap_or(0);
            self.pane_order.insert(at, PaneKey::Cvd);
        } else if !cvd_on && has_cvd {
            self.pane_order.retain(|p| *p != PaneKey::Cvd);
        }
    }

    /// Chart single-max default: the authored UNIFIED sub-pane order — Volume,
    /// CVD, and study panes as PEERS, in `pane_order` order, filtered to those
    /// logically present (Volume iff `show_volume`, CVD iff `cvd_on`, a study
    /// pane iff it still holds a member). The read seam threaded into
    /// `chart::draw` as `ChartInputs::sub_panes`; `resolve_pane_layout` applies
    /// the finer style/footprint/visibility gating on top. Call
    /// [`sync_sub_panes`](Self::sync_sub_panes) first so Volume/CVD membership is
    /// up to date.
    pub fn present_sub_panes(&self) -> Vec<vike_chart::chart::PaneKey> {
        use vike_chart::chart::PaneKey;
        self.pane_order
            .iter()
            .copied()
            .filter(|p| match p {
                PaneKey::Volume => self.options.show_volume,
                PaneKey::Cvd => self.cvd_on,
                PaneKey::Study(_) => self.study_pane.values().any(|v| v == p),
                PaneKey::Price | PaneKey::Series(_) => false,
            })
            .collect()
    }

    /// Feature #1 (TradingView parity) — chart single-max default: move a sub-pane
    /// (a study pane, Volume, or CVD — all peers now) one slot up (`up = true`) or
    /// down within the PRESENT sub-pane group, driven by the pane header's ↑/↓
    /// controls. Adjacency is resolved over [`present_sub_panes`] (so a
    /// pruned/empty/absent entry never traps the move), then the two panes are
    /// swapped in the authored `pane_order` — the render order reflects it next
    /// frame. A no-op at the group's ends or for an unknown/absent `pane`.
    pub fn reorder_pane(&mut self, pane: vike_chart::chart::PaneKey, up: bool) {
        let present = self.present_sub_panes();
        let Some(pos) = present.iter().position(|&p| p == pane) else { return };
        let swap_with = if up {
            if pos == 0 {
                return;
            }
            present[pos - 1]
        } else {
            if pos + 1 >= present.len() {
                return;
            }
            present[pos + 1]
        };
        if let (Some(i), Some(j)) = (
            self.pane_order.iter().position(|&p| p == pane),
            self.pane_order.iter().position(|&p| p == swap_with),
        ) {
            self.pane_order.swap(i, j);
        }
    }

    /// C2a Task 4: overlay `sym` as a %-normalized Compare series in the price
    /// pane. Order-preserving (add-order == color-order in `main.rs`'s
    /// `COMPARE_COLORS` gather), deduped (no-op if already overlaid), and it
    /// NEVER adds the window's own `symbol` (case-insensitive) — a chart doesn't
    /// overlay itself.
    ///
    /// On the FIRST overlay (list was empty → now non-empty) it forces
    /// `self.scale` to [`ScaleMode::Percent`](vike_chart::ScaleMode::Percent): the
    /// vike-chart overlay render (Task 3) only draws overlays in Percent mode (a
    /// raw %-line can't share a Linear/Log price axis), so without this flip the
    /// freshly-added overlay would be invisible. An IGNORED add (own symbol) does
    /// NOT count as the first overlay, so it never flips the scale. Restoring the
    /// pre-overlay scale on removal is intentionally out of scope — the user
    /// toggles the scale back manually.
    pub fn add_compare(&mut self, sym: &str) {
        if sym.eq_ignore_ascii_case(&self.symbol) {
            return; // never overlay the window's own series
        }
        if self.compare.iter().any(|s| s == sym) {
            return; // already overlaid
        }
        let was_empty = self.compare.is_empty();
        self.compare.push(sym.to_string());
        if was_empty && self.scale != vike_chart::ScaleMode::Percent {
            // FIRST overlay: auto-switch so it's visible (Task 3 gates on Percent).
            self.scale = vike_chart::ScaleMode::Percent;
        }
    }

    /// C2a Task 4: remove `sym` from the Compare overlays (exact match; no-op if
    /// absent). Does NOT restore the pre-`add_compare` scale (see that method).
    /// Also cascades into the C2b series-pane model — mirrors how `remove_indicator`
    /// cleans up `study_pane` — so removing a symbol that had its own pane doesn't
    /// leave an orphaned `series_pane` entry / empty pane behind. C2b Task 7 review
    /// Minor-1: the secondary-axis pin (`series_scale`) is dropped too, so re-adding
    /// the same symbol later defaults back to `Percent` rather than resurrecting a
    /// stale `Right` pin.
    pub fn remove_compare(&mut self, sym: &str) {
        self.compare.retain(|s| s != sym);
        self.series_pane.shift_remove(sym);
        self.series_scale.shift_remove(sym);
        self.drop_empty_series_panes();
    }

    /// C2b Task 5: move `symbol` (a compare series, see `compare`) from the
    /// price-pane %-overlay into its OWN pane — the "Move to own pane"
    /// action. Allocates a fresh `PaneKey::Series(next_series_pane_id)`,
    /// appended to `series_pane_order`. Unlike C1's `assign_default_pane`,
    /// there is NO assign-on-add: `add_compare` never touches `series_pane`,
    /// so this is the ONLY way a symbol enters it. No-op if `symbol` is
    /// already in its own pane.
    #[allow(dead_code)] // wired by chart.rs/main.rs in C2 Task 6/9 ("Move to own pane" action); exercised by tests today
    pub fn move_series_to_new_pane(&mut self, symbol: &str) {
        if self.series_pane.contains_key(symbol) {
            return; // already in its own pane
        }
        let pane = vike_chart::chart::PaneKey::Series(self.next_series_pane_id);
        self.next_series_pane_id += 1;
        self.series_pane_order.push(pane);
        self.series_pane.insert(symbol.to_string(), pane);
    }

    /// C2b Task 5: return `symbol` from its own pane back to the price-pane
    /// %-overlay (removes it from `series_pane`; a no-op if it's already
    /// overlaid or unknown). `drop_empty_series_panes` then prunes the
    /// vacated pane from `series_pane_order` if `symbol` was its last member —
    /// mirrors `remove_indicator`'s pane-side cleanup.
    #[allow(dead_code)] // wired by chart.rs/main.rs in C2 Task 6/9 ("Move to overlay" action); exercised by tests today
    pub fn overlay_series(&mut self, symbol: &str) {
        self.series_pane.shift_remove(symbol);
        self.drop_empty_series_panes();
    }

    /// C2b Task 5: move series `symbol` (already in its own pane) per
    /// `target`, mirroring `move_study` exactly: `Into(pane)` re-points
    /// `series_pane[symbol]` at an existing pane (merging it in); `NewAbove`/
    /// `NewBelow` allocate a fresh pane and insert it into `series_pane_order`
    /// immediately adjacent to `anchor`. Either way, the series' PREVIOUS pane
    /// may end up with no members — `drop_empty_series_panes` prunes it
    /// afterward. A no-op if `symbol` is not currently in its own pane
    /// (unknown symbol, or still overlaid — call `move_series_to_new_pane`
    /// first).
    #[allow(dead_code)] // wired by chart.rs/main.rs in C2 Task 6/9 (pane reorder/merge "Move to" menu); exercised by tests today
    pub fn move_series(&mut self, symbol: &str, target: vike_chart::chart::MoveTarget) {
        use vike_chart::chart::MoveTarget;
        if !self.series_pane.contains_key(symbol) {
            return;
        }
        match target {
            MoveTarget::Into(pane) => {
                // Only move into a pane that is actually live in
                // `series_pane_order`. Without this guard a stale `PaneKey`
                // (a pane dropped earlier, or a foreign key — `Series` ids
                // are per-WinState, not global, and distinct from `Study`
                // ids) would point `series_pane[symbol]` at a pane
                // `present_series_panes()` never yields, silently rendering
                // the series in zero panes. Mirrors `move_study`'s `Into`
                // guard verbatim.
                if self.series_pane_order.contains(&pane) {
                    self.series_pane.insert(symbol.to_string(), pane);
                }
            }
            MoveTarget::NewAbove(anchor) | MoveTarget::NewBelow(anchor) => {
                let pane = vike_chart::chart::PaneKey::Series(self.next_series_pane_id);
                self.next_series_pane_id += 1;
                let at = match self.series_pane_order.iter().position(|&p| p == anchor) {
                    Some(i) if matches!(target, MoveTarget::NewAbove(_)) => i,
                    Some(i) => i + 1,                     // NewBelow
                    None => self.series_pane_order.len(), // anchor not present (defensive) — append
                };
                self.series_pane_order.insert(at, pane);
                self.series_pane.insert(symbol.to_string(), pane);
            }
        }
        self.drop_empty_series_panes();
    }

    /// C2b Task 5: remove any pane from `series_pane_order` with no
    /// remaining `series_pane` member. Mirrors `drop_empty_study_panes` —
    /// called after every mutation that can empty a pane (`overlay_series`,
    /// `move_series`).
    #[allow(dead_code)] // only called from the (also currently-unwired) methods above; exercised via them by tests
    fn drop_empty_series_panes(&mut self) {
        self.series_pane_order.retain(|p| self.series_pane.values().any(|v| v == p));
    }

    /// C2b Task 5: `series_pane_order` filtered to panes that still have at
    /// least one `series_pane` member — the read seam the render/present build
    /// uses (threaded into `chart::draw` as `ChartInputs::series_panes` in
    /// main.rs). Mirrors `present_study_panes`.
    pub fn present_series_panes(&self) -> Vec<vike_chart::chart::PaneKey> {
        self.series_pane_order
            .iter()
            .copied()
            .filter(|p| self.series_pane.values().any(|v| v == p))
            .collect()
    }

    /// C2b Task 7 ("Pin to scale"): the [`vike_chart::chart::ScaleAssign`] for a
    /// compare symbol — its stored pin, or the `Percent` default when unpinned
    /// (the C2a shared-% behavior). The read seam the render path uses to route
    /// each overlay to the shared % axis vs its own secondary (Right) price axis.
    #[allow(dead_code)] // set + read by the Task-9 "Pin to scale" menu
    pub fn series_scale_of(&self, sym: &str) -> vike_chart::chart::ScaleAssign {
        self.series_scale.get(sym).copied().unwrap_or_default()
    }

    #[allow(dead_code)] // per-frame sync is done inline in the window loop via Active::update
    pub fn recompute_indicators(&mut self, bars: &[vike_chart::model::Bar]) {
        for a in &mut self.indicators {
            a.recompute_full(bars);
        }
    }

    /// A non-chart tool window (Options/News/Calendar/Data).
    pub fn tool(id_src: &str, kind: WinKind, rect: Rect) -> Self {
        let mut s = Self::new(id_src, "", "", kind, rect);
        s.title = kind.label().to_string();
        s
    }

    /// Feed/chart key. Binance keeps its historical byte-identical `"SYMBOL@interval"` form (so
    /// every existing `charts`/`subs`/`spawned` key, persisted or in-flight, is unchanged); other
    /// venues are namespaced `"venue:SYMBOL@interval"` so the same symbol on two venues (e.g.
    /// Binance vs Bybit `BTCUSDT`) never collides in those maps. See [`series_key`].
    pub fn key(&self) -> String {
        series_key(&self.venue, &self.symbol, &self.interval)
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
    /// an `orderflow::OrderflowAgg` — never on a fresh, all-default window.
    pub fn orderflow_on(&self) -> bool {
        self.cvd_on
            || self.profile_on
            || (matches!(BarKind::parse(&self.interval), BarKind::Kline(_))
                && self.style == vike_chart::chart::ChartStyle::Footprint)
    }
}

fn title_for(venue: &str, symbol: &str, interval: &str) -> String {
    // Binance stays exactly "SYMBOL · interval" (zero visual change); other venues get an
    // upper-cased venue prefix so a Bybit/OKX chart is unambiguous in the title bar and taskbar.
    if venue == DEFAULT_VENUE {
        format!("{symbol} · {interval}")
    } else {
        format!("{} {symbol} · {interval}", venue.to_uppercase())
    }
}

/// Feed/chart key builder shared by [`WinState::key`] and the app's feed-routing sites so the two
/// can never drift. Binance is byte-identical to the historical `"SYMBOL@interval"`; every other
/// venue is namespaced `"venue:SYMBOL@interval"` (venue lower-cased, matching the `feeds` map keys)
/// so identical symbols on different venues occupy distinct `charts`/`subs`/`spawned` slots.
pub fn series_key(venue: &str, symbol: &str, interval: &str) -> String {
    if venue == DEFAULT_VENUE {
        format!("{symbol}@{interval}")
    } else {
        format!("{venue}:{symbol}@{interval}")
    }
}

/// Show one window for this frame: consume any forced geometry, draw, read the
/// live rect back. `extra` lets the caller add per-window title-bar controls
/// (e.g. a Maximize button) and returns whether maximize was toggled.
/// Show one window with a CUSTOM single-row title bar (no egui title bar). `draw`
/// renders the title bar (first row) + content and RETURNS the title bar's drag
/// delta in screen points, which we apply to the window position. Geometry is
/// app-controlled via `current_pos` so the custom bar drives movement.
/// Returns `true` if the user dragged this window's title bar this frame (a manual move),
/// so the caller can drop any remembered tiling mode (don't re-tile a hand-placed layout).
pub fn show_window(
    ctx: &egui::Context,
    w: &mut WinState,
    bounds: Rect,
    draw: impl FnOnce(&mut egui::Ui) -> egui::Vec2,
) -> bool {
    let forced = w.pending.take();
    if let Some(rect) = forced {
        w.pos = rect.min;
        w.size = rect.size();
        w.force_frames = 4; // pin the arranged geometry briefly so egui commits it, then release
    }
    // Keep the window inside the workspace: its top can't go above the main caption's bottom,
    // nor past the status bar / rail (ctx.content_rect doesn't exclude our sub-ui caption panel).
    w.pos.x = w.pos.x.clamp(bounds.min.x, (bounds.max.x - 80.0).max(bounds.min.x));
    w.pos.y = w.pos.y.clamp(bounds.min.y, (bounds.max.y - 40.0).max(bounds.min.y));

    // While pinning (just after arrange/move) force geometry so it commits; otherwise let egui
    // OWN pos+size and read it back — that's what makes resize work from EVERY edge (forcing
    // current_pos every frame would snap top/left-edge resizes back).
    // A MAXIMIZED window must stay pinned to the full bounds EVERY frame — otherwise egui's
    // auto-sizing shrinks it back to the content's natural size (the "maximize doesn't fill /
    // tiles don't resize" bug). force_frames pins briefly after arrange/move (then releases for
    // all-edge resize); maximized pins permanently until restored.
    if w.maximized {
        w.pos = bounds.min;
        w.size = bounds.size();
    }
    // FILL-window kinds (Chart plot, DOM ladder) are ALWAYS pinned to `w.size` (like a maximized
    // window), never left to egui's native resize. An egui `Window` auto-sizes to its content; a
    // fill-window's body is made to fill exactly `ui.max_rect()` (the chart's `leftover` fill; the
    // DOM sizes its ladder region to `available_rect_before_wrap`), so a pinned body's content
    // height == the window height and egui keeps the fixed size. Leaving such a body to egui's
    // native auto-size is a run-away SHRINK loop: because the body reports whatever height egui
    // hands it, each frame's read-back is a hair shorter than the window, so `default_size` creeps
    // down toward the content floor every frame (the DOM "resizes down at open" bug). We drive
    // resize ourselves via the edge handles below (the same pattern as the custom title-bar move).
    // Intrinsic-height tool windows (Options, News, …) don't fill, so they keep egui-native resize.
    // The Polymarket cockpit fills like the DOM: its probability ladder sizes its region to
    // `available_rect_before_wrap`, so the same pinned-body treatment keeps the window from
    // auto-shrinking to the ladder's content floor.
    let fills = matches!(w.kind, WinKind::Chart | WinKind::Dom | WinKind::Polymarket);
    let pinning = w.force_frames > 0 || w.maximized || fills;
    let mut win = egui::Window::new("")
        .id(w.id)
        .title_bar(false) // custom title bar lives inside the content
        .movable(false) // we move the window via the custom bar's drag
        .resizable(!fills) // fill windows use our own edge handles; tools keep egui-native resize
        .constrain_to(bounds);
    win = if pinning {
        win.current_pos(w.pos).fixed_size(w.size)
    } else {
        win.default_pos(w.pos).default_size(w.size)
    };

    // Custom edge/corner resize for fill windows (native resize is off while pinned). Disabled
    // while maximized or during a forced-geometry pin (arrange/restore settle).
    let resizable_now = fills && !w.maximized && w.force_frames == 0;
    let wid = w.id;
    let mut resize: Option<(u8, egui::Vec2)> = None;
    let mut drag = egui::Vec2::ZERO;
    let resp = win.show(ctx, |ui| {
        drag = draw(ui);
        if resizable_now {
            resize = window_resize_handles(ui, wid);
        }
    });
    if let Some((mask, d)) = resize {
        const MINW: f32 = 240.0;
        const MINH: f32 = 160.0;
        if mask & 2 != 0 {
            w.size.x = (w.size.x + d.x).max(MINW);
        }
        if mask & 8 != 0 {
            w.size.y = (w.size.y + d.y).max(MINH);
        }
        if mask & 1 != 0 {
            let nx = (w.size.x - d.x).max(MINW);
            w.pos.x += w.size.x - nx;
            w.size.x = nx;
        }
        w.force_frames = w.force_frames.max(1); // commit the new size on the next frame
    }

    if let Some(r) = resp {
        // Only sync geometry back from egui when it OWNS the geometry (non-pinned tool windows,
        // native resize). For pinned windows (maximized / force-pinned / chart) `w.size`/`w.pos`
        // are authoritative — set by maximize, arrange, or our own resize handles — and reading
        // back the fixed size here would clobber a just-applied resize.
        if !pinning {
            let rect = r.response.rect;
            w.size = rect.size();
            w.pos = rect.min; // egui owns geometry on free frames — sync back (all-edge resize)
        }
    }
    if w.force_frames > 0 {
        w.force_frames -= 1;
    }
    let moved = drag != egui::Vec2::ZERO;
    if moved {
        w.pos += drag;
        w.force_frames = w.force_frames.max(1); // re-pin so the dragged pos is applied next frame
    }
    if forced.is_some() {
        ctx.move_to_top(LayerId::new(Order::Middle, w.id));
    }
    moved
}

/// Custom edge/corner resize handles for a pinned chart window (native egui resize is off).
/// Allocates invisible drag-sensing bands along the LEFT, RIGHT and BOTTOM edges + the two
/// BOTTOM corners of the content rect, drawn AFTER the chart so they win the interaction over the
/// plot underneath. The TOP edge is skipped (it overlaps the title bar's move-drag + controls);
/// side bands start below the title bar for the same reason. Returns `(edge_mask, drag_delta)` for
/// the active handle this frame: bit 1 = left, 2 = right, 8 = bottom. `show_window` applies the
/// delta to `w.pos`/`w.size`. The chart fills `max_rect` exactly, so `max_rect.max.y` is the
/// window's visible bottom edge — the bottom band lands where the user grabs.
fn window_resize_handles(ui: &egui::Ui, wid: Id) -> Option<(u8, Vec2)> {
    use egui::{pos2, CursorIcon, Rect, Sense};
    const BAND: f32 = 8.0;
    const TITLE_CLEAR: f32 = 36.0; // keep side bands clear of the title-bar controls
    let r = ui.max_rect();
    if r.width() < 3.0 * BAND || r.height() < TITLE_CLEAR + 2.0 * BAND {
        return None; // too small to place non-overlapping bands
    }
    let bl = Rect::from_min_max(pos2(r.min.x, r.max.y - BAND), pos2(r.min.x + BAND, r.max.y));
    let br = Rect::from_min_max(pos2(r.max.x - BAND, r.max.y - BAND), pos2(r.max.x, r.max.y));
    let left = Rect::from_min_max(
        pos2(r.min.x, r.min.y + TITLE_CLEAR),
        pos2(r.min.x + BAND, r.max.y - BAND),
    );
    let right = Rect::from_min_max(
        pos2(r.max.x - BAND, r.min.y + TITLE_CLEAR),
        pos2(r.max.x, r.max.y - BAND),
    );
    let bottom =
        Rect::from_min_max(pos2(r.min.x + BAND, r.max.y - BAND), pos2(r.max.x - BAND, r.max.y));
    // Corners FIRST so a corner press is not stolen by an edge band.
    for (mask, rect) in [(1u8 | 8, bl), (2u8 | 8, br), (1u8, left), (2u8, right), (8u8, bottom)] {
        let resp = ui.interact(rect, wid.with(("winresize", mask)), Sense::drag());
        if resp.hovered() || resp.dragged() {
            let cursor = match mask {
                m if m == 1 | 8 => CursorIcon::ResizeNeSw, // bottom-left
                m if m == 2 | 8 => CursorIcon::ResizeNwSe, // bottom-right
                m if m & (1 | 2) != 0 => CursorIcon::ResizeHorizontal,
                _ => CursorIcon::ResizeVertical,
            };
            ui.ctx().set_cursor_icon(cursor);
        }
        if resp.dragged() {
            return Some((mask, resp.drag_delta()));
        }
    }
    None
}

#[cfg(test)]
mod orderflow_on_tests {
    use super::*;
    use egui::{pos2, vec2, Rect};

    fn win(interval: &str) -> WinState {
        let r = Rect::from_min_size(pos2(0.0, 0.0), vec2(100.0, 100.0));
        WinState::new("t", "BTCUSDT", interval, WinKind::Chart, r)
    }

    #[test]
    fn fresh_window_is_off() {
        // DEFAULT: a fresh chart has every orderflow input off → zero change (Task 7's
        // default-off invariant, mirrored from vike-chart's `ChartInputs` fields).
        assert!(!win("1m").orderflow_on());
    }

    #[test]
    fn either_toggle_turns_it_on_regardless_of_interval() {
        // The literal formula (task-7-brief step 3, restated with explicit parens by the
        // orchestrator) does NOT Kline-gate `cvd_on`/`profile_on` — only the Footprint-style
        // arm is Kline-gated. Pin that asymmetry down explicitly, incl. on a tick interval.
        let mut w = win("1m");
        w.cvd_on = true;
        assert!(w.orderflow_on());

        let mut w = win("100t");
        w.profile_on = true;
        assert!(w.orderflow_on(), "toggles are on regardless of Kline vs tick/volume interval");
    }

    #[test]
    fn footprint_style_needs_a_kline_interval() {
        let mut w = win("1m");
        w.style = vike_chart::chart::ChartStyle::Footprint;
        assert!(w.orderflow_on(), "Footprint style on a Kline interval turns orderflow on");

        let mut w = win("100t"); // tick interval — out of scope for the Footprint STYLE
        w.style = vike_chart::chart::ChartStyle::Footprint;
        assert!(!w.orderflow_on(), "Footprint style on a non-Kline interval must NOT turn it on");

        let mut w = win("10v"); // volume interval — same restriction
        w.style = vike_chart::chart::ChartStyle::Footprint;
        assert!(!w.orderflow_on());
    }

    #[test]
    fn candles_style_on_a_kline_interval_stays_off() {
        assert!(!win("1m").orderflow_on()); // style defaults to Candles in `WinState::new`
    }
}

#[cfg(test)]
mod venue_key_tests {
    use super::*;
    use egui::{pos2, vec2, Rect};

    fn win(symbol: &str) -> WinState {
        let r = Rect::from_min_size(pos2(0.0, 0.0), vec2(100.0, 100.0));
        WinState::new("t", symbol, "1m", WinKind::Chart, r)
    }

    #[test]
    fn binance_key_and_title_are_byte_identical_to_the_historical_single_venue_form() {
        let w = win("BTCUSDT"); // fresh window defaults to Binance
        assert_eq!(w.venue, DEFAULT_VENUE);
        assert_eq!(w.key(), "BTCUSDT@1m", "Binance key must stay the historical bare form");
        assert_eq!(w.title, "BTCUSDT · 1m", "Binance title unchanged");
    }

    #[test]
    fn non_binance_key_is_namespaced_and_title_is_venue_prefixed() {
        let mut w = win("BTCUSDT");
        w.venue = "bybit".into();
        w.retitle();
        assert_eq!(w.key(), "bybit:BTCUSDT@1m", "non-Binance keys are venue-namespaced");
        assert_eq!(w.title, "BYBIT BTCUSDT · 1m");

        // A shared symbol on two venues yields DISTINCT keys — no `charts`/`subs` collision.
        assert_ne!(w.key(), win("BTCUSDT").key());
    }

    #[test]
    fn series_key_matches_win_key_for_both_venues() {
        assert_eq!(series_key("binance", "BTCUSDT", "1m"), "BTCUSDT@1m");
        assert_eq!(series_key("okx", "BTC-USDT", "1m"), "okx:BTC-USDT@1m");
    }
}

/// `WinState.asset_class` (feed-routing slice 1). `WinState` itself is never the serde type —
/// window persistence goes through `super::persist::WinSnap` (see that module's `asset_class`
/// field + its own backward-compat test), so the "old JSON still loads" guarantee is proven
/// there. What's proven here is the runtime default this field must have so every existing
/// window-construction path (`new`/`tool`, quick-picks, plain-symbol paths) is unaffected: a
/// fresh `WinState` is `None` (spot/legacy), and `Option<vike_catalog::AssetClass>` itself
/// round-trips through JSON losslessly (the property `WinSnap::asset_class` relies on).
#[cfg(test)]
mod asset_class_tests {
    use super::*;
    use egui::{pos2, vec2, Rect};
    use vike_catalog::AssetClass;

    #[test]
    fn fresh_window_defaults_to_none() {
        let r = Rect::from_min_size(pos2(0.0, 0.0), vec2(100.0, 100.0));
        let w = WinState::new("t", "BTCUSDT", "1m", WinKind::Chart, r);
        assert_eq!(w.asset_class, None);
    }

    #[test]
    fn option_asset_class_json_round_trip_preserves_some_and_defaults_none() {
        // Mirrors the exact serde shape `WinSnap::asset_class` uses
        // (`#[serde(default, skip_serializing_if = "Option::is_none")]`): a JSON object with
        // the key absent (old-format file) must deserialize to `None`, and `Some(..)` must
        // round-trip losslessly.
        #[derive(serde::Serialize, serde::Deserialize, PartialEq, Debug)]
        struct Wrapper {
            #[serde(default, skip_serializing_if = "Option::is_none")]
            asset_class: Option<AssetClass>,
        }

        // Old JSON with the key entirely absent still deserializes, defaulting to None.
        let old: Wrapper = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(old.asset_class, None);

        // A `Some(CryptoPerp)` value survives a full serde round-trip.
        let w = Wrapper { asset_class: Some(AssetClass::CryptoPerp) };
        let json = serde_json::to_string(&w).unwrap();
        let back: Wrapper = serde_json::from_str(&json).unwrap();
        assert_eq!(back, w);
    }
}

#[cfg(test)]
impl Default for WinState {
    /// Test-only baseline window (C1 Task 2's `pane_model_tests` needs a `WinState`
    /// with no real geometry/symbol) — delegates to `new` so every field's actual
    /// default-construction logic (`FollowLive::default()`, `ChartOptions::default()`,
    /// …) stays in the ONE place that already owns it.
    fn default() -> Self {
        let r = Rect::from_min_size(Pos2::ZERO, Vec2::splat(100.0));
        Self::new("default", "", "", WinKind::Chart, r)
    }
}

#[cfg(test)]
impl WinState {
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

#[cfg(test)]
mod pane_model_tests {
    use super::*;
    use vike_chart::chart::{MoveTarget, PaneKey};

    // helper: a WinState with N oscillator studies added in order, returning their uids
    fn win_with_oscillators(n: usize) -> (WinState, Vec<u64>) {
        let mut w = WinState::default();
        let mut uids = Vec::new();
        for i in 0..n {
            // add a known oscillator-kind indicator (RSI is Oscillator); use the same path add_indicator uses
            let uid = w.add_test_oscillator(&format!("rsi_{i}"));
            uids.push(uid);
        }
        (w, uids)
    }

    #[test]
    fn each_oscillator_gets_its_own_pane_in_add_order() {
        let (w, uids) = win_with_oscillators(3);
        assert_eq!(w.present_study_panes().len(), 3);
        // each uid maps to a distinct pane, ordered as added
        let keys: Vec<PaneKey> = uids.iter().map(|u| w.study_pane[u]).collect();
        assert_eq!(keys, w.present_study_panes());
        assert!(keys.iter().all(|k| matches!(k, PaneKey::Study(_))));
    }

    #[test]
    fn add_indicator_to_auto_matches_add_indicator() {
        use vike_chart::chart::PaneTarget;
        // Part (a) byte-identical guarantee: adding with `Auto` produces the exact
        // same pane arrangement as the plain `add_indicator` path.
        let mut a = WinState::default();
        a.add_indicator("rsi", &[]);
        a.add_indicator("macd", &[]);
        let mut b = WinState::default();
        b.add_indicator_to("rsi", &[], PaneTarget::Auto);
        b.add_indicator_to("macd", &[], PaneTarget::Auto);
        assert_eq!(a.present_study_panes(), b.present_study_panes());
        assert_eq!(a.study_pane.len(), b.study_pane.len());
        assert_eq!(a.indicators.len(), b.indicators.len());
    }

    #[test]
    fn add_indicator_to_existing_merges_into_that_pane() {
        use vike_chart::chart::{PaneKey, PaneTarget};
        let mut w = WinState::default();
        w.add_indicator("rsi", &[]);
        let first_uid = w.indicators[0].uid;
        let target = w.study_pane[&first_uid];
        // Add a SECOND oscillator straight into the first one's pane.
        w.add_indicator_to("macd", &[], PaneTarget::Existing(target));
        let second_uid = w.indicators[1].uid;
        assert_eq!(w.study_pane[&second_uid], target, "second study merged into the target pane");
        assert_eq!(
            w.present_study_panes(),
            vec![target],
            "still one pane (the fresh one was dropped)"
        );
        assert!(matches!(target, PaneKey::Study(_)));
    }

    #[test]
    fn add_indicator_to_overlay_is_never_placed_in_a_study_pane() {
        use vike_chart::chart::{PaneKey, PaneTarget};
        // An OVERLAY (SMA) is restricted to the price pane: even asking for a study
        // pane leaves it off `study_pane`/`pane_order` entirely.
        let mut w = WinState::default();
        w.add_indicator("rsi", &[]); // one real study pane exists
        let osc_pane = w.study_pane[&w.indicators[0].uid];
        w.add_indicator_to("sma", &[], PaneTarget::Existing(osc_pane));
        let sma_uid = w.indicators[1].uid;
        assert!(w.indicators[1].is_overlay(), "sma must be overlay-kind (registry drift?)");
        assert!(!w.study_pane.contains_key(&sma_uid), "overlay never enters study_pane");
        assert_eq!(w.present_study_panes(), vec![osc_pane], "overlay add didn't touch panes");
        assert!(matches!(osc_pane, PaneKey::Study(_)));
    }

    #[test]
    fn move_into_merges_two_studies_into_one_pane() {
        let (mut w, uids) = win_with_oscillators(2);
        let target = w.study_pane[&uids[0]];
        w.move_study(uids[1], MoveTarget::Into(target));
        assert_eq!(w.study_pane[&uids[0]], target);
        assert_eq!(w.study_pane[&uids[1]], target);
        assert_eq!(w.present_study_panes(), vec![target]); // one pane now, the emptied one dropped
    }

    #[test]
    fn move_into_a_stale_pane_key_is_ignored_not_orphaning() {
        // Review Important: Into() must not point a study at a dropped/foreign pane.
        let (mut w, uids) = win_with_oscillators(2);
        let p0 = w.study_pane[&uids[0]];
        let p1_stale = w.study_pane[&uids[1]];
        w.move_study(uids[1], MoveTarget::Into(p0)); // merges; p1_stale is now dropped
        assert!(!w.present_study_panes().contains(&p1_stale));
        // moving uid[0] into the now-stale key must be a no-op, NOT orphan uid[0]
        w.move_study(uids[0], MoveTarget::Into(p1_stale));
        assert_eq!(w.study_pane[&uids[0]], p0); // still in a LIVE pane
                                                // every tracked study still resolves to a present pane (nothing rendered in zero panes)
        let present = w.present_study_panes();
        assert!(w.study_pane.values().all(|k| present.contains(k)));
    }

    #[test]
    fn move_new_below_creates_and_orders_a_fresh_pane() {
        let (mut w, uids) = win_with_oscillators(2);
        let anchor = w.study_pane[&uids[0]];
        let before = w.present_study_panes();
        w.move_study(uids[1], MoveTarget::NewBelow(anchor));
        let after = w.present_study_panes();
        assert_eq!(after.len(), 2);
        assert_eq!(after[0], anchor); // anchor stays first
        assert_ne!(after[1], before[1]); // uid[1]'s pane is a NEW key, below anchor
        assert_eq!(w.study_pane[&uids[1]], after[1]);
    }

    #[test]
    fn reorder_pane_swaps_adjacent_and_clamps_at_ends() {
        // Feature #1 (TradingView parity): the pane ↑/↓ controls swap a study
        // pane with its neighbor, and no-op at the group's ends / for unknowns.
        let (mut w, _uids) = win_with_oscillators(3);
        w.options.show_volume = false; // isolate the study group for this case
        let p = w.present_study_panes(); // [p0, p1, p2] in add-order
        assert_eq!(p.len(), 3);
        // middle pane UP → swaps with the first
        w.reorder_pane(p[1], true);
        assert_eq!(w.present_study_panes(), vec![p[1], p[0], p[2]]);
        // and back DOWN → original order
        w.reorder_pane(p[1], false);
        assert_eq!(w.present_study_panes(), vec![p[0], p[1], p[2]]);
        // UP at the top is a no-op
        w.reorder_pane(p[0], true);
        assert_eq!(w.present_study_panes(), vec![p[0], p[1], p[2]]);
        // DOWN at the bottom is a no-op
        w.reorder_pane(p[2], false);
        assert_eq!(w.present_study_panes(), vec![p[0], p[1], p[2]]);
        // an unknown pane is a no-op (never panics / reorders)
        w.reorder_pane(PaneKey::Study(9999), true);
        assert_eq!(w.present_study_panes(), vec![p[0], p[1], p[2]]);
    }

    #[test]
    fn reorder_pane_treats_volume_cvd_and_studies_as_peers() {
        // Chart single-max default: Volume and CVD are reorderable peers of study
        // panes. Turn both on, sync them into the authored order (Volume front,
        // CVD after), then reorder across the whole unified group.
        let (mut w, _uids) = win_with_oscillators(1);
        w.options.show_volume = true;
        w.cvd_on = true;
        w.sync_sub_panes();
        let study = w.present_study_panes()[0];
        // Default authored order: Volume, Cvd, then the study pane.
        assert_eq!(w.present_sub_panes(), vec![PaneKey::Volume, PaneKey::Cvd, study]);
        // Move the study pane UP twice → it climbs above CVD, then above Volume.
        w.reorder_pane(study, true);
        assert_eq!(w.present_sub_panes(), vec![PaneKey::Volume, study, PaneKey::Cvd]);
        w.reorder_pane(study, true);
        assert_eq!(w.present_sub_panes(), vec![study, PaneKey::Volume, PaneKey::Cvd]);
        // UP again at the top is a no-op.
        w.reorder_pane(study, true);
        assert_eq!(w.present_sub_panes(), vec![study, PaneKey::Volume, PaneKey::Cvd]);
        // Move Volume DOWN → swaps with CVD (the bottom-most now).
        w.reorder_pane(PaneKey::Volume, false);
        assert_eq!(w.present_sub_panes(), vec![study, PaneKey::Cvd, PaneKey::Volume]);
        // DOWN at the bottom (Volume) is a no-op.
        w.reorder_pane(PaneKey::Volume, false);
        assert_eq!(w.present_sub_panes(), vec![study, PaneKey::Cvd, PaneKey::Volume]);
    }

    #[test]
    fn sync_sub_panes_adds_and_removes_volume_cvd() {
        // Toggling show_volume / cvd_on reconciles their membership in the
        // authored order (added at the default slot, removed cleanly).
        let mut w = WinState::default();
        w.options.show_volume = false;
        w.cvd_on = false;
        w.sync_sub_panes();
        assert!(w.present_sub_panes().is_empty());
        // Volume on → front.
        w.options.show_volume = true;
        w.sync_sub_panes();
        assert_eq!(w.present_sub_panes(), vec![PaneKey::Volume]);
        // CVD on → directly after Volume.
        w.cvd_on = true;
        w.sync_sub_panes();
        assert_eq!(w.present_sub_panes(), vec![PaneKey::Volume, PaneKey::Cvd]);
        // Volume off → removed, CVD stays.
        w.options.show_volume = false;
        w.sync_sub_panes();
        assert_eq!(w.present_sub_panes(), vec![PaneKey::Cvd]);
        // idempotent — a second sync with no toggle change is a no-op.
        w.sync_sub_panes();
        assert_eq!(w.present_sub_panes(), vec![PaneKey::Cvd]);
    }

    #[test]
    fn removing_the_last_study_in_a_pane_drops_the_pane() {
        let (mut w, uids) = win_with_oscillators(2);
        w.remove_indicator(uids[1]);
        assert_eq!(w.present_study_panes().len(), 1);
        assert!(!w.study_pane.contains_key(&uids[1]));
    }
}

#[cfg(test)]
mod compare_tests {
    use super::*;
    use egui::{pos2, vec2, Rect};

    fn chart_win(symbol: &str) -> WinState {
        let r = Rect::from_min_size(pos2(0.0, 0.0), vec2(100.0, 100.0));
        WinState::new("t", symbol, "1m", WinKind::Chart, r)
    }

    #[test]
    fn compare_add_dedups_and_orders() {
        let mut w = WinState::default();
        assert!(w.compare.is_empty(), "a fresh window has no compare overlays");
        w.add_compare("ETHUSDT");
        w.add_compare("SOLUSDT");
        w.add_compare("ETHUSDT"); // duplicate → no-op
        assert_eq!(w.compare, vec!["ETHUSDT".to_string(), "SOLUSDT".to_string()]);
        w.remove_compare("ETHUSDT");
        assert_eq!(w.compare, vec!["SOLUSDT".to_string()]);
    }

    #[test]
    fn add_compare_ignores_the_windows_own_symbol() {
        let mut w = chart_win("BTCUSDT");
        w.add_compare("BTCUSDT"); // exact self
        w.add_compare("btcusdt"); // case-insensitive self
        assert!(w.compare.is_empty(), "a chart must never overlay its own symbol");
        w.add_compare("ETHUSDT");
        assert_eq!(w.compare, vec!["ETHUSDT".to_string()]);
    }

    #[test]
    fn first_compare_forces_percent_scale_second_does_not_reflip() {
        // Task 3 gates overlay render on Percent mode, so the FIRST overlay must
        // auto-switch a Linear window to Percent (otherwise the overlay is invisible).
        let mut w = chart_win("BTCUSDT");
        assert_eq!(w.scale, vike_chart::ScaleMode::Linear);
        w.add_compare("ETHUSDT");
        assert_eq!(
            w.scale,
            vike_chart::ScaleMode::Percent,
            "first overlay auto-switches to Percent"
        );
        // A subsequent add must NOT re-touch the scale — the user may switch back manually.
        w.scale = vike_chart::ScaleMode::Log;
        w.add_compare("SOLUSDT");
        assert_eq!(
            w.scale,
            vike_chart::ScaleMode::Log,
            "second overlay must not re-flip the scale"
        );
    }

    #[test]
    fn ignored_self_add_does_not_count_as_first_overlay() {
        // An add that's ignored (own symbol) must NOT trigger the first-overlay
        // auto-Percent flip — the overlay list is still empty afterward.
        let mut w = chart_win("BTCUSDT");
        w.add_compare("BTCUSDT");
        assert_eq!(w.scale, vike_chart::ScaleMode::Linear, "ignored add must not flip scale");
        assert!(w.compare.is_empty());
    }

    /// C2 tidy FIX 1: switching a window's PRIMARY symbol onto one that is already a Compare
    /// overlay must drop the now-redundant chip. This is the `WinState`-level primitive the
    /// fix relies on (`main.rs`'s title-bar `new_symbol` handler calls `remove_compare` right
    /// before reassigning `w.symbol` — that call site is UI-wired egui code, not unit-testable
    /// without a full `App`/`eframe::CreationContext`, so this test exercises the same sequence
    /// directly against `WinState`, standing in for the handler).
    #[test]
    fn switching_primary_to_an_already_compared_symbol_drops_the_stale_chip() {
        let mut w = chart_win("BTCUSDT");
        w.add_compare("ETHUSDT");
        w.add_compare("SOLUSDT");
        assert_eq!(w.compare, vec!["ETHUSDT".to_string(), "SOLUSDT".to_string()]);

        // Mirror main.rs's `new_symbol` handler: remove_compare(&new_symbol) BEFORE reassigning
        // `w.symbol`, so the (now-redundant) chip is dropped instead of lingering.
        let new_symbol = "ETHUSDT".to_string();
        w.remove_compare(&new_symbol);
        w.symbol = new_symbol;

        assert_eq!(w.compare, vec!["SOLUSDT".to_string()], "the matching chip must be gone");
        assert!(!w.compare.iter().any(|s| s == &w.symbol), "no chip may match the new primary");
    }
}

#[cfg(test)]
mod series_pane_model_tests {
    use super::*;
    use vike_chart::chart::{MoveTarget, PaneKey};

    #[test]
    fn compare_symbol_defaults_to_overlay() {
        // DEFAULT: a compare symbol lives in the price-pane %-overlay, NOT its
        // own pane — unlike C1 oscillators (which assign-on-add via
        // `assign_default_pane`), there is NO assign-on-add here; see
        // `move_series_to_new_pane`'s doc.
        let mut w = WinState::default();
        w.add_compare("ETHUSDT");
        assert!(!w.series_pane.contains_key("ETHUSDT"));
        assert!(w.present_series_panes().is_empty());
    }

    #[test]
    fn remove_compare_cascades_into_series_pane() {
        use vike_chart::chart::ScaleAssign;
        // Review Important: removing an own-paned compare symbol must not leave an
        // orphaned series_pane entry / empty pane (mirrors remove_indicator).
        let mut w = WinState::default();
        w.add_compare("ETHUSDT");
        w.move_series_to_new_pane("ETHUSDT");
        w.series_scale.insert("ETHUSDT".to_string(), ScaleAssign::Right); // Task 7 review Minor-1
        assert_eq!(w.present_series_panes().len(), 1);
        w.remove_compare("ETHUSDT");
        assert!(!w.series_pane.contains_key("ETHUSDT"));
        assert!(w.present_series_panes().is_empty());
        // Task 7 review Minor-1: the scale pin is cleared too, so re-adding ETHUSDT
        // defaults back to Percent (no resurrected Right pin).
        assert_eq!(w.series_scale_of("ETHUSDT"), ScaleAssign::Percent);
    }

    #[test]
    fn move_series_to_new_pane_gives_it_its_own_pane() {
        let mut w = WinState::default();
        w.add_compare("ETHUSDT");
        w.move_series_to_new_pane("ETHUSDT");
        assert_eq!(w.present_series_panes().len(), 1);
        let pane = w.series_pane["ETHUSDT"];
        assert!(matches!(pane, PaneKey::Series(_)));
        assert_eq!(w.present_series_panes(), vec![pane]);

        // Calling it again while already in its own pane is a no-op — same
        // pane, no second allocation.
        w.move_series_to_new_pane("ETHUSDT");
        assert_eq!(w.series_pane["ETHUSDT"], pane);
        assert_eq!(w.present_series_panes(), vec![pane]);
    }

    #[test]
    fn overlay_series_returns_it_to_the_price_pane_and_drops_the_pane() {
        let mut w = WinState::default();
        w.add_compare("ETHUSDT");
        w.move_series_to_new_pane("ETHUSDT");
        assert_eq!(w.present_series_panes().len(), 1);

        w.overlay_series("ETHUSDT");
        assert!(!w.series_pane.contains_key("ETHUSDT"));
        assert!(w.present_series_panes().is_empty(), "the vacated pane must be dropped");
    }

    #[test]
    fn move_into_a_stale_pane_key_is_ignored_not_orphaning() {
        // Review Important (mirrors C1's identically-purposed test): `Into()`
        // must not point a series at a dropped/foreign pane.
        let mut w = WinState::default();
        w.add_compare("ETHUSDT");
        w.add_compare("SOLUSDT");
        w.move_series_to_new_pane("ETHUSDT");
        w.move_series_to_new_pane("SOLUSDT");
        let p0 = w.series_pane["ETHUSDT"];
        let p1_stale = w.series_pane["SOLUSDT"];
        w.move_series("SOLUSDT", MoveTarget::Into(p0)); // merges; p1_stale is now dropped
        assert!(!w.present_series_panes().contains(&p1_stale));
        // moving ETHUSDT into the now-stale key must be a no-op, NOT orphan it
        w.move_series("ETHUSDT", MoveTarget::Into(p1_stale));
        assert_eq!(w.series_pane["ETHUSDT"], p0); // still in a LIVE pane
        let present = w.present_series_panes();
        assert!(w.series_pane.values().all(|k| present.contains(k)));
    }

    #[test]
    fn move_series_on_a_still_overlaid_symbol_is_a_no_op() {
        // `move_series` (reorder/merge) only operates on a symbol ALREADY in
        // its own pane — an overlaid (or unknown) symbol must not be silently
        // pulled into a pane via the reorder path (use
        // `move_series_to_new_pane` for that transition).
        let mut w = WinState::default();
        w.add_compare("ETHUSDT");
        w.move_series_to_new_pane("ETHUSDT");
        let anchor = w.series_pane["ETHUSDT"];

        w.add_compare("SOLUSDT"); // stays overlaid — never entered series_pane
        w.move_series("SOLUSDT", MoveTarget::NewBelow(anchor));
        assert!(!w.series_pane.contains_key("SOLUSDT"), "still overlaid, not pulled in");
        assert_eq!(w.present_series_panes(), vec![anchor]);
    }

    #[test]
    fn move_new_below_creates_and_orders_a_fresh_pane() {
        let mut w = WinState::default();
        w.add_compare("ETHUSDT");
        w.add_compare("SOLUSDT");
        w.move_series_to_new_pane("ETHUSDT");
        w.move_series_to_new_pane("SOLUSDT");
        let anchor = w.series_pane["ETHUSDT"];
        let before = w.present_series_panes();
        w.move_series("SOLUSDT", MoveTarget::NewBelow(anchor));
        let after = w.present_series_panes();
        assert_eq!(after.len(), 2);
        assert_eq!(after[0], anchor); // anchor stays first
        assert_ne!(after[1], before[1]); // SOLUSDT's pane is a NEW key, below anchor
        assert_eq!(w.series_pane["SOLUSDT"], after[1]);
    }

    #[test]
    fn moving_the_last_symbol_out_of_a_pane_drops_the_pane() {
        let mut w = WinState::default();
        w.add_compare("ETHUSDT");
        w.add_compare("SOLUSDT");
        w.move_series_to_new_pane("ETHUSDT");
        w.move_series_to_new_pane("SOLUSDT");
        let p0 = w.series_pane["ETHUSDT"];
        w.move_series("SOLUSDT", MoveTarget::Into(p0)); // merge into one pane
        assert_eq!(w.present_series_panes(), vec![p0]);

        w.overlay_series("ETHUSDT"); // one member leaves, one remains
        assert_eq!(
            w.present_series_panes(),
            vec![p0],
            "pane survives while SOLUSDT is still in it"
        );

        w.overlay_series("SOLUSDT"); // last member leaves
        assert!(w.present_series_panes().is_empty(), "pane dropped once empty");
    }

    // C2b Task 7: a fresh window pins nothing to a secondary axis — every symbol
    // resolves to the `Percent` default (the shared-% overlay behavior), which is
    // what makes the default render byte-identical to C2a.
    #[test]
    fn fresh_window_series_scale_defaults_to_percent() {
        use vike_chart::chart::ScaleAssign;
        let w = WinState::default();
        assert!(w.series_scale.is_empty(), "no pins on a fresh window");
        // An unpinned (indeed unknown) symbol reads back the Percent default.
        assert_eq!(w.series_scale_of("ETHUSDT"), ScaleAssign::Percent);

        // And once pinned, the read seam reflects it (the menu wiring is Task 9).
        let mut w = w;
        w.series_scale.insert("ETHUSDT".to_string(), ScaleAssign::Right);
        assert_eq!(w.series_scale_of("ETHUSDT"), ScaleAssign::Right);
        assert_eq!(w.series_scale_of("SOLUSDT"), ScaleAssign::Percent, "others stay default");
    }
}

/// [`WinKind::from_slug`] exhaustiveness: every kind round-trips through its slug, slugs are
/// distinct, unknowns parse to `None`, and the legacy `VIKE_TOOL` vocabulary (the silent
/// `_ => Calendar` match in `vike-app`'s `main.rs` this API replaced) still resolves identically.
#[cfg(test)]
mod winkind_slug_tests {
    use super::WinKind;

    #[test]
    fn every_kind_round_trips_through_its_slug() {
        for k in WinKind::ALL {
            assert_eq!(WinKind::from_slug(k.slug()), Some(k), "slug {:?}", k.slug());
        }
        // Distinct slugs — `from_slug`'s first-match resolution must be unambiguous.
        let mut slugs: Vec<&str> = WinKind::ALL.iter().map(|k| k.slug()).collect();
        slugs.sort_unstable();
        slugs.dedup();
        assert_eq!(slugs.len(), WinKind::ALL.len(), "duplicate slug in WinKind::ALL");
    }

    #[test]
    fn unknown_and_miscased_slugs_parse_to_none() {
        // Case-sensitive, like the `VIKE_TOOL` match this replaced — the caller owns the fallback.
        assert_eq!(WinKind::from_slug(""), None);
        assert_eq!(WinKind::from_slug("News"), None);
        assert_eq!(WinKind::from_slug("data manager"), None); // the LABEL is not the slug
        assert_eq!(WinKind::from_slug("nope"), None);
    }

    /// The legacy `VIKE_TOOL=<slug>` grid: every value the replaced match named still resolves to
    /// the same kind, with `.unwrap_or(Calendar)` supplying the old `_ =>` fallback for junk.
    #[test]
    fn legacy_vike_tool_vocabulary_is_unchanged() {
        for (s, want) in [
            ("trade", WinKind::Trade),
            ("dom", WinKind::Dom),
            ("options", WinKind::Options),
            ("greeks", WinKind::Greeks),
            ("tearsheet", WinKind::Tearsheet),
            ("news", WinKind::News),
            ("data", WinKind::Data),
            ("polymarket", WinKind::Polymarket),
            ("calendar", WinKind::Calendar),
        ] {
            assert_eq!(WinKind::from_slug(s).unwrap_or(WinKind::Calendar), want, "slug {s:?}");
        }
        assert_eq!(WinKind::from_slug("junk").unwrap_or(WinKind::Calendar), WinKind::Calendar);
    }
}
