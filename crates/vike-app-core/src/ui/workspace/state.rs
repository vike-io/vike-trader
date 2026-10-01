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

use egui::{Id, LayerId, Order, Pos2, Rect, Vec2};
use vike_orderflow::tickvol::BarKind;
use vike_ui_theme::icons::{self, Icon};

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
    /// The Settings window: the app's own preferences — today one section, Appearance (design
    /// system spec §5).
    Settings,
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
            WinKind::Settings => "Settings",
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
            WinKind::Settings => "settings",
        }
    }
    /// Whether this kind's body is TABBED and seats its segmented control inside the title bar
    /// rather than in a row under it (`super::title_bar::TitleTabSlot`).
    ///
    /// ⚠ Two consequences, and the second is the one that surprises: the bar RESERVES a rect
    /// between the title and the window controls for a kind that answers `true`, and below
    /// [`super::title_bar::TITLE_DROP_W`] that kind's TITLE DROPS so two segments still fit at
    /// ~400pt. A kind that answers `false` keeps its title at every width, because nothing is
    /// competing with it for the row.
    ///
    /// Exhaustive match rather than `matches!`, so a new variant has to decide.
    pub fn carries_title_tabs(self) -> bool {
        match self {
            WinKind::Connections => true,
            WinKind::Chart
            | WinKind::Trade
            | WinKind::Dom
            | WinKind::Options
            | WinKind::Greeks
            | WinKind::News
            | WinKind::Calendar
            | WinKind::Data
            | WinKind::Studio
            | WinKind::Tearsheet
            | WinKind::Polymarket
            | WinKind::Settings => false,
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
    /// The window's icon in its title bar — one registry icon per kind.
    pub fn icon(self) -> Icon {
        match self {
            WinKind::Chart => icons::CHART,
            WinKind::Trade => icons::TRADE,
            WinKind::Dom => icons::DOM,
            WinKind::Options => icons::OPTIONS,
            WinKind::Greeks => icons::GREEKS,
            WinKind::News => icons::NEWS,
            WinKind::Calendar => icons::CALENDAR,
            WinKind::Data => icons::DATA,
            WinKind::Studio => icons::STUDIO,
            WinKind::Connections => icons::CONNECTIONS,
            WinKind::Tearsheet => icons::TEARSHEET,
            WinKind::Polymarket => icons::POLYMARKET,
            WinKind::Settings => icons::SETTINGS,
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
            // No launcher icon to colour; a token rather than a new colour literal.
            WinKind::Settings => vike_ui_theme::palette::TEXT2,
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
    pub const ALL: [WinKind; 13] = [
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
        WinKind::Settings,
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
    pub asset_class: Option<vike_model::AssetClass>,
    pub kind: WinKind,
    pub open: bool,      // false => hidden off-desktop (rail can unhide)
    pub minimized: bool, // true => not drawn as a Window; shows in the left rail
    pub pos: Pos2,       // our tracked copy of the live geometry
    pub size: Vec2,
    pub pending: Option<Rect>, // one-frame forced geometry (arrange/maximize)
    pub pre_max: Option<Rect>, // rect to restore after un-maximize
    pub maximized: bool,
    pub force_frames: u8, // pin geometry for N frames after arrange/move, then egui owns it
    /// LATCH: the user has grabbed one of this window's own edge bands at least once, so the app
    /// — not egui — owns its geometry from here on. Set by [`show_window`] the first time
    /// [`window_resize_handles`] fires on a NON-`fills` kind (a `fills` kind is app-owned from
    /// frame one and must never take the latch — see that site's comment), and NEVER cleared for
    /// the life of the window.
    ///
    /// Before it latches a tool window is byte-identical to its pre-latch self: unpinned,
    /// `default_pos`/`default_size`, geometry read back from egui every settled frame, growing to
    /// fit whatever its body lays out. That matters because nine tool kinds open at 560×400 and
    /// several bodies are naturally wider — pinning from frame one would open them into
    /// scrollbars. After it latches the window is `current_pos` + `fixed_size`, its body bounds
    /// itself with a `ScrollArea`, and only the edge bands move it.
    ///
    /// ⚠ Latching also FLOORS `size` on BOTH axes (`MINW`×`MINH`), not merely on the axis the
    /// pressed band consumed, and that is what keeps this field's promise that a pinned window's
    /// `size` is AUTHORITATIVE: a side-band latch otherwise pinned the window at a stale painted
    /// height while egui painted it at its own 64pt scroll-area floor, and the next drag on that
    /// axis then failed to track the pointer. See that assignment's comment in [`show_window`].
    ///
    /// ⚠ It is NOT what keeps the body's id chain stable, and an earlier version of this doc
    /// claimed it was. For a kind that can take this latch at all, [`BodyBounds`] wraps the body
    /// in a `ScrollArea` UNCONDITIONALLY and
    /// toggles only that container's sizing, so the container is in the id chain from frame one
    /// whatever this flag says (a `fills` kind never latches and is never wrapped at all) — see
    /// [`BodyBounds::show`] for why inserting one on the latching
    /// frame instead would have discarded the Studio editor's undo stack, every inner scroll
    /// offset and every `CollapsingState` inside the body.
    ///
    /// PERSISTED, as `super::persist::WinSnap::user_sized`: `WinSnap::size` is saved, so without
    /// this flag a window the user deliberately dragged SMALLER reloaded at that size and then
    /// re-grew to its content, discarding the drag. ⚠ Only `WinKind::Chart` windows are captured
    /// today (`persist::capture` filters on it) and a chart is a `fills` kind that never takes the
    /// latch, so every key written today is `false`; the field carries the plumbing so the value
    /// survives the day tool windows join that capture.
    pub user_sized: bool,
    /// LATCH: this window's body asked for more room than the ARENA has, so the app took its
    /// geometry over and CAPPED it at the arena — same ownership `user_sized` confers, reached
    /// without anybody grabbing anything. Set by [`show_window`] the frame egui PAINTS a
    /// non-`fills` window larger than the `bounds` it was given, and never cleared (a window whose
    /// content later shrinks keeps the size it has, exactly as a user-dragged one does).
    ///
    /// ⚠ **This is the fix for "why can't I resize it at the bottom — the status bar is over the
    /// window".** Until it latches, a tool window grows to whatever its body lays out and NOTHING
    /// bounds that growth: `egui-0.36.1/src/containers/resize.rs`'s `Resize::end` takes
    /// `else { size[d] = state.last_content_size[d]; }` for a `Window`, so even a `fixed_size`
    /// window is PAINTED at its content. A body taller than the arena therefore paints a window
    /// whose bottom edge lands past `bounds` — and `egui-0.36.1/src/containers/area.rs` does
    /// `ui.set_clip_rect(self.constrain_rect)` while `Ui::interact` records
    /// `interact_rect: self.clip_rect().intersect(rect)`, which
    /// `egui-0.36.1/src/hit_test.rs` DROPS outright when it `is_negative()`. So
    /// [`window_resize_handles`]' bottom band and both bottom corners are allocated, are returned
    /// by `read_response`, paint a resize cursor on nothing, and can never be hit. The same
    /// mechanism takes BOTH side bands off an over-WIDE window, because
    /// `Context::constrain_window_rect_to_area` pins an oversized window's left/top edge at the
    /// arena's and lets the far edge overhang.
    ///
    /// It cannot be `user_sized` itself: that flag means "the user grabbed a band", it is
    /// PERSISTED (`super::persist::WinSnap::user_sized`) and it is what a reloaded layout reads
    /// to know a size was chosen deliberately. This one is a per-session consequence of the
    /// CURRENT arena and re-derives itself within one frame, so it is deliberately not persisted.
    pub arena_bounded: bool,
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
    /// `vike_orderflow::bar_agg::OrderflowAgg::new`); `Some(x)` pins it. Set from the title-bar Orderflow
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
    /// **THE OPERATOR CHOSE THIS WINDOW'S SERIES**, so
    /// [`series_follow::follow_backend`](crate::ui::series_follow::follow_backend) may not retarget it.
    ///
    /// `false` from [`WinState::new`] — the startup default chart
    /// ([`startup::plan`](crate::ui::startup::plan)'s `BTCUSDT` `1m` on [`DEFAULT_VENUE`]) and a fresh
    /// `New chart window` both name a series NOBODY asked for, and on a thin client pointed at a
    /// node mounting some other venue that series can never paint anything. `true` from
    /// [`super::persist::apply`] (a saved layout IS a choice) and from the symbol/interval picker's
    /// apply in `crates/vike-desktop/src/app_ui.rs`'s `draw_windows`.
    ///
    /// ⚠ **Deliberately NOT persisted** — there is no [`super::persist::WinSnap`] key for it, and
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
    /// `false` from [`WinState::new`]; `true` from the interval picker's apply in
    /// `crates/vike-desktop/src/app_ui.rs`'s `draw_windows` and from [`super::persist::apply`] (a
    /// saved layout chose both halves). NOT persisted, for the same reason its sibling is not.
    pub interval_pinned: bool,
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

/// The FLOOR a band drag may drive a window's size to, on each axis.
///
/// At module scope rather than inside [`show_window`]'s resize block — which is where both
/// constants lived, and where every comment in this file still cites them from — because the ARENA
/// CEILING at the top of that same function has to spell the same pair: a clamp that can drive a
/// window BELOW the floor its own drags are floored at would fight the band arithmetic every
/// frame, on a box whose arena is smaller than one of these.
const MINW: f32 = 240.0;
const MINH: f32 = 160.0;

/// Slack the ARENA CEILING allows before it calls a painted window OVERSIZED.
///
/// Not tuning: `egui-0.36.1/src/containers/area.rs`'s `round_area_position` rounds every area
/// position to physical pixels and then to ui points, and `Resize::begin` `round_ui()`s its
/// desired size — so a window deliberately laid out AT the arena (a tile-vertical arrange, a
/// restore from maximize) can come back a sub-point over it. Without this, such a window would
/// latch [`WinState::arena_bounded`] and start scrolling a body that fits.
const CEILING_SLACK: f32 = 0.5;

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
            user_sized: false,             // latches on the first edge-band drag; see the field doc
            arena_bounded: false,          // latches the frame a body overflows the arena; ditto
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
            // Nobody chose this series yet — see the field doc. `persist::apply` and the symbol
            // picker's apply are the two sites that set it.
            series_pinned: false,
            // …and nobody chose this RESOLUTION yet. The interval picker's apply and
            // `persist::apply` are this one's two sites.
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
        if let Some(spec) = vike_chart::indicators::get_any(name)
            && let Some(mt) = vike_chart::chart::resolve_add_target(spec.kind, target)
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
    /// this once per frame BEFORE reading [`Self::present_sub_panes`] so a freshly
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
    /// controls. Adjacency is resolved over [`Self::present_sub_panes`] (so a
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
    /// an `vike_orderflow::bar_agg::OrderflowAgg` — never on a fresh, all-default window.
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

/// The inset a TOOL window's body is drawn at, below its flush custom title bar. Lives here, not
/// at the vike-desktop call site that applies it, so the headless harness in this file models the
/// SHIPPED inset rather than a second copy of it — the margin is geometry the resize bands depend
/// on, and a harness with no margin is the one configuration
/// [`window_resize_handles`] says must never ship.
///
/// ⚠ The SIDE inset (8) equals `window_resize_handles`' `BAND`, and that is load-bearing rather
/// than cosmetic: it puts a floating vertical scrollbar's 10pt interact strip
/// (`egui-0.36.1/src/style.rs`, `ScrollStyle::floating()` — `bar_width: 10.0`,
/// `floating_allocated_width: 0.0`, so the strip is drawn over content and reserves no space)
/// entirely INBOARD of the band, adjacent and disjoint.
///
/// ⚠ The BOTTOM inset is 6, NOT 8, so a floating HORIZONTAL scrollbar's strip and the 8pt bottom
/// band overlap by exactly 2pt. That is ACCEPTED, and the direction matters: the bands are
/// allocated LAST, and `egui-0.36.1/src/hit_test.rs`'s `find_closest_within` breaks a distance tie
/// by taking "the last one = the one on top", so the BAND wins those 2 points and the horizontal
/// bar's grab area is 2pt thinner. The resize edge is never captured — which is the opposite of
/// what an earlier version of this note claimed. Gated by
/// `the_side_bands_clear_the_scrollbars_and_the_bottom_band_overlaps_by_the_measured_2pt`.
pub const TOOL_BODY_MARGIN: egui::Margin = egui::Margin { left: 8, right: 8, top: 0, bottom: 6 };

/// The body wrapper [`show_window`] hands to a window's draw closure: call [`BodyBounds::show`]
/// around the window's BODY (below its custom title bar) and the window is bounded correctly in
/// both of its states.
///
/// ⚠ This exists so the wrap lives in the crate the gate compiles. It was spelled at the
/// vike-desktop call site as a bare `if scrolls { ScrollArea… } else { body(ui) }`, and
/// `vike-desktop` is in `xtask::ci::tables`' `EXCLUDE_FROM_CI` while `app-check` runs only
/// `cargo nextest run -p vike-desktop` (which reaches `chart_gpu.rs` and nothing else) — so every
/// mutation the tests below claim for the scroll half was a mutation of the TEST HARNESS, and
/// deleting `auto_shrink(false)` from production left all of them green.
///
/// ⚠ **A `fills` kind takes NO container at all** — [`Self::show`] hands the body straight
/// through — and that is the same id-chain argument one step further out. Only THREE kinds fill
/// (Chart, Dom, Polymarket) and only ONE of them is drawn at the chart call site that passes the
/// body no wrapper; Dom and Polymarket are dispatched from the TOOL call site, which does call
/// [`Self::show`]. Wrapping them would put an extra `Ui` level into the id chain of the DOM ladder
/// and the Polymarket cockpit — discarding every egui-persisted widget state inside them ONCE, on
/// upgrade, which is the exact failure this type exists to avoid. A `fills` body also needs no
/// container: it sizes itself to the available rect, so there is nothing to bound. Gated by
/// `the_tool_sites_fills_body_is_handed_through_unwrapped`.
///
/// ⚠ For a NON-`fills` kind the container is created UNCONDITIONALLY and only its SIZING is
/// toggled — that is the whole
/// point, not a simplification. A `ScrollArea` inserted when the latch flips would change `ui.id`
/// for the entire body, and egui discards auto-id-keyed widget state on an id change: the Studio
/// code editor's cursor, selection and UNDO STACK, every inner scroll offset, every
/// `CollapsingState`, a half-typed field in the Connections editor — all thrown away the first
/// time a user dragged an edge. VERIFIED in `egui-0.36.1/src/containers/scroll_area.rs`: with both
/// directions disabled and `auto_shrink` on, `ScrollArea::begin` computes `current_bar_use` from
/// `show_bars` (false, since `Prepared::end`'s `content_is_too_large` ANDs in `direction_enabled`)
/// so no space is reserved, `content_clip_rect` takes the `else` arm for every disabled direction
/// and inherits the parent clip verbatim, `state.offset` is clamped to `content_size -
/// inner_rect.size()` = zero, and `Prepared::end`'s `(false, true) => content_size[d]` arm makes
/// the reserved `outer_rect` exactly the content size — "follow the content", the same rect an
/// unwrapped body would have advanced the cursor past.
pub struct BodyBounds {
    /// Whether this window's kind FILLS (`WinKind::Chart | Dom | Polymarket`). Such a body takes
    /// no container at all — see this type's doc.
    fills: bool,
    /// Whether this window is app-owned — by the user's own drag ([`WinState::user_sized`]) or by
    /// the ARENA CEILING ([`WinState::arena_bounded`]) — and so will NOT grow for its body any
    /// more, which is what makes the body bound itself. Always false for a `fills` kind.
    scrolls: bool,
}

impl BodyBounds {
    /// Lay out a window's body inside the bounding container this window needs.
    ///
    /// The `ScrollArea` goes HERE — around the BODY, below the window's custom title bar — and
    /// never via `Window::scroll(true)`: egui's `Window::show_dyn` calls `scroll.show(ui,
    /// add_contents)` around the ENTIRE closure, which would scroll vike's own title bar (its
    /// move-drag and its close button) out of reach on an overflowing window.
    ///
    /// `auto_shrink(!scrolls)` is what makes the latched half work, and it is not decoration:
    /// `scroll_area.rs` resolves the `(scroll_enabled, auto_shrink)` pair `(true, false)` to
    /// `inner_size[d]` — "let scroll area be larger than content; fill with blank space" — so the
    /// body reports the WINDOW's size rather than its content's, and `Resize::begin`'s
    /// `desired_size.max(last_content_size)` content floor never engages.
    ///
    /// ⚠ A `fills` kind returns BEFORE any of that: `body(ui)` on the caller's own `Ui`, no
    /// container, no extra id level — byte-identical to the unwrapped spelling the Dom and
    /// Polymarket bodies were dispatched under before this wrapper existed.
    pub fn show<R>(&self, ui: &mut egui::Ui, body: impl FnOnce(&mut egui::Ui) -> R) -> R {
        if self.fills {
            return body(ui);
        }
        egui::ScrollArea::new([self.scrolls, self.scrolls])
            .auto_shrink(!self.scrolls)
            .show(ui, body)
            .inner
    }

    /// Whether this window's body is being bounded rather than grown for. Tests assert on it; no
    /// production caller needs to branch on it (that is [`Self::show`]'s job).
    pub fn scrolls(&self) -> bool {
        self.scrolls
    }
}

/// Show one window with a CUSTOM single-row title bar (no egui title bar): consume any forced
/// geometry, draw, read the live rect back. `draw` renders the title bar (first row) + content and
/// RETURNS the title bar's drag delta in screen points, which we apply to the window position;
/// geometry is app-controlled via `current_pos` so the custom bar drives movement. Its second
/// argument is the [`BodyBounds`] this window's BODY must be laid out through.
/// Returns `true` if the user dragged this window's title bar this frame (a manual move),
/// so the caller can drop any remembered tiling mode (don't re-tile a hand-placed layout).
///
/// ⚠ `bounds` is a CEILING as well as a clamp, and that is this function's job rather than
/// egui's: egui bounds what it PAINTS (an `Area`'s content `Ui` is clipped to its `constrain_rect`)
/// but it does not bound how large a `Window` is LAID OUT, so a body bigger than the arena paints
/// a window whose far edges — and therefore [`window_resize_handles`]' bands — land outside that
/// clip, where `egui-0.36.1/src/hit_test.rs` drops them and no drag can reach them. See
/// [`WinState::arena_bounded`].
pub fn show_window(
    ctx: &egui::Context,
    w: &mut WinState,
    bounds: Rect,
    draw: impl FnOnce(&mut egui::Ui, &BodyBounds) -> egui::Vec2,
) -> bool {
    let forced = w.pending.take();
    if let Some(rect) = forced {
        w.pos = rect.min;
        w.size = rect.size();
        w.force_frames = 4; // pin the arranged geometry briefly so egui commits it, then release
    }
    // ⚠ THE ARENA CEILING (size half). No window may be laid out LARGER than the arena it lives
    // in — `w.size` is authoritative for every pinned window (maximized, `fills`, `user_sized`,
    // `arena_bounded`), and a band drag can push it past `bounds` on either axis in one gesture.
    // egui clamps what it PAINTS regardless (`Window::show_dyn` does
    // `resize.max_size = resize.max_size.min(constrain_rect.size())` before the frame margin comes
    // off), so leaving the oversized value in `w.size` only makes the two DISAGREE: the bands
    // anchor on the painted rect while the next drag's `w.size.y + d.y` starts from a number
    // nobody can see, and the whole divergence is released in one jump — the same failure the
    // both-axes floor inside the resize block below exists to prevent, one bound over.
    //
    // Floored at `MINW`×`MINH` so a degenerate arena (an app window dragged tiny, a frame taken
    // mid-restore) cannot squash every window to nothing IRREVERSIBLY, and skipped outright for a
    // non-positive `bounds` — `Rect::NOTHING`'s width is `-inf`, and `w.size` must not be built
    // out of that.
    if bounds.is_positive() {
        w.size = w.size.min(egui::vec2(bounds.width().max(MINW), bounds.height().max(MINH)));
    }
    // Keep the window inside the workspace: its top can't go above the main caption's bottom,
    // nor past the status bar / rail (ctx.content_rect doesn't exclude our sub-ui caption panel).
    w.pos.x = w.pos.x.clamp(bounds.min.x, (bounds.max.x - 80.0).max(bounds.min.x));
    w.pos.y = w.pos.y.clamp(bounds.min.y, (bounds.max.y - 40.0).max(bounds.min.y));
    // ⚠ THE ARENA CEILING (position half), and it is deliberately NARROWER than the clamp above:
    // only a window the CEILING owns is placed so it FITS. The clamp above lets a window overhang
    // by design (80×40 of it must stay reachable), which is right for a window egui is still free
    // to pull back in — but an `arena_bounded` window is `current_pos` + `fixed_size` from here
    // on, so `Context::constrain_window_rect_to_area` would move the PAINTED window back inside
    // while `w.pos` went on describing a window nobody sees, and the bands (anchored on the
    // painted rect) would answer for a different window than the drag arithmetic.
    if w.arena_bounded && bounds.is_positive() {
        w.pos = w.pos.min(bounds.max - w.size).max(bounds.min);
    }

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
    // A tool window whose user has grabbed an edge band (`user_sized`) is app-owned from then on,
    // exactly like a `fills` kind: pinned geometry, and a body that must BOUND itself because the
    // window will no longer grow for it. `scrolls` is that instruction, handed to `draw` so the
    // caller can wrap its body in a `ScrollArea` — a `fills` body already sizes itself to the
    // available rect, so it never scrolls.
    //
    // ⚠ `fills` is carried too, and it does more than zero `scrolls`: `BodyBounds::show` puts NO
    // container around a `fills` body, because Dom and Polymarket are dispatched from the TOOL
    // call site (which calls `show`) while only Chart uses the wrapper-less chart call site. See
    // [`BodyBounds`].
    //
    // ⚠ `arena_bounded` confers the SAME ownership without anybody grabbing anything: a body that
    // wants more room than the arena has gets the window capped at the arena and told to bound
    // itself, because the alternative is a window painted past `bounds` whose bottom band lands
    // outside `Area`'s clip rect where nothing can hit it. See that field's doc.
    let bounded = !fills && w.arena_bounded;
    let body_bounds = BodyBounds { fills, scrolls: !fills && (w.user_sized || bounded) };
    // ⚠ Recomputed after the resize block below (`pinning_now`) — see the note there. This copy
    // is only what the `Window` builder needs BEFORE the frame is drawn.
    let pinning = w.force_frames > 0 || w.maximized || fills || w.user_sized || bounded;
    let mut win = egui::Window::new("")
        .id(w.id)
        .title_bar(false) // custom title bar lives inside the content
        .movable(false) // we move the window via the custom bar's drag
        // ⚠ NOT a sizing change, and this flag is NOT what makes a window auto-grow. egui 0.36's
        // `Window::show_dyn` does `let resize = resize.resizable(false); // We resize it manually`
        // UNCONDITIONALLY, on the line right after `PossibleInteractions::new(&area, &resize,
        // is_collapsed)` reads it — so the builder flag feeds ONLY whether egui registers its own
        // edge widgets. `Resize::end`'s `if self.with_stroke || self.resizable[d]` loop is
        // unaffected, and an unlatched tool window still auto-grows to its content exactly as it
        // did. Turning it off removes the mechanism that would fight our own bands below — and
        // egui could never have served the LEFT edge here anyway: `PossibleInteractions::new`
        // reads `resize_left: resizable.x && (movable || pivot.x() != Align::LEFT)`, and these
        // windows are `.movable(false)` with the default LEFT_TOP pivot.
        .resizable(false)
        .constrain_to(bounds);
    win = if pinning {
        win.current_pos(w.pos).fixed_size(w.size)
    } else {
        win.default_pos(w.pos).default_size(w.size)
    };

    // Custom edge/corner resize for EVERY kind (egui's own native resize is off — see the
    // `.resizable(false)` note above, and note that egui structurally refuses the left edge for a
    // non-movable LEFT_TOP-pivot window, so the bands are the only thing that can serve it).
    // Disabled while maximized or during a forced-geometry pin (arrange/restore settle).
    let resizable_now = !w.maximized && w.force_frames == 0;
    let wid = w.id;
    let mut resize: Option<(u8, egui::Vec2)> = None;
    let mut drag = egui::Vec2::ZERO;
    let resp = win.show(ctx, |ui| {
        drag = draw(ui, &body_bounds);
        if resizable_now {
            resize = window_resize_handles(ui, wid);
        }
    });
    if let Some((mask, d)) = resize {
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
        // ⚠ The AXIS the pressed band actually consumes, not the whole delta. Each arm above reads
        // exactly one component — the side bands `d.x`, the bottom band `d.y` — so a press on a
        // side band that jitters only VERTICALLY (or on the bottom band, only horizontally)
        // resizes NOTHING. Testing `d != Vec2::ZERO` latched the window on precisely that motion:
        // the same irreversible conversion the zero-delta guard below exists to prevent, one axis
        // over, and a pointer that moves perfectly straight for one frame is the exception rather
        // than the rule. Gated by `cross_axis_jitter_on_a_band_does_not_latch_the_window`.
        let moved_the_edge = (mask & (1 | 2) != 0 && d.x != 0.0) || (mask & 8 != 0 && d.y != 0.0);
        if !fills && moved_the_edge {
            // LATCH (see `WinState::user_sized`): from the first band MOVEMENT this tool window is
            // app-owned — pinned geometry, self-bounding body. Guarded on `!fills` because a
            // `fills` kind is app-owned from frame one and must NEVER take the latch: its body
            // sizes itself to the available rect, so handing it `scrolls == true` would wrap a
            // self-filling body in a `ScrollArea` and break it.
            //
            // ⚠ `moved_the_edge` also subsumes the NON-ZERO guard this started as (a zero delta
            // moves no component), which matters because `window_resize_handles` returns `Some` on
            // every `resp.dragged()` frame, and egui marks a `Sense::drag()` widget dragged on the
            // PRESS frame, where `drag_delta()` is zero. Latching on `Some` alone turned one
            // accidental CLICK on a window's 8pt edge into a permanent, irreversible conversion to
            // `fixed_size` + scrolling for the rest of the session.
            //
            // ⚠ BOTH axes are floored HERE, not just the one the pressed band consumed. Each arm
            // above floors only its own axis (`MINW` under `mask & (1 | 2)`, `MINH` under
            // `mask & 8`), so a SIDE-band drag latched with `w.size.y` left at whatever the
            // pre-latch read-back wrote — the PAINTED content height, which for a short body (an
            // empty list, a collapsed panel) is well under `MINH`. From the next frame
            // `pinning_now` builds the window `.fixed_size(w.size)`, so that stale value becomes
            // the window's declared geometry while the window is PAINTED somewhere else entirely:
            // `Resize::end` takes `size[d] = state.last_content_size[d]` for a `Window`, and a
            // latched body is a `ScrollArea` whose `inner_size` is floored at
            // `min_scrolled_size` — 64pt, BOTH axes, unconditional for an enabled direction
            // (`egui-0.36.1/src/containers/scroll_area.rs`: `if direction_enabled[d] {
            // inner_size[d] = inner_size[d].max(min_scrolled_size[d]); }`). `w.size` then stops
            // describing the window a user sees, which is exactly what "for pinned windows
            // `w.size`/`w.pos` are authoritative" (the read-back's own comment below) promises it
            // does. The visible consequence is the NEXT drag on that axis not tracking the
            // pointer: that drag's PRESS frame carries a zero delta but still runs the arm above,
            // whose `.max(MIN*)` snaps the stale value up before the move frame adds anything. So
            // the whole accumulated divergence is released in one step — MEASURED by deleting this
            // line, a +90pt drag on the bottom band moved the painted edge from 100 to 250, a
            // 150pt jump. Gated by
            // `a_side_band_latch_floors_the_axis_its_band_never_touched`.
            //
            // ⚠ What this is NOT is a stranding fix, and a review round asked for it as one: the
            // claim was that `fixed_size` makes `ui.max_rect()` equal `w.size` exactly (true —
            // `Resize::fixed_size` sets `min_size = max_size = size` and `Resize::begin` clamps
            // `desired_size` into that pair), so a short stale value would put BOTH of
            // `window_resize_handles`' anchor rects under its size guard and leave the window
            // unresizable by any means. It cannot: the same 64pt `min_scrolled_size` floor holds
            // the PAINTED rect — and therefore `ui.min_rect()` — clear of that guard whatever
            // `w.size` says (MEASURED at 100pt tall on the fixture above, against a `fits` floor
            // of `TITLE_CLEAR + 2*BAND` = 52). Deleting this line leaves every band allocated and
            // reddens only the DRAG assertion. Do not restate the stranding story.
            w.size = w.size.max(egui::vec2(MINW, MINH));
            w.user_sized = true;
        }
        // ⚠ There is deliberately NO `force_frames` re-pin here. The line that used to sit on this
        // spot — `w.force_frames = w.force_frames.max(1); // commit the new size on the next
        // frame` — was DEAD as written: `resize` is `Some` only when `resizable_now`, which
        // requires `force_frames == 0`, so `.max(1)` raised it to 1 and the unconditional
        // decrement below put it straight back to 0 in the same call, leaving nothing for the next
        // frame to read. Its comment was therefore a promise the code did not keep, which is how
        // it survived: it reads like the thing that commits a resize, and the `user_sized` latch
        // reads like a duplicate of it.
        //
        // ⚠ It does NOT stay dead now that `pinning_now` is re-derived BELOW it, and that is why
        // it is deleted rather than moved: `force_frames > 0` is the first term of `pinning_now`,
        // so re-pinning here would suppress this frame's read-back on EVERY frame a band reported
        // a drag — including a bare press, which must change nothing (see the non-zero-delta guard
        // above) and must leave an unlatched window egui-owned. MEASURED by restoring the line:
        // `the_bottom_band_stays_inside_a_window_shorter_than_its_box` reddens with "the bottom
        // edge must drag: 135 -> 220", the drag applying more than it was given.
        //
        // What commits a new size is `pinning_now` below — through `fills`/`maximized` for those
        // kinds, and through the `user_sized` latch for a tool window from its first movement. The
        // move path's own `.max(1)` sits AFTER the decrement and is a different case: a move is
        // driven by the title bar's own response and genuinely needs the NEXT frame pinned.
    }

    // ⚠ RE-DERIVED, not the `pinning` computed before the frame. The resize block above can set
    // `user_sized` on THIS frame, and with movement-latching the first latching frame ALWAYS
    // carries a non-zero delta — so reading the stale flag here let the end-of-frame read-back
    // overwrite the delta just applied with egui's pre-drag rect. A press and a move coalescing
    // into one frame's `RawInput` is routine with a high-polling mouse.
    let pinning_now = w.force_frames > 0 || w.maximized || fills || w.user_sized || bounded;

    if let Some(r) = resp {
        let rect = r.response.rect;
        // Only sync geometry back from egui when it OWNS the geometry (non-pinned tool windows,
        // native resize). For pinned windows (maximized / force-pinned / chart) `w.size`/`w.pos`
        // are authoritative — set by maximize, arrange, or our own resize handles — and reading
        // back the fixed size here would clobber a just-applied resize.
        if !pinning_now {
            w.size = rect.size();
            w.pos = rect.min; // egui owns geometry on free frames — sync back (all-edge resize)
        }
        // ⚠ THE ARENA CEILING (the latch). `rect` is egui's OWN answer for where it just painted
        // this window, which is the only rect that can report the overflow: a `Window` is painted
        // at `Resize::end`'s `size[d] = state.last_content_size[d]` whatever `fixed_size` says, so
        // neither `w.size` nor the builder's inputs know the body overran. One frame of overflow
        // is unavoidable — the body has to lay out before anything can measure it — and one frame
        // is all it costs, because `force_frames` pins a freshly-opened window anyway, so a tool
        // window that cannot fit is bounded before an operator has seen it settle.
        //
        // NOT applied to a `fills` kind: its body sizes itself to the rect it is offered, so it
        // never overruns, and `scrolls` must stay false for it (`BodyBounds::show` hands a `fills`
        // body straight through — wrapping a self-filling body in a `ScrollArea` breaks it).
        // NOT applied while maximized: that path already writes `w.size = bounds.size()` every
        // frame, so any excess is rounding, which `CEILING_SLACK` covers.
        if !fills
            && !w.maximized
            && !w.arena_bounded
            && bounds.is_positive()
            && (rect.width() > bounds.width() + CEILING_SLACK
                || rect.height() > bounds.height() + CEILING_SLACK)
        {
            w.arena_bounded = true;
            // Take the size egui painted, capped at the arena, rather than whatever `w.size`
            // happens to hold: during the open pin `w.size` is still the SPAWN size (the read-back
            // above is skipped), and a body that grew past the arena on one axis has usually
            // settled on a perfectly good width on the other.
            w.size =
                rect.size().min(egui::vec2(bounds.width().max(MINW), bounds.height().max(MINH)));
            w.pos = w.pos.min(bounds.max - w.size).max(bounds.min);
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

/// Custom edge/corner resize handles for EVERY window kind (native egui resize is off).
///
/// ⚠ This used to say "for a pinned chart window" and served only the `fills` kinds. It is now
/// the ONLY resize mechanism any window has, tool windows included: egui 0.36's
/// `PossibleInteractions::new` reads `resize_left: resizable.x && (movable || pivot.x() !=
/// Align::LEFT)`, and every window here is built `.movable(false)` with the default LEFT_TOP
/// pivot, so egui structurally refuses the LEFT edge no matter how the builder is spelled.
///
/// Allocates invisible drag-sensing bands along the LEFT, RIGHT and BOTTOM edges + the two
/// BOTTOM corners of the content rect, drawn AFTER the body so they win the interaction over the
/// content underneath. Returns `(edge_mask, drag_delta)` for the active handle this frame:
/// bit 1 = left, 2 = right, 8 = bottom. `show_window` applies the delta to `w.pos`/`w.size`.
///
/// ⚠ **The bands are anchored on `ui.min_rect()`, NOT `ui.max_rect()`, and that is load-bearing.**
/// This function runs after the body has been laid out, so `min_rect` is the content drawn this
/// frame — and for an egui `Window` that IS the painted rect. `Window::show_dyn` builds its
/// `Resize` with `.with_stroke(false)` and then forces `let resize = resize.resizable(false); //
/// We resize it manually` unconditionally, so `Resize::end`'s `if self.with_stroke ||
/// self.resizable[d]` loop always takes the `else { size[d] = state.last_content_size[d]; }` arm,
/// and `last_content_size` is `content_ui.min_size()` — this `min_rect`'s size. `max_rect` is
/// `Resize::begin`'s `state.desired_size`, a MONOTONE HIGH-WATER MARK
/// (`desired_size = desired_size.max(last_content_size)` on every non-dragging frame), so for an
/// UNLATCHED tool window whose body is shorter than its box the two diverge: the bottom band and
/// both bottom corners were allocated BELOW the visible window, where the bottom edge could not be
/// dragged at all and an invisible 8pt `Sense::drag` strip floated in dead space showing a resize
/// cursor and swallowing clicks on whatever was underneath. A REGRESSION, since that edge worked
/// through egui's native resize before `.resizable(false)`.
///
/// ⚠ **Chart, DOM and Polymarket are all band-identical to the `max_rect` spelling, but only the
/// CHART is so for the reason this doc used to give.** It said "a `fills` body covers `max_rect`
/// exactly", which is true of the chart alone: `vike_chart::chart::draw` ends with
/// `add_space(ui.clip_rect().bottom().min(ui.max_rect().bottom()) - ui.cursor().top())`, and
/// `cursor().top()` is already past every `item_spacing.y` gap it laid, so its `min_rect` lands
/// exactly on `max_rect` (gated by
/// `a_chart_windows_band_anchor_is_byte_identical_to_the_old_max_rect_one`).
///
/// The two LADDER bodies OVERRUN instead. `vike_panels::dom::draw` and `vike_cockpit::ladder::draw`
/// each read `ui.available_rect_before_wrap()` ONCE at the top and then lay out sibling
/// allocations SUMMING to that height — the DOM four (`header` 26 / `toolbar` 26 / the ladder
/// region / `footer` 28), the cockpit ladder two (`ladder_header` the density's header height, 24
/// at Normal / the region) — so the
/// `item_spacing.y` egui inserts BETWEEN siblings was never subtracted and is pure overrun, by
/// `(siblings - 1) * item_spacing.y`: at the app's `item_spacing` (`(6, 4)`, set by
/// `vike_ui_theme::appearance::install` at the default density) that is 12pt for the DOM and 4pt
/// for the Polymarket cockpit. **Their bands do not move for it**, because
/// `egui-0.36.1/src/layout.rs`'s
/// `Region::expand_to_include_rect` unions each child's rect into `min_rect` AND `max_rect` alike
/// — "`max_rect` will always be at least the size of `min_rect`", per `Region::max_rect`'s own doc
/// — so by the time this function runs the overrun is in both and the two anchors agree. Gated by
/// `a_ladder_windows_bands_survive_its_item_spacing_overflow_byte_identical`, which asserts the
/// overrun against the rect the body was OFFERED and then asserts the bands anyway.
///
/// ⚠ That union is also why the anchor swap is a one-directional fix, and a review round read it
/// the other way round (that the ladders' `max_rect` bands sat 12pt INBOARD of the painted edge —
/// they did not, and nothing moved). `max_rect ⊇ min_rect` always, so a `max_rect` band can only
/// ever sit at-or-OUTSIDE the painted edge. The two can diverge only where a body UNDER-fills its
/// box, which is the tool-window case above and which no `fills` kind can reach.
///
/// ⚠ **With ONE fallback, and it is a stranding fix rather than a softening of that rule.** A
/// window whose painted content is too short to carry a band set gets no bands from `min_rect` —
/// and because these bands are the only resize mechanism any window has, "no bands" means
/// "unresizable by any means, permanently", where before `.resizable(false)` egui's own right and
/// bottom edges still worked. So when `min_rect` cannot carry them and `max_rect` can, the bands
/// go on `max_rect`; the arm argues itself at the site.
///
/// ⚠ **A band that is ALLOCATED is not a band that can be HIT, and nothing in this function can
/// tell the difference.** `ui.interact` records `interact_rect: self.clip_rect().intersect(rect)`,
/// an `Area`'s content `Ui` is clipped to its `constrain_rect` (`area.rs`:
/// `ui.set_clip_rect(self.constrain_rect)` — "Don't paint outside our bounds"), and
/// `egui-0.36.1/src/hit_test.rs` drops any widget whose `interact_rect.is_negative()` before it
/// measures a single distance. So a band placed outside the workspace bounds is registered,
/// answers `Context::read_response`, sets a resize cursor on nothing, and can never be dragged.
/// That is not hypothetical geometry: an unlatched tool window is PAINTED at its content
/// (`Resize::end`'s `size[d] = state.last_content_size[d]`), so a body taller than the arena put
/// the bottom band and both bottom corners below it — the reported "why can't I resize it at the
/// bottom, the status bar is over the window". The cure is upstream of this function, in
/// [`WinState::arena_bounded`]: a window that cannot fit is CAPPED at the arena, so every band it
/// allocates is inside the clip. Gated by
/// `a_body_taller_than_the_arena_keeps_the_window_and_its_bands_inside_it`.
///
/// **The TOP edge is deliberately skipped, and there is no bit 4.** It overlaps the title bar's
/// move-drag and its control buttons — and the cost is not just the drag: a drag-only band
/// suppresses CLICKS as well, because `egui-0.36.1/src/hit_test.rs` returns only the drag-widget
/// when one is on top ("The drag-widget is separate from the click-widget, so return only the
/// drag-widget" → `click: None`). A top band would therefore kill double-click-to-maximize and
/// the top slice of every control button on all twelve window kinds. The side bands start below
/// the title bar (`TITLE_CLEAR`) for the same reason. This is an owner's ruling, not an
/// oversight: do not add a top band, and do not widen the side bands over the title bar.
fn window_resize_handles(ui: &egui::Ui, wid: Id) -> Option<(u8, Vec2)> {
    use egui::{CursorIcon, Rect, Sense, pos2};
    const BAND: f32 = 8.0;
    const TITLE_CLEAR: f32 = 36.0; // keep side bands clear of the title-bar controls
    // Room for a non-overlapping band set: a bottom band between two corners, and side bands that
    // start below `TITLE_CLEAR` and are still at least `BAND` tall above the bottom one.
    let fits = |r: Rect| r.width() >= 3.0 * BAND && r.height() >= TITLE_CLEAR + 2.0 * BAND;
    let painted = ui.min_rect(); // the PAINTED content rect — see this function's doc
    let r = if fits(painted) {
        painted
    } else if fits(ui.max_rect()) {
        // ⚠ THE STRANDING FALLBACK, and it is not a softening of the anchor rule above — it is
        // the case where that rule has nothing to offer. These bands are the ONLY resize
        // mechanism any window has (`show_window` builds every `Window` `.resizable(false)`, and
        // egui structurally refuses the left edge for a non-movable LEFT_TOP-pivot window
        // anyway), so returning `None` here does not fall back to egui — it makes the window
        // UNRESIZABLE BY ANY MEANS, permanently. A tool window whose body paints shorter than
        // `TITLE_CLEAR + 2*BAND` is exactly that case: `egui-0.36.1/src/containers/resize.rs`'s
        // `Resize::end` takes `else { size[d] = state.last_content_size[d]; }` for a `Window`
        // (`with_stroke(false)` + the forced `resizable(false)`), so the window is PAINTED at its
        // content and `min_size` never floors it.
        //
        // `max_rect` is `Resize::begin`'s `state.desired_size`, a monotone high-water mark that
        // still remembers the spawn size, so it is the one rect that can carry a band set here.
        // The bands then sit partly below the painted window — the very shape the anchor rule
        // above exists to avoid — and that is ACCEPTED here because it is self-healing: one grab
        // latches the window (`user_sized`), `show_window` pins it to `w.size`, the body is
        // bounded to that, and `min_rect` clears `fits` from the next frame on, so the fallback
        // arm is never taken again for that window. Gated by
        // `a_window_too_short_for_its_bands_is_still_resizable`.
        //
        // ⚠ Self-healing WITHOUT help from the latch's `MINW`×`MINH` floor, and a review round
        // claimed otherwise — that a SIDE-band grab pins the window at the short painted height,
        // that a pinned window's `max_rect` IS `w.size`, and that this arm therefore has nothing
        // left to offer. The middle step is true (`Resize::fixed_size` sets
        // `min_size = max_size = size`) and the conclusion does not follow: a LATCHED body is a
        // `ScrollArea` with both directions enabled, and
        // `egui-0.36.1/src/containers/scroll_area.rs` floors its `inner_size` at
        // `min_scrolled_size` — 64pt, both axes, unconditional for an enabled direction — so the
        // PAINTED rect clears `fits` from the latching frame on however small `w.size` is, and it
        // is `painted` rather than this arm that serves the window. MEASURED by deleting that
        // floor: every band is still allocated — see
        // `a_side_band_latch_floors_the_axis_its_band_never_touched`.
        ui.max_rect()
    } else {
        return None; // too small to place non-overlapping bands on either rect
    };
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

#[path = "orderflow_on_tests.rs"]
#[cfg(test)]
mod orderflow_on_tests;

/// The edge-band resize path (`window_resize_handles` -> `show_window`), the `user_sized` latch
/// and the body wrapper [`BodyBounds`], driven headlessly through a real `egui::Context`.
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
/// ⚠ The harness calls the PRODUCTION wrapper ([`BodyBounds::show`]) and the PRODUCTION inset
/// ([`TOOL_BODY_MARGIN`]). It modelled both itself until 2026-09-13, which meant the scroll half
/// of this fix was gated by mutations of the HARNESS rather than of shipped code — `vike-desktop`
/// is in `xtask::ci::tables`' `EXCLUDE_FROM_CI`, so deleting `auto_shrink(false)` from the call
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

#[cfg(test)]
mod venue_key_tests {
    use super::*;
    use egui::{Rect, pos2, vec2};

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
/// fresh `WinState` is `None` (spot/legacy), and `Option<vike_model::AssetClass>` itself
/// round-trips through JSON losslessly (the property `WinSnap::asset_class` relies on).
#[cfg(test)]
mod asset_class_tests {
    use super::*;
    use egui::{Rect, pos2, vec2};
    use vike_model::AssetClass;

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

#[path = "pane_model_tests.rs"]
#[cfg(test)]
mod pane_model_tests;

#[path = "compare_tests.rs"]
#[cfg(test)]
mod compare_tests;

#[path = "series_pane_model_tests.rs"]
#[cfg(test)]
mod series_pane_model_tests;

/// [`WinKind::from_slug`] exhaustiveness: every kind round-trips through its slug, slugs are
/// distinct, unknowns parse to `None`, and the legacy `VIKE_TOOL` vocabulary (the silent
/// `_ => Calendar` match in `vike-app`'s `main.rs` this API replaced) still resolves identically.
#[path = "winkind_slug_tests.rs"]
#[cfg(test)]
mod winkind_slug_tests;
