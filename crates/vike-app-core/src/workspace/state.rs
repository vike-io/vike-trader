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
            | WinKind::Polymarket => false,
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
    /// **THE OPERATOR CHOSE THIS WINDOW'S SERIES**, so
    /// [`series_follow::follow_backend`](crate::series_follow::follow_backend) may not retarget it.
    ///
    /// `false` from [`WinState::new`] — the startup default chart
    /// ([`startup::plan`](crate::startup::plan)'s `BTCUSDT` `1m` on [`DEFAULT_VENUE`]) and a fresh
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
    /// [`series_follow::follow_backend`](crate::series_follow::follow_backend) may retarget its
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
/// region / `footer` 28), the cockpit ladder two (`ladder_header` 26 / the region) — so the
/// `item_spacing.y` egui inserts BETWEEN siblings was never subtracted and is pure overrun, by
/// `(siblings - 1) * item_spacing.y`: at the app's `item_spacing` (`vike-desktop`'s `main.rs`,
/// `s.spacing.item_spacing = vec2(6.0, 4.0)`) that is 12pt for the DOM and 4pt for the Polymarket
/// cockpit. **Their bands do not move for it**, because `egui-0.36.1/src/layout.rs`'s
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

#[cfg(test)]
mod orderflow_on_tests {
    use super::*;
    use egui::{Rect, pos2, vec2};

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
#[cfg(test)]
mod resize_tests {
    use super::*;
    use egui::{Context, Event, PointerButton, RawInput, pos2, vec2};
    use std::cell::Cell;

    /// The headless viewport, and the DEFAULT workspace bounds `show_window` clamps into (the app
    /// passes its desktop rect). Big enough that no fixture window is ever clamped or constrained
    /// — which is why the arena CEILING needs [`arena`] to be seen at all.
    fn screen() -> Rect {
        Rect::from_min_size(Pos2::ZERO, vec2(1600.0, 1000.0))
    }

    /// The app's real window ARENA, shaped like the one `vike-desktop`'s `draw_chrome` computes:
    /// the viewport minus the caption/menu strip at the top and minus the fixed-height status bar
    /// at the bottom (`app_ui.rs`'s `egui::Panel::bottom("statusbar")` is added BEFORE the
    /// `CentralPanel`, so `app.desktop = ui.max_rect()` already excludes it — MEASURED in
    /// `egui-0.36.1/src/containers/panel.rs`, whose `show_inside_dyn` sets
    /// `cursor.max[axis] = visible_outer_rect.min[axis]` for a `PanelSide::Bottom` before
    /// `CentralPanel` reads `available_rect_before_wrap`).
    ///
    /// ⚠ It is deliberately SMALLER than [`screen`] on both axes: the viewport stays 1600×1000 so
    /// a window can be painted outside the arena without leaving the context's own screen rect,
    /// which is exactly the shape the reported bug has.
    fn arena() -> Rect {
        Rect::from_min_max(pos2(0.0, 40.0), pos2(1200.0, 1000.0 - STATUS_BAR_H))
    }

    /// The shipped status bar's fixed height (`vike-desktop`'s `main.rs`, `STATUS_BAR_H`) — the
    /// strip [`arena`] excludes, and the one an over-tall window was reported as sitting under.
    const STATUS_BAR_H: f32 = 22.0;

    /// The title strip a real tool window draws before its body
    /// (`crates/vike-app-core/src/workspace/title_bar.rs`'s `tool_title_bar`, which moved down out
    /// of vike-desktop when it started reserving the Connections window's tab slot), modelled as a
    /// plain full-width allocation. Its only load-bearing property is
    /// that it clears `window_resize_handles`' `TITLE_CLEAR` — which is what keeps the side bands
    /// off the real title bar's move-drag and its control buttons.
    const TITLE_H: f32 = 24.0;

    /// The `item_spacing` the shipped app sets on every style (`vike-desktop`'s `main.rs`,
    /// `s.spacing.item_spacing = egui::vec2(6.0, 4.0)`) — egui's own default is `(8, 3)`. The `y`
    /// component is what a LADDER body over-allocates by, once per gap between its siblings; see
    /// [`Site::ToolFillsSpaced`] and [`LADDER_SIBLINGS`].
    const ITEM_SPACING: Vec2 = egui::vec2(6.0, 4.0);

    /// The sibling counts of the two shipped LADDER bodies, each with the kind it belongs to, and
    /// the OVERRUN each therefore produces past the rect its body was offered:
    /// `(siblings - 1) * ITEM_SPACING.y`.
    ///
    /// * `vike_panels::dom::draw` — `header` (26) / `toolbar` (26) / the ladder region /
    ///   `footer` (28), four allocations whose heights sum to the `full` it read at the top, so
    ///   3 gaps = **12pt**.
    /// * `vike_cockpit::ladder::draw` — `ladder_header` (26) / the region, two allocations
    ///   summing the same way, so 1 gap = **4pt**. (The cockpit's rail, PTB header and ticket are
    ///   drawn BEFORE the ladder and consume only their own heights; the ladder reads
    ///   `available_rect_before_wrap` after them, so their gaps are already in the cursor and
    ///   contribute nothing.)
    const LADDER_SIBLINGS: [(&str, usize); 2] = [("DOM", 4), ("Polymarket cockpit", 2)];

    /// The width of a FLOATING scrollbar's interact strip: `egui-0.36.1/src/style.rs`'s
    /// `ScrollStyle::floating()` sets `bar_width: 10.0`, and `scroll_area.rs` senses the bar over
    /// `max_bar_rect` — `outer_rect.with_min_x(max_cross - full_width)`, i.e. the outermost
    /// `bar_width` points of the scroll area, whatever width the bar is currently ANIMATED to.
    const BAR_W: f32 = 10.0;

    /// Which vike-desktop CALL SITE a fixture models. There are two of them and the split is not
    /// by kind: `app_ui.rs`'s `if w.kind == workspace::WinKind::Chart` branch draws ONE of the
    /// three `fills` kinds, and the other two — DOM and Polymarket — are dispatched from the TOOL
    /// site alongside the nine intrinsic-height kinds. A harness that modelled only the chart site
    /// for `fills` was therefore modelling the wrong site for two of the three.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    enum Site {
        /// The CHART call site: a `fills` body covering `ui.max_rect()` exactly, no margin frame,
        /// no wrapper at all.
        Chart,
        /// The TOOL call site for a NON-`fills` kind: title strip, [`TOOL_BODY_MARGIN`], then a
        /// fixed-size body handed through [`BodyBounds::show`].
        Tool,
        /// The TOOL call site for a `fills` kind (DOM, Polymarket): the same margin frame, a body
        /// that covers the frame's available rect EXACTLY, handed through [`BodyBounds::show`] —
        /// which must hand a `fills` body straight through.
        ///
        /// ⚠ It is the ID-CHAIN and WRAPPER reference, not a model of either shipped ladder's
        /// GEOMETRY. Both ladders read `available_rect_before_wrap` once and then lay out siblings
        /// summing to it, so they end up PAST that rect by their own item-spacing gaps — this
        /// fixture lands on it exactly. Use [`Site::ToolFillsSpaced`] for anything that turns on
        /// where the body ends.
        ToolFills,
        /// The SAME site with [`BodyBounds::show`] BYPASSED — origin/main's spelling, and the
        /// reference `ToolFills`'s id chain, bands and painted rect are compared against.
        ToolFillsBare,
        /// The TOOL site with a LADDER-shaped `fills` body: `N` sibling allocations SUMMING to the
        /// rect the body read once at the top, which is the shape both shipped ladders have
        /// (`vike_panels::dom::draw`, `vike_cockpit::ladder::draw`). The `item_spacing.y` gaps
        /// BETWEEN those siblings were never subtracted, so the body over-allocates past its
        /// available rect by `(N - 1)` of them.
        ///
        /// ⚠ [`Site::ToolFills`] cannot express that and never could: its body allocates exactly
        /// the rect it was offered, so its `min_rect == max_rect` is a property of the FIXTURE.
        /// The doc's "Chart / DOM / Polymarket are byte-identical" claim was gated by that
        /// fixture, and so held for a reason no shipped ladder has. The claim itself SURVIVES —
        /// `Region::expand_to_include_rect` unions the overrun into `max_rect` as well, so both
        /// anchors end on it — and this fixture is what makes that falsifiable instead of
        /// tautological. See
        /// `a_ladder_windows_bands_survive_its_item_spacing_overflow_byte_identical`.
        ToolFillsSpaced(usize),
    }

    /// One scripted window: a persistent `Context`, a pointer script, and a body whose size the
    /// test chooses. The draw closure MIRRORS one of the two vike-desktop call sites — see
    /// [`Site`].
    struct Harness {
        ctx: Context,
        time: f64,
        ptr: Option<Pos2>,
        queued: Vec<Event>,
        /// Last frame's `BodyBounds::scrolls`, exactly as `show_window` computed it.
        scrolled: Cell<bool>,
        /// The size the window BODY lays out at, below the title strip. Ignored by the `fills`
        /// sites, whose bodies cover whatever rect they are offered.
        body: Vec2,
        /// Which call site this fixture draws as.
        site: Site,
        /// `ui.min_rect()` / `ui.max_rect()` of the window's content `Ui` at the moment
        /// `window_resize_handles` reads one, so a test can prove WHICH rect the bands anchor on.
        ///
        /// ⚠ BOTH are captured where that function reads them — AFTER the body — and `content_max`
        /// captured them at the top of the closure until 2026-09-13, which is a different rect for
        /// any body that OVERFLOWS: `egui-0.36.1/src/layout.rs`'s `Region::expand_to_include_rect`
        /// unions the child's rect into `min_rect` AND `max_rect` alike, so `max_rect` absorbs the
        /// overflow before the bands are placed. Reading the top-of-closure value made the two
        /// anchors look as though they diverged for the ladder bodies when the production function
        /// sees them EQUAL — and a claim was written on it. [`Self::offered_max`] is that
        /// top-of-closure rect, kept under a name that says what it is.
        content_min: Cell<Rect>,
        content_max: Cell<Rect>,
        /// The rect the window's content `Ui` was OFFERED, read before a single child is added —
        /// `Resize::begin`'s `inner_rect`. A body that over-allocates ends past it; see
        /// [`Site::ToolFillsSpaced`]. It is NOT what the bands anchor on under either spelling.
        offered_max: Cell<Rect>,
        /// The BODY `Ui`'s `max_rect` — the `ScrollArea`'s `inner_rect`, which is where its
        /// floating scrollbars sense.
        body_max: Cell<Rect>,
        /// The BODY `Ui`'s `Ui::id` — the id every auto-id widget inside the body is keyed on.
        body_id: Cell<Option<Id>>,
        /// The workspace bounds handed to `show_window` — the app's `desktop` rect. Defaults to
        /// [`screen`] (so no fixture is ever constrained); [`Harness::in_arena`] narrows it to
        /// [`arena`] for the tests that turn on a window meeting the arena's edge.
        bounds: Rect,
    }

    impl Harness {
        fn tool(body: Vec2) -> Self {
            Self::with(body, Site::Tool)
        }

        fn fill() -> Self {
            Self::with(Vec2::ZERO, Site::Chart)
        }

        /// A `fills` kind drawn at the TOOL call site (DOM / Polymarket). `wrapped` picks the
        /// production spelling ([`BodyBounds::show`]) or origin/main's bare one.
        fn tool_fill(wrapped: bool) -> Self {
            Self::with(Vec2::ZERO, if wrapped { Site::ToolFills } else { Site::ToolFillsBare })
        }

        /// A LADDER-shaped `fills` body at the TOOL call site: `siblings` allocations summing to
        /// the rect it read at the top — see [`Site::ToolFillsSpaced`].
        fn ladder(siblings: usize) -> Self {
            Self::with(Vec2::ZERO, Site::ToolFillsSpaced(siblings))
        }

        fn with(body: Vec2, site: Site) -> Self {
            let ctx = Context::default();
            // The app zeroes the window margin (`vike-desktop`'s `main.rs`, the chart-title flush
            // fix) and egui's default is 6 — left at the default, every geometric assertion below
            // would measure a different frame inset than production does, and `fixed_size` (an
            // INNER content size) would disagree with the OUTER rect `show_window` reads back by
            // twice that margin.
            // ⚠ `item_spacing` is the SHIPPED value too (`vike-desktop`'s `main.rs`:
            // `s.spacing.item_spacing = egui::vec2(6.0, 4.0)`), not egui's default `(8, 3)`. It
            // is geometry the bands depend on for the same reason the margin is: a ladder body
            // over-allocates past its available rect by exactly the gaps egui inserts between its
            // siblings, so a harness on the default spacing would measure a shift the app never
            // produces. See [`Site::ToolFillsSpaced`].
            ctx.all_styles_mut(|s| {
                s.spacing.window_margin = egui::Margin::ZERO;
                s.spacing.item_spacing = ITEM_SPACING;
            });
            Self {
                ctx,
                time: 0.0,
                ptr: None,
                queued: Vec::new(),
                scrolled: Cell::new(false),
                body,
                site,
                content_min: Cell::new(Rect::NOTHING),
                content_max: Cell::new(Rect::NOTHING),
                offered_max: Cell::new(Rect::NOTHING),
                body_max: Cell::new(Rect::NOTHING),
                body_id: Cell::new(None),
                bounds: screen(),
            }
        }

        /// Narrow the workspace bounds this fixture's window lives in to the app-shaped
        /// [`arena`] — the viewport minus the caption strip and the status bar.
        fn in_arena(mut self) -> Self {
            self.bounds = arena();
            self
        }

        /// Draw one frame of this window.
        fn frame(&mut self, w: &mut WinState) {
            let mut raw = RawInput {
                screen_rect: Some(screen()),
                time: Some(self.time),
                ..Default::default()
            };
            self.time += 1.0 / 60.0;
            if let Some(p) = self.ptr {
                raw.events.push(Event::PointerMoved(p));
            }
            raw.events.append(&mut self.queued);
            self.ctx.begin_pass(raw);
            let scrolled = &self.scrolled;
            let content_min = &self.content_min;
            let content_max = &self.content_max;
            let offered_max = &self.offered_max;
            let body_max = &self.body_max;
            let body_id = &self.body_id;
            let body = self.body;
            let site = self.site;
            show_window(&self.ctx, w, self.bounds, |ui, bounds| {
                scrolled.set(bounds.scrolls());
                offered_max.set(ui.max_rect());
                let _ = ui.allocate_space(vec2(ui.max_rect().width(), TITLE_H));
                // A `fills` body covers whatever rect it is offered — the chart's `leftover` fill;
                // the DOM and Polymarket ladders sized to `available_rect_before_wrap`.
                let fill_body = |ui: &mut egui::Ui| {
                    body_max.set(ui.max_rect());
                    body_id.set(Some(ui.id()));
                    let full = ui.max_rect();
                    let _ = ui.allocate_rect(full, egui::Sense::hover());
                };
                match site {
                    // The CHART call site: no margin frame, and no `BodyBounds` wrapper at all.
                    Site::Chart => fill_body(ui),
                    Site::Tool => {
                        egui::Frame::new().inner_margin(TOOL_BODY_MARGIN).show(ui, |ui| {
                            bounds.show(ui, |ui| {
                                body_max.set(ui.max_rect());
                                body_id.set(Some(ui.id()));
                                let _ = ui.allocate_space(body);
                            });
                        });
                    }
                    // The TOOL call site with a `fills` kind, through the production wrapper...
                    Site::ToolFills => {
                        egui::Frame::new().inner_margin(TOOL_BODY_MARGIN).show(ui, |ui| {
                            bounds.show(ui, fill_body);
                        });
                    }
                    // ...and the same site with the wrapper bypassed (origin/main).
                    Site::ToolFillsBare => {
                        egui::Frame::new().inner_margin(TOOL_BODY_MARGIN).show(ui, fill_body);
                    }
                    // The LADDER shape: read the available rect ONCE, then lay out `n` siblings
                    // whose heights SUM to it — exactly `vike_panels::dom::draw`'s
                    // `header`/`toolbar`/`region`/`footer` and `vike_cockpit::ladder::draw`'s
                    // `ladder_header`/`region`. Neither subtracts the `item_spacing.y` egui
                    // inserts BETWEEN them, so the body ends `(n - 1)` gaps past the rect it read.
                    Site::ToolFillsSpaced(n) => {
                        egui::Frame::new().inner_margin(TOOL_BODY_MARGIN).show(ui, |ui| {
                            bounds.show(ui, |ui| {
                                body_max.set(ui.max_rect());
                                body_id.set(Some(ui.id()));
                                let full = ui.available_rect_before_wrap();
                                let each = full.height() / n as f32;
                                for _ in 0..n {
                                    let _ = ui.allocate_exact_size(
                                        vec2(full.width(), each),
                                        egui::Sense::hover(),
                                    );
                                }
                            });
                        });
                    }
                }
                // `window_resize_handles` runs immediately after this closure returns and nothing
                // between the two touches the placer, so THESE are the two rects it chooses
                // between — `min_rect` for the anchor, `max_rect` for the fallback and for the
                // spelling origin/main used. ⚠ `max_rect` is read HERE rather than at the top,
                // because it is not the same rect: every child the cursor advances past is unioned
                // into it (`Region::expand_to_include_rect`), so an overflowing body moves it.
                content_min.set(ui.min_rect());
                content_max.set(ui.max_rect());
                Vec2::ZERO // this fixture's title strip is inert — it never move-drags
            });
            // A headless pass harvests no textures; say "deliberately not rendered" rather than
            // letting `TexturesDelta`'s drop panic on unapplied deltas (egui 0.36).
            self.ctx.end_pass().drop_without_applying_deltas();
        }

        fn frames(&mut self, w: &mut WinState, n: usize) {
            for _ in 0..n {
                self.frame(w);
            }
        }

        /// The rect `window_resize_handles` actually allocated for `mask` — read back out of the
        /// context rather than recomputed here, so the test grabs the PRODUCTION band and cannot
        /// drift from its `BAND`/`TITLE_CLEAR` arithmetic. `None` when no band was allocated.
        fn band(&self, w: &WinState, mask: u8) -> Option<Rect> {
            self.ctx.read_response(w.id.with(("winresize", mask))).map(|r| r.rect)
        }

        /// The rect the window was actually PAINTED at (egui's own area geometry) — what a user
        /// sees, and NOT the same thing as `w.size` once the latch has taken hold.
        ///
        /// ⚠ It is the window FRAME's rect, so it is [`Self::window_stroke`] points LARGER than
        /// the content rect the bands anchor on, on every side.
        fn painted(&self, w: &WinState) -> Rect {
            self.ctx.memory(|m| m.area_rect(w.id)).expect("the window has been shown")
        }

        /// The window frame's stroke width. `egui-0.36.1/src/containers/frame.rs` counts it as
        /// part of the frame's total margin (`content_rect + inner_margin +
        /// MarginF32::from(self.stroke.width) + outer_margin`), and `Frame::window` takes it from
        /// `visuals.window_stroke()` — so an area rect sits exactly this far outside the content
        /// rect `ui.min_rect()` reports, on every side. Read from the live style rather than
        /// written down, because it is a theme value.
        fn window_stroke(&self) -> f32 {
            self.ctx.style_of(self.ctx.theme()).visuals.window_stroke.width
        }

        fn move_to(&mut self, at: Pos2) {
            self.ptr = Some(at);
        }

        fn button(&mut self, at: Pos2, pressed: bool) {
            self.ptr = Some(at);
            self.queued.push(Event::PointerButton {
                pos: at,
                button: PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::default(),
            });
        }

        /// Press at `from` and move to `to` inside ONE frame's `RawInput` — what a high-polling
        /// mouse routinely delivers. `to` must stay inside the band: egui hit-tests with
        /// `interact_pos`, which every pointer event in the frame updates
        /// (`egui-0.36.1/src/input_state/mod.rs`), so the widget picked up is the one under the
        /// FINAL position.
        fn press_and_move(&mut self, from: Pos2, to: Pos2) {
            self.ptr = Some(from);
            self.queued.push(Event::PointerButton {
                pos: from,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::default(),
            });
            self.queued.push(Event::PointerMoved(to));
        }
    }

    /// Grab the `mask` band and drag it by `by`, one mouse step per frame: hover, press, move,
    /// release. `Sense::drag()` alone reports `dragged()` from the PRESS frame on
    /// (`egui-0.36.1/src/interaction.rs`: "This widget is just sensitive to drags, so we can mark
    /// it as dragged right away"), and `Response::drag_delta` is that frame's pointer delta — so
    /// the press frame hands `show_window` a ZERO delta and the move frame hands it exactly `by`.
    fn drag_band(h: &mut Harness, w: &mut WinState, mask: u8, by: Vec2) {
        let at = h
            .band(w, mask)
            .expect("the band must have been allocated before it can be grabbed")
            .center();
        h.move_to(at);
        h.frame(w); // hover
        h.button(at, true);
        h.frame(w); // press — `dragged()` but a ZERO delta, so it must NOT latch
        h.move_to(at + by);
        h.frame(w); // the drag itself
        h.button(at + by, false);
        h.frame(w); // release
    }

    /// A tool window spawned at `size`, settled past `OPEN_PIN_FRAMES` so `force_frames` has
    /// drained and the bands are allocated.
    fn settled_tool(h: &mut Harness, kind: WinKind, at: Pos2, size: Vec2) -> WinState {
        let mut w = WinState::tool("fixture", kind, Rect::from_min_size(at, size));
        h.frames(&mut w, 14);
        w
    }

    #[test]
    fn a_left_drag_on_a_tool_window_moves_the_left_edge_and_pins_the_right() {
        // THE REPORTED BUG. A tool window's LEFT edge could not be dragged at all: egui refuses it
        // structurally — `PossibleInteractions::new` reads `resize_left: resizable.x && (movable
        // || pivot.x() != Align::LEFT)` and these windows are `.movable(false)` at the default
        // LEFT_TOP pivot — so the app's own bands are the only thing that can ever serve it.
        //
        // MUTATION that must redden this: drop `|| w.user_sized` from `show_window`'s
        // `pinning_now`. The band arithmetic still runs, but the end-of-frame read-back
        // (`if !pinning_now`) hands the geometry straight back to egui's stale rect and the left
        // edge snaps home.
        let mut h = Harness::tool(vec2(560.0, 520.0));
        let mut w = settled_tool(&mut h, WinKind::Greeks, pos2(240.0, 140.0), vec2(600.0, 420.0));
        assert!(!w.user_sized, "an untouched window has not latched");

        let before = w.pos.x + w.size.x; // the RIGHT edge — the invariant a left drag must pin
        let x0 = w.pos.x;
        let width0 = w.size.x;

        drag_band(&mut h, &mut w, 1, vec2(100.0, 0.0));

        assert!(w.user_sized, "moving a band latches the window as app-owned");
        assert!(
            (w.pos.x - (x0 + 100.0)).abs() < 2.0,
            "left edge must follow the drag: {x0} -> {} (wanted {})",
            w.pos.x,
            x0 + 100.0
        );
        assert!(
            (w.size.x - (width0 - 100.0)).abs() < 2.0,
            "width must shrink by the drag: {width0} -> {}",
            w.size.x
        );
        assert!(
            (w.pos.x + w.size.x - before).abs() < 2.0,
            "the RIGHT edge is pinned: {before} -> {}",
            w.pos.x + w.size.x
        );

        // ...and it STAYS moved. This half is what catches egui handing the geometry back a frame
        // later, which is exactly what the un-latched code did.
        h.frames(&mut w, 4);
        assert!(
            (w.pos.x - (x0 + 100.0)).abs() < 2.0,
            "the left edge must not snap home on a later frame: {}",
            w.pos.x
        );
        assert!(
            (w.pos.x + w.size.x - before).abs() < 2.0,
            "the right edge must stay pinned on later frames: {}",
            w.pos.x + w.size.x
        );

        // ⚠ ...and finally on what a USER sees. Every assertion above reads a value `show_window`
        // itself just wrote, so all of them would pass with the window painted somewhere else
        // entirely. The painted rect is egui's own answer.
        let painted = h.painted(&w);
        assert!(
            (painted.min.x - w.pos.x).abs() < 2.0,
            "the PAINTED left edge must be where `w.pos.x` says: {} vs {}",
            painted.min.x,
            w.pos.x
        );
        assert!(
            (painted.width() - w.size.x).abs() < 2.0,
            "the PAINTED width must be `w.size.x`: {} vs {}",
            painted.width(),
            w.size.x
        );
    }

    #[test]
    fn a_resized_tool_window_stops_growing_to_its_content() {
        // THE OTHER REPORTED BUG: a tool window could not be dragged SMALLER, because
        // `Resize::begin` does `desired_size = desired_size.max(last_content_size)` on every
        // non-dragging frame — the window's floor is its content's minimum size.
        //
        // ⚠ The assertion is on the PAINTED rect, never on `w.size`: after the latch that value is
        // written by the band arithmetic itself, so asserting on it would pass with the whole fix
        // deleted.
        //
        // ⚠ It drags BOTH WAYS, and that is not thoroughness — it is what makes each half of the
        // fix load-bearing. The two mutations redden different halves, and a shrink-only test was
        // MEASURED green under the second one:
        //
        //   * `BodyBounds::show` -> call `body(ui)` directly (no container at all) and phase A
        //     reddens: the body reports its CONTENT size again, which re-arms `Resize`'s floor.
        //   * change only `auto_shrink(!scrolls)` to `auto_shrink(true)` and phase A stays GREEN —
        //     a scroll area is capped at the space available either way once its content
        //     OVERFLOWS, so shrinking alone cannot tell the two apart. Phase B is what reddens:
        //     with auto-shrink ON, a window dragged TALLER than its content collapses back to the
        //     content (`scroll_area.rs`'s `(true, true) => inner_size[d].min(content_size[d])`),
        //     so the painted window stops matching the size the user dragged and the bands end up
        //     outside it.
        let body = vec2(560.0, 400.0);
        let spawn = vec2(600.0, 400.0);
        let mut h = Harness::tool(body);
        let mut w = settled_tool(&mut h, WinKind::Studio, pos2(200.0, 120.0), spawn);
        // ⚠ Compared against the SPAWN height, not against `body.y`: the latter is true of any
        // window taller than its body and would hold with nothing grown at all. Against the spawn
        // it says the thing that matters — egui grew the window PAST what the app asked for.
        assert!(
            h.painted(&w).height() > spawn.y,
            "before the latch egui grows the window past its spawn size onto its content floor — \
             that is the bug ({} <= {})",
            h.painted(&w).height(),
            spawn.y
        );

        // PHASE A — drag the bottom edge UP, through the content floor.
        drag_band(&mut h, &mut w, 8, vec2(0.0, -180.0));
        h.frames(&mut w, 4);

        assert!(w.user_sized, "the bottom band latches the window too");
        assert!(h.scrolled.get(), "a latched tool body is told to bound itself");
        assert!(
            h.painted(&w).height() < body.y,
            "the PAINTED window must now be shorter than its content ({} >= {})",
            h.painted(&w).height(),
            body.y
        );

        // PHASE B — and back DOWN, past the content: the painted window must follow the drag
        // rather than collapsing onto the body it contains.
        let tall = w.size.y + 400.0;
        drag_band(&mut h, &mut w, 8, vec2(0.0, 400.0));
        h.frames(&mut w, 4);
        assert!(
            (h.painted(&w).height() - tall).abs() < 4.0,
            "the PAINTED window must match the dragged size, not the body's ({} vs {tall})",
            h.painted(&w).height()
        );
    }

    #[test]
    fn the_bottom_band_stays_inside_a_window_shorter_than_its_box() {
        // THE ANCHOR. `window_resize_handles` read `ui.max_rect()`, which for an UNLATCHED tool
        // window is `Resize::begin`'s `desired_size` — a MONOTONE HIGH-WATER MARK — while the
        // window is PAINTED at `last_content_size` (`Resize::end` takes `else { size[d] =
        // state.last_content_size[d]; }`, since `Window::show_dyn` forces `resizable(false)` and
        // builds its `Resize` `.with_stroke(false)`). A body SHORTER than the spawn box makes the
        // two diverge, and the bottom band plus both bottom corners were then allocated BELOW the
        // visible window: the bottom edge could not be grabbed at all, and an invisible 8pt
        // `Sense::drag` strip floated in dead space showing a resize cursor and swallowing clicks
        // on whatever sat underneath. A REGRESSION — that edge worked through egui's native
        // resize until `.resizable(false)`.
        //
        // MUTATION that must redden this: in `window_resize_handles`, replace the whole
        // `let r = if fits(painted) { … }` selection with `let r = ui.max_rect();`.
        let mut h = Harness::tool(vec2(520.0, 100.0)); // a body FAR shorter than the box
        let mut w = settled_tool(&mut h, WinKind::News, pos2(300.0, 160.0), vec2(600.0, 420.0));

        // NON-VACUITY: the two rects really do diverge here, so "inside the painted rect" is a
        // claim about the fix rather than about a degenerate fixture.
        let (min_r, max_r) = (h.content_min.get(), h.content_max.get());
        assert!(
            max_r.height() > min_r.height() + 100.0,
            "the fixture must actually diverge: max_rect {} vs min_rect {}",
            max_r.height(),
            min_r.height()
        );

        let painted = h.painted(&w);
        for mask in [8u8, 1 | 8, 2 | 8] {
            let band = h.band(&w, mask).unwrap_or_else(|| panic!("band {mask} must be allocated"));
            assert!(
                painted.contains_rect(band),
                "band {mask} at {band:?} must lie INSIDE the painted window {painted:?}"
            );
        }

        // ...and the edge is not merely inside, it WORKS: grabbing it resizes the window.
        let h0 = w.size.y;
        drag_band(&mut h, &mut w, 8, vec2(0.0, 60.0));
        h.frames(&mut w, 4);
        assert!(
            (w.size.y - (h0 + 60.0)).abs() < 2.0,
            "the bottom edge must drag: {h0} -> {}",
            w.size.y
        );
    }

    #[test]
    fn a_body_taller_than_the_arena_keeps_the_window_and_its_bands_inside_it() {
        // THE REPORTED BUG, in the owner's words: "WHY I CANT RESIZE IT AT BOTTOM?? IT SEEMS LIKE
        // WINDOW IS OVERLAYED BY BOTTOM STATUS BAR?" — on the Connections tool window.
        //
        // The arena is NOT the problem and the theory it suggests is wrong: `app_ui.rs` adds
        // `egui::Panel::bottom("statusbar")` BEFORE the `CentralPanel`, and a bottom panel sets
        // `cursor.max[axis] = visible_outer_rect.min[axis]` on its parent
        // (`egui-0.36.1/src/containers/panel.rs`), so `app.desktop` genuinely excludes the strip —
        // see [`arena`], which is shaped the same way.
        //
        // What is true is the WINDOW. Until it is app-owned a tool window is painted at its
        // CONTENT, whatever its declared size says: `Window::show_dyn` forces
        // `resize.resizable(false)` and `.with_stroke(false)`, so `Resize::end` always takes
        // `else { size[d] = state.last_content_size[d]; }`. A body taller than the arena therefore
        // paints a window whose bottom edge is past `bounds` — under the status bar, exactly as
        // reported — and `window_resize_handles` then allocates the bottom band and both bottom
        // corners out there, where `Ui::interact`'s `interact_rect: self.clip_rect().intersect(
        // rect)` is NEGATIVE (the area's clip rect is its `constrain_rect`) and
        // `egui-0.36.1/src/hit_test.rs` drops the widget before measuring anything. The band
        // exists, `read_response` answers for it, and it can never be hit.
        //
        // MUTATION that must redden this: delete the `w.arena_bounded = true;` arm from
        // `show_window`'s read-back block (the ARENA CEILING latch). PERFORMED — it reddens here
        // and on the width twin, and leaves `a_window_that_fits_its_arena_never_takes_the_ceiling`
        // green. ⚠ MEASURED under it, by stripping this test's earlier assertions one layer at a
        // time so each later claim was actually reached — because a test that only ever reddens on
        // its first assertion has proved nothing about the rest:
        //
        //   * painted rect  `[..1476]` against an arena ending at `978` — the window is drawn
        //     498pt past the status bar's top edge, which is the report verbatim;
        //   * band 8        `[[129 1467] - [671 1475]]` against a clip rect of
        //     `[[0 40] - [1200 978]]` — allocated, answering `read_response`, 489pt outside;
        //   * the DRAG      a full press-move-release on it moves the window `1436 -> 1436`.
        //
        // That last line is the whole bug in one number, and it is why this test drags rather than
        // stopping at geometry.
        //
        // ⚠ The assertion ORDER is deliberate, because every band is still ALLOCATED under that
        // mutation and `h.band(..)` still answers for it. The geometric claims come FIRST so the
        // mutation's first red is the symptom a user sees — a window painted past the arena — and
        // only then the bookkeeping. Asserting `w.arena_bounded` first would have reddened on a
        // flag while proving nothing about the window.
        const BODY_H: f32 = 1400.0;
        let mut h = Harness::tool(vec2(520.0, BODY_H)).in_arena();
        let arena = arena();
        let mut w =
            settled_tool(&mut h, WinKind::Connections, pos2(120.0, 60.0), vec2(560.0, 400.0));

        // NON-VACUITY, from the fixture's own arithmetic rather than from anything the fix wrote:
        // this body genuinely cannot fit, so the containment below is a claim about the ceiling.
        assert!(
            BODY_H > arena.height(),
            "the fixture must overflow: body {BODY_H} vs arena {}",
            arena.height()
        );

        assert!(!w.user_sized, "nothing was grabbed, so the USER latch must stay clear");

        // THE SYMPTOM: the window is no longer painted past the arena, i.e. under the status bar.
        let painted = h.painted(&w);
        assert!(
            painted.max.y <= arena.max.y + 1.0,
            "the painted window must not reach past the arena bottom ({}) into the status bar: {}",
            arena.max.y,
            painted.max.y
        );
        assert!(
            arena.contains_rect(painted.shrink(1.0)),
            "the whole window must sit inside the arena {arena:?}: {painted:?}"
        );

        // THE MECHANISM: every band is inside the clip rect, which for this `Area` IS the arena.
        for mask in [8u8, 1 | 8, 2 | 8, 1, 2] {
            let band = h.band(&w, mask).unwrap_or_else(|| panic!("band {mask} must be allocated"));
            assert!(
                arena.intersect(band).is_positive(),
                "band {mask} at {band:?} must intersect the clip rect {arena:?} — a band outside \
                 it is registered, answers `read_response`, and can never be hit"
            );
            assert!(
                painted.contains_rect(band),
                "band {mask} at {band:?} must lie inside the painted window {painted:?}"
            );
        }

        // ...and the bottom edge WORKS. It can only be dragged UP from here — the window is at the
        // ceiling — which is the half the owner could not reach at all.
        let h0 = w.size.y;
        assert!(
            (h0 - arena.height()).abs() < 1.0,
            "the ceiling must cap the window at the arena height {}: {h0}",
            arena.height()
        );
        drag_band(&mut h, &mut w, 8, vec2(0.0, -120.0));
        h.frames(&mut w, 4);
        assert!(
            (w.size.y - (h0 - 120.0)).abs() < 2.0,
            "the bottom edge must drag the window SMALLER: {h0} -> {}",
            w.size.y
        );
        let painted = h.painted(&w);
        assert!(
            (painted.height() - (h0 - 120.0)).abs() < 2.0,
            "...and the PAINTED window must follow it: {}",
            painted.height()
        );

        // ...and back DOWN again, so the ceiling is a cap rather than a one-way freeze.
        drag_band(&mut h, &mut w, 8, vec2(0.0, 60.0));
        h.frames(&mut w, 4);
        assert!(
            (w.size.y - (h0 - 60.0)).abs() < 2.0,
            "the bottom edge must drag back down too: {}",
            w.size.y
        );

        // ...and only now the bookkeeping, which is the MEANS rather than the end.
        assert!(w.arena_bounded, "a body the arena cannot hold must latch the ceiling");
        assert!(h.scrolled.get(), "...and the body must be told to bound itself");
        assert!(
            h.body_max.get().height() < BODY_H,
            "...which is what BOUNDS it: offered {} of {BODY_H}",
            h.body_max.get().height()
        );
    }

    #[test]
    fn a_body_wider_than_the_arena_keeps_both_side_bands_inside_it() {
        // The SAME mechanism on the other axis, and WHICH band it costs depends on where the
        // window sits. `Context::constrain_window_rect_to_area` clamps an oversized window's
        // position into `[area.left - margin, area.left]`, where
        // `margin = window.width() - area.width()` — so the overflow hangs off whichever side the
        // pivot did not pin. At this fixture's position the left edge lands exactly ON the arena's
        // and the RIGHT band is the one outside the clip; a window whose pivot had been dragged
        // further left loses the LEFT band instead. Either way at least one side band is gone,
        // which is how a wide body can reproduce the ORIGINAL report's symptom ("resize does not
        // work from the left edge") on a build where #1773 already fixed its cause — a different
        // defect wearing the same complaint.
        //
        // MUTATION that must redden this: as above, delete the `w.arena_bounded = true;` arm —
        // PERFORMED, and it reddens here first on the painted WIDTH, not on a flag (same
        // assertion-order argument as the height twin).
        const BODY_W: f32 = 1400.0;
        let mut h = Harness::tool(vec2(BODY_W, 300.0)).in_arena();
        let arena = arena();
        let mut w = settled_tool(&mut h, WinKind::Studio, pos2(60.0, 80.0), vec2(560.0, 400.0));

        assert!(
            BODY_W > arena.width(),
            "the fixture must overflow: body {BODY_W} vs arena {}",
            arena.width()
        );

        let painted = h.painted(&w);
        assert!(
            painted.width() <= arena.width() + 1.0,
            "the window must be capped at the arena width {}: {}",
            arena.width(),
            painted.width()
        );
        for mask in [1u8, 2, 1 | 8, 2 | 8] {
            let band = h.band(&w, mask).unwrap_or_else(|| panic!("band {mask} must be allocated"));
            assert!(
                arena.intersect(band).is_positive(),
                "band {mask} at {band:?} must intersect the clip rect {arena:?}"
            );
        }

        // ...and the LEFT edge drags, pinning the right — the original report's exact claim, now
        // asserted on a window that could not previously offer that band at all.
        let right0 = w.pos.x + w.size.x;
        let x0 = w.pos.x;
        drag_band(&mut h, &mut w, 1, vec2(90.0, 0.0));
        h.frames(&mut w, 4);
        assert!(
            (w.pos.x - (x0 + 90.0)).abs() < 2.0,
            "the left edge must follow the drag: {x0} -> {}",
            w.pos.x
        );
        assert!(
            (w.pos.x + w.size.x - right0).abs() < 2.0,
            "...with the RIGHT edge pinned: {right0} -> {}",
            w.pos.x + w.size.x
        );

        // ...and only now the bookkeeping.
        assert!(w.arena_bounded, "a body the arena cannot hold must latch the ceiling");
        assert!(
            h.body_max.get().width() < BODY_W,
            "...which is what BOUNDS it: offered {} of {BODY_W}",
            h.body_max.get().width()
        );
    }

    #[test]
    fn a_window_that_fits_its_arena_never_takes_the_ceiling() {
        // The ceiling's NO-OP half, and the reason it is a latch on OVERFLOW rather than a clamp
        // on every window. A tool window that fits must stay exactly what it was before this
        // change: unpinned, sized by egui, growing to its content — nine tool kinds open at
        // 560×400 and several bodies are naturally bigger, so bounding one that fits would open it
        // into scrollbars for nothing.
        //
        // ⚠ It runs in the NARROW [`arena`], not in [`screen`]: run against bounds nothing can
        // reach, this test would pass with the ceiling wired to fire unconditionally.
        //
        // MUTATION that must redden this: drop the `rect.width() > … || rect.height() > …`
        // condition from the ARENA CEILING latch (latch every window), or drop `CEILING_SLACK`
        // from both comparisons and hand the fixture a body that lands exactly on the arena.
        let mut h = Harness::tool(vec2(620.0, 500.0)).in_arena();
        let arena = arena();
        let w = settled_tool(&mut h, WinKind::News, pos2(100.0, 120.0), vec2(560.0, 400.0));

        assert!(!w.arena_bounded, "a window that fits must NOT take the ceiling");
        assert!(!h.scrolled.get(), "...and its body is never told to bound itself");
        assert!(!w.user_sized, "...nor does the user latch fire on its own");
        let painted = h.painted(&w);
        assert!(
            painted.height() < arena.height(),
            "non-vacuity: the fixture must actually fit ({} vs {})",
            painted.height(),
            arena.height()
        );
        // The pre-existing promise this must not break: egui still owns the geometry, so the
        // window has GROWN to its taller and wider content rather than staying at the 560×400 it
        // opened with.
        assert!(
            w.size.y > 400.0,
            "an unbounded window must still grow to its content: {}",
            w.size.y
        );
        assert!(w.size.x > 560.0, "...on the other axis too: {}", w.size.x);
    }

    /// `window_resize_handles`' own `BAND` / `TITLE_CLEAR`, restated here so
    /// [`expected_bands`] can rebuild the band set from OUTSIDE the function under test. They are
    /// private to it, so this is a deliberate second copy: a production change to either constant
    /// moves every band and must redden the byte-identity tests below, which is exactly what a
    /// shared constant would hide.
    const BAND: f32 = 8.0;
    const TITLE_CLEAR: f32 = 36.0;

    /// The five band rects `window_resize_handles` places on an anchor rect, keyed by mask, spelled
    /// as origin/main spelled them (`bl`, `br`, `left`, `right`, `bottom`).
    fn expected_bands(r: Rect) -> [(u8, Rect); 5] {
        [
            (
                1 | 8,
                Rect::from_min_max(pos2(r.min.x, r.max.y - BAND), pos2(r.min.x + BAND, r.max.y)),
            ),
            (
                2 | 8,
                Rect::from_min_max(pos2(r.max.x - BAND, r.max.y - BAND), pos2(r.max.x, r.max.y)),
            ),
            (
                1,
                Rect::from_min_max(
                    pos2(r.min.x, r.min.y + TITLE_CLEAR),
                    pos2(r.min.x + BAND, r.max.y - BAND),
                ),
            ),
            (
                2,
                Rect::from_min_max(
                    pos2(r.max.x - BAND, r.min.y + TITLE_CLEAR),
                    pos2(r.max.x, r.max.y - BAND),
                ),
            ),
            (
                8,
                Rect::from_min_max(
                    pos2(r.min.x + BAND, r.max.y - BAND),
                    pos2(r.max.x - BAND, r.max.y),
                ),
            ),
        ]
    }

    #[test]
    fn a_chart_windows_band_anchor_is_byte_identical_to_the_old_max_rect_one() {
        // THE CHART must not move by ONE POINT, and it reaches that by FILLING rather than by
        // overrunning: `vike_chart::chart::draw` ends with
        // `add_space(min(clip.bottom, max_rect.bottom) - ui.cursor().top())`, and the cursor is
        // already PAST every gap it laid — so it lands exactly on `max_rect.bottom` however many
        // siblings it drew, and `min_rect == max_rect` with nothing unioned in.
        //
        // ⚠ This test ran over `[Site::Chart, Site::ToolFills]` and claimed the property for
        // Chart, DOM and Polymarket at once. The claim holds for all three, but that fixture could
        // not show it: its body allocates exactly the rect it is offered, so its
        // `min_rect == max_rect` is a property of the FIXTURE rather than of any shipped body, and
        // both ladders in fact OVERRUN. They keep the property for a different reason —
        // `Region::expand_to_include_rect` grows `max_rect` with them — which is why they have
        // their own test, `a_ladder_windows_bands_survive_its_item_spacing_overflow_byte_identical`,
        // rather than riding this one.
        //
        // ⚠ It asserted `min_rect == max_rect` and NOTHING else at first, and that gated nothing:
        // every change to where the bands actually land left it green. What is asserted now is
        // each of the five BAND RECTS, in absolute coordinates, against the arithmetic origin/main
        // performed on `ui.max_rect()`. The equality survives as the PREMISE.
        //
        // MUTATION that must redden this: `const TITLE_CLEAR: f32 = 40.0;` in
        // `window_resize_handles` (or any change to `BAND` or to a band rect's arithmetic) — the
        // chart's bands move, which is precisely what "must not move by one point" forbids.
        // ⚠ The anchor swap itself CANNOT redden THIS test and that is the claim, not a gap: the
        // chart's body reaches `ui.max_rect()`'s bottom, so `content_ui.min_rect()` IS that rect
        // (`egui-0.36.1/src/containers/resize.rs`, `Resize::begin`'s `inner_rect =
        // Rect::from_min_size(position, state.desired_size)`). The anchor is gated where the two
        // rects CAN differ, which is where a body UNDER-fills its box —
        // `the_bottom_band_stays_inside_a_window_shorter_than_its_box`. No `fills` kind can reach
        // that state, so no `fills` test gates the anchor and none pretends to.
        let mut h = Harness::fill();
        let mut w = WinState::new(
            "chartfix",
            "BTCUSDT",
            "1m",
            WinKind::Chart,
            Rect::from_min_size(pos2(200.0, 150.0), vec2(700.0, 500.0)),
        );
        h.frames(&mut w, 14);

        let (min_r, max_r) = (h.content_min.get(), h.content_max.get());
        assert_eq!(
            min_r, max_r,
            "PREMISE: the chart body fills to `max_rect`'s bottom, so the bands anchor on the \
             same rect either way"
        );
        // THE CLAIM: every band is exactly where the old `max_rect` spelling put it.
        for (mask, want) in expected_bands(max_r) {
            let got = h.band(&w, mask).unwrap_or_else(|| panic!("band {mask} must be allocated"));
            assert_eq!(got, want, "band {mask} must not move by one point");
        }
        // ...and that rect is the window a user sees, which is the property the doc claims.
        let painted = h.painted(&w);
        assert!(
            painted.contains_rect(min_r),
            "the anchor rect {min_r:?} must lie inside the painted window {painted:?}"
        );
    }

    #[test]
    fn a_ladder_windows_bands_survive_its_item_spacing_overflow_byte_identical() {
        // THE LADDER SHAPE, and the two wrong stories told about it.
        //
        // The DOM and the Polymarket cockpit each read `ui.available_rect_before_wrap()` ONCE and
        // then lay out siblings SUMMING to that height, so the `item_spacing.y` egui inserts
        // BETWEEN them was never subtracted and is pure overflow: the body ends
        // `(siblings - 1) * ITEM_SPACING.y` past the rect it was OFFERED — 12pt for the DOM's four
        // allocations, 4pt for the cockpit ladder's two ([`LADDER_SIBLINGS`]). That much is real
        // and is asserted below against [`Harness::offered_max`].
        //
        // ⚠ STORY ONE, the doc's original: "a `fills` body covers `max_rect` exactly, so
        // `min_rect == max_rect` and the bands are byte-identical". The CONCLUSION is right and
        // the REASON is not — these bodies overrun `max_rect` rather than covering it — and it was
        // gated by a fixture (`Site::ToolFills`) whose body allocates exactly the rect it is
        // offered, so the claim was unfalsifiable by construction.
        //
        // ⚠ STORY TWO, a review round's correction: "the bands therefore MOVE DOWN by the
        // overflow, and on the `max_rect` spelling the DOM's bottom band sat 12pt INBOARD of the
        // painted edge". Also false, and in the more dangerous direction — it reads as a bug fixed
        // when nothing moved. `egui-0.36.1/src/layout.rs`'s `Region::expand_to_include_rect`
        // unions every child's rect into `min_rect` AND `max_rect` alike ("`max_rect` will always
        // be at least the size of `min_rect`", `Region::max_rect`'s own doc), so by the time
        // `window_resize_handles` runs the overflow is in BOTH rects and the anchors AGREE. A
        // `max_rect` band can only ever sit at-or-OUTSIDE the painted edge, never inboard.
        // MEASURED: under the anchor mutation named below this test stays GREEN, band rects and
        // all, while `the_bottom_band_stays_inside_a_window_shorter_than_its_box` reddens.
        //
        // What this test therefore gates is the CONCLUSION with its real mechanism: a body that
        // overruns its offered rect by a measured amount still gets byte-identical bands, because
        // `max_rect` grew with it.
        //
        // MUTATION that must redden this: `const TITLE_CLEAR: f32 = 40.0;` in
        // `window_resize_handles` (or any change to `BAND` or to a band rect's arithmetic) — the
        // ladders' bands move, which is what "byte-identical" forbids. RUN: it reddens here with
        // "DOM: band 1 must not move by one point" (and the chart's twin above with the same
        // wording). ⚠ The ANCHOR SWAP (replace the whole `let r = if fits(painted) { … }`
        // selection with `let r = ui.max_rect();`) deliberately does NOT redden it: that is the
        // claim, and it was RUN too — 16 tests, one failure, and it is the under-fill one.
        for (label, siblings) in LADDER_SIBLINGS {
            let mut h = Harness::ladder(siblings);
            let mut w = WinState::new(
                "ladderfix",
                "BTCUSDT",
                "1m",
                WinKind::Dom,
                Rect::from_min_size(pos2(200.0, 150.0), vec2(700.0, 500.0)),
            );
            h.frames(&mut w, 14);

            let (min_r, max_r, offered) =
                (h.content_min.get(), h.content_max.get(), h.offered_max.get());
            let shift = (siblings - 1) as f32 * ITEM_SPACING.y;
            assert!(shift > 0.0, "{label}: a ladder has at least two siblings");

            // NON-VACUITY: the fixture really does model an OVERRUNNING body, by exactly the gaps
            // the shipped one leaves. Without this the byte-identity below is a claim about a body
            // that fits, which is the hole `Site::ToolFills` left.
            assert!(
                (min_r.max.y - offered.max.y - shift).abs() < 0.01,
                "{label}: the body must over-allocate by its {} item-spacing gaps — content \
                 bottom {} vs the {} it was offered (wanted a {shift}pt overrun)",
                siblings - 1,
                min_r.max.y,
                offered.max.y
            );

            // THE MECHANISM: `max_rect` absorbed that overrun, so the two anchors AGREE and the
            // `max_rect` spelling could not have placed a band inboard of the painted edge.
            assert_eq!(
                min_r, max_r,
                "{label}: `Region::expand_to_include_rect` unions the overrun into `max_rect` too, \
                 so the anchor the bands use and the one origin/main used are the same rect"
            );

            // ...and that rect IS the window a user sees. ⚠ Offset by the window frame's stroke,
            // which egui counts as part of the frame's margin, so the area rect sits that far
            // outside the content rect on every side.
            let painted = h.painted(&w);
            let edge = h.window_stroke();
            assert!(
                (painted.max.y - min_r.max.y - edge).abs() < 0.01,
                "{label}: the painted bottom edge IS the anchor's, plus the {edge}pt frame stroke \
                 ({} vs {})",
                painted.max.y,
                min_r.max.y
            );

            // THE CLAIM: every band, byte-identical to the arithmetic origin/main performed.
            for (mask, want) in expected_bands(min_r) {
                let got = h
                    .band(&w, mask)
                    .unwrap_or_else(|| panic!("{label}: band {mask} must be allocated"));
                assert_eq!(got, want, "{label}: band {mask} must not move by one point");
            }
        }
    }

    #[test]
    fn the_tool_sites_fills_body_is_handed_through_unwrapped() {
        // G1's regression, and the one the layout assertions above can NEVER see. DOM and
        // Polymarket are `fills` kinds dispatched from the TOOL call site, so they go through
        // `BodyBounds::show` — and a `ScrollArea` there is LAYOUT-identical (`[false, false]` +
        // `auto_shrink(true)` reserves no space and follows the content) while putting an extra
        // `Ui` level into the body's ID CHAIN. egui keys widget state on `Ui::id`, so that is one
        // silent discard of every egui-persisted state inside the DOM ladder and the Polymarket
        // cockpit — scroll offsets, collapsing headers, text cursors — on upgrade.
        //
        // The reference is the SAME fixture with the wrapper bypassed, which is origin/main's
        // spelling: same window id, same kind, same geometry, same body.
        //
        // MUTATION that must redden this: delete `if self.fills { return body(ui); }` from
        // `BodyBounds::show`.
        let rect = Rect::from_min_size(pos2(180.0, 120.0), vec2(640.0, 460.0));
        let (mut hw, mut hb) = (Harness::tool_fill(true), Harness::tool_fill(false));
        let mut ww = WinState::tool("domfix", WinKind::Dom, rect);
        let mut wb = WinState::tool("domfix", WinKind::Dom, rect);
        hw.frames(&mut ww, 14);
        hb.frames(&mut wb, 14);

        assert!(!hw.scrolled.get(), "a `fills` kind is never told to bound its body");
        let wrapped_id = hw.body_id.get().expect("the wrapped body was drawn");
        assert_eq!(
            wrapped_id,
            hb.body_id.get().expect("the bare body was drawn"),
            "the tool site's `fills` body must get the SAME `Ui::id` it had unwrapped — every \
             auto-id widget inside the DOM ladder and the Polymarket cockpit is keyed on it"
        );

        // ...and the GEOMETRY is unchanged too: same painted window, same bands, same body rect.
        assert_eq!(hw.painted(&ww), hb.painted(&wb), "the painted window must be unchanged");
        assert_eq!(hw.body_max.get(), hb.body_max.get(), "the body's own rect must be unchanged");
        for (mask, _) in expected_bands(hw.content_min.get()) {
            let got = hw.band(&ww, mask).unwrap_or_else(|| panic!("band {mask} must exist"));
            let want = hb.band(&wb, mask).expect("the bare fixture's band");
            assert_eq!(got, want, "band {mask} must be unchanged by the wrapper");
        }
    }

    #[test]
    fn a_window_too_short_for_its_bands_is_still_resizable() {
        // The bands are the ONLY resize mechanism any window has — `show_window` builds every
        // `Window` `.resizable(false)`, and egui structurally refuses the left edge for a
        // non-movable LEFT_TOP-pivot window regardless. So `window_resize_handles`' too-small
        // guard is not "skip a nicety": keyed on the PAINTED rect alone it made a tool window
        // whose body paints shorter than `TITLE_CLEAR + 2*BAND` unresizable BY ANY MEANS, for the
        // life of the window — where before `.resizable(false)` egui's own right and bottom edges
        // worked. A short body is not exotic: an empty list or a collapsed panel is one.
        //
        // MUTATION that must redden this: drop the `else if fits(ui.max_rect())` arm from
        // `window_resize_handles` (i.e. `return None` as soon as the painted rect is too small).
        let mut h = Harness::tool(vec2(520.0, 6.0)); // a body only a few points tall
        let mut w = settled_tool(&mut h, WinKind::News, pos2(300.0, 160.0), vec2(600.0, 420.0));

        // NON-VACUITY: the painted content really is too short to carry a band set, and the
        // monotone high-water rect really is not — so this fixture takes the fallback arm.
        let (min_r, max_r) = (h.content_min.get(), h.content_max.get());
        assert!(
            min_r.height() < TITLE_CLEAR + 2.0 * BAND,
            "the fixture must actually be too short: painted height {}",
            min_r.height()
        );
        assert!(
            max_r.height() >= TITLE_CLEAR + 2.0 * BAND,
            "...and the fallback rect must be able to carry the bands: {}",
            max_r.height()
        );

        for mask in [1u8, 2, 8] {
            assert!(
                h.band(&w, mask).is_some(),
                "band {mask} must be allocated — without it this window can never be resized again"
            );
        }

        // ...and the bottom edge WORKS: grabbing it resizes the window.
        let h0 = w.size.y;
        drag_band(&mut h, &mut w, 8, vec2(0.0, 200.0));
        h.frames(&mut w, 4);
        assert!(w.user_sized, "the drag must latch the window as app-owned");
        assert!(w.size.y > h0 + 100.0, "the bottom edge must have dragged: {h0} -> {}", w.size.y);

        // SELF-HEALING, which is what makes the fallback's out-of-window bands acceptable: the
        // latched window is pinned to `w.size` and its body is bounded to that, so the painted
        // rect now carries the bands itself and the fallback arm is never taken again.
        let painted = h.painted(&w);
        assert!(
            (painted.height() - w.size.y).abs() < 4.0,
            "the latched window must paint at the dragged size: {} vs {}",
            painted.height(),
            w.size.y
        );
        for mask in [8u8, 1 | 8, 2 | 8] {
            let band = h.band(&w, mask).unwrap_or_else(|| panic!("band {mask} must be allocated"));
            assert!(
                painted.contains_rect(band),
                "band {mask} at {band:?} must now lie INSIDE the painted window {painted:?}"
            );
        }
    }

    #[test]
    fn a_side_band_latch_floors_the_axis_its_band_never_touched() {
        // A SIDE-band drag latches the window, and the two resize arms above the latch floor only
        // the axis the pressed band consumes — `MINW` under `mask & (1 | 2)`, `MINH` under
        // `mask & 8`. So without the both-axes floor a side drag pinned the window with `w.size.y`
        // left at whatever the pre-latch read-back wrote: the PAINTED content height, which for a
        // short body (an empty list, a collapsed panel) is far under `MINH`.
        //
        // That value is then the window's DECLARED geometry (`pinning_now` builds it
        // `.fixed_size(w.size)`) while the window is PAINTED somewhere else entirely, because
        // `Resize::end` takes `size[d] = state.last_content_size[d]` for a `Window` and a latched
        // body is a `ScrollArea` whose `inner_size` is floored at `min_scrolled_size` — 64pt, BOTH
        // axes, unconditional for an enabled direction. `w.size` stops describing the window a
        // user sees, which is precisely what the read-back's own comment ("for pinned windows
        // `w.size`/`w.pos` are authoritative") promises it does not do, and the visible symptom is
        // the NEXT drag on that axis not tracking the pointer: the arm's own `.max(MIN*)` snaps
        // the stale value up first, so the edge moves by something other than the delta.
        //
        // ⚠ THIS IS NOT A STRANDING TEST, and a review round asked for it as one — the claim being
        // that `fixed_size` collapses `ui.max_rect()` onto `w.size` (true) and so puts BOTH of
        // `window_resize_handles`' anchor rects under its `fits` guard, leaving the window
        // unresizable by any means. It cannot: the same 64pt `min_scrolled_size` floor holds the
        // PAINTED rect — and therefore `ui.min_rect()` — well clear of that guard whatever
        // `w.size` says. The band-allocation assertions below are kept as the REFUTATION of that
        // claim and are GREEN under the mutation named here; the DRAG assertion is the gate.
        //
        // MUTATION that must redden this: delete `w.size = w.size.max(egui::vec2(MINW, MINH));`
        // from `show_window`'s latch block — the two `.max(MIN*)` calls in the arms above are the
        // per-axis floors, so deleting the line restores the per-axis-only spelling. RUN, and it
        // reddens HERE and nowhere else: "the bottom edge must follow the pointer after a
        // side-band latch: painted height 100 -> 250 for a 90pt drag (w.size.y = 250)". The jump
        // is the bottom drag's PRESS frame — a zero delta that still runs the `mask & 8` arm,
        // whose `.max(MINH)` snaps the stale value up before the move frame adds its 90.
        let mut h = Harness::tool(vec2(520.0, 6.0)); // the same short body as the test above
        let mut w = settled_tool(&mut h, WinKind::News, pos2(300.0, 160.0), vec2(600.0, 420.0));

        // NON-VACUITY: the pre-latch read-back really does leave `w.size.y` under the `MINH` the
        // bottom arm would have applied, so the floor deleted by the mutation has work to do.
        assert!(
            w.size.y < 160.0,
            "the fixture must actually spawn a window shorter than MINH: w.size.y = {}",
            w.size.y
        );

        // THE SIDE BAND — mask 1, which consumes only `d.x`.
        drag_band(&mut h, &mut w, 1, vec2(60.0, 0.0));
        h.frames(&mut w, 4);
        assert!(w.user_sized, "a side-band drag latches the window");

        // THE REFUTATION (green under the mutation, and that is the point): a latched short window
        // is NOT stranded — `min_scrolled_size` keeps the painted rect band-sized either way.
        for mask in [1u8, 2, 8, 1 | 8, 2 | 8] {
            assert!(
                h.band(&w, mask).is_some(),
                "band {mask} must be allocated after a SIDE-band latch (w.size = {:?}, \
                 painted = {:?})",
                w.size,
                h.painted(&w)
            );
        }

        // THE GATE: `w.size` describes the window a user sees, so the bottom edge TRACKS the
        // pointer. Asserted on the PAINTED rect, never on `w.size` — after the latch that value is
        // written by the band arithmetic itself, so a `w.size`-only assertion passes with the
        // floor deleted and the window painted somewhere else.
        let before = h.painted(&w).height();
        drag_band(&mut h, &mut w, 8, vec2(0.0, 90.0));
        h.frames(&mut w, 4);
        let after = h.painted(&w).height();
        assert!(
            (after - before - 90.0).abs() < 2.0,
            "the bottom edge must follow the pointer after a side-band latch: painted height \
             {before} -> {after} for a 90pt drag (w.size.y = {})",
            w.size.y
        );
    }

    #[test]
    fn cross_axis_jitter_on_a_band_does_not_latch_the_window() {
        // The latch guard tested the WHOLE delta while the resize arithmetic above it consumes
        // only the component the mask names: the side bands read `d.x`, the bottom band `d.y`. So
        // a press on the bottom band that jittered purely HORIZONTALLY (or on a side band, purely
        // vertically) resized NOTHING and latched the window anyway — the same permanent,
        // irreversible conversion to `fixed_size` + a self-bounding body that the zero-delta guard
        // exists to prevent, one axis over. A pointer that moves on exactly one axis for a frame
        // is the common case, not the exotic one.
        //
        // MUTATION that must redden this: restore `if !fills && d != egui::Vec2::ZERO` in place of
        // the `moved_the_edge` guard in `show_window`.
        // (band, the CROSS-axis jitter it consumes none of, an IN-axis drag it must still take)
        let rounds =
            [(8u8, vec2(20.0, 0.0), vec2(0.0, -40.0)), (1u8, vec2(0.0, 20.0), vec2(-40.0, 0.0))];
        for (mask, jitter, along) in rounds {
            let mut h = Harness::tool(vec2(560.0, 520.0));
            let mut w =
                settled_tool(&mut h, WinKind::Trade, pos2(240.0, 140.0), vec2(600.0, 420.0));
            let from = h.band(&w, mask).expect("the band must be allocated").center();
            let to = from + jitter;
            let (x0, y0) = (w.size.x, w.size.y);

            h.move_to(from);
            h.frame(&mut w); // hover
            h.button(from, true);
            h.frame(&mut w); // press — zero delta
            h.move_to(to);
            h.frame(&mut w); // the CROSS-AXIS jitter: this band consumes none of it
            h.button(to, false);
            h.frame(&mut w); // release
            h.frames(&mut w, 4);

            assert!(
                !w.user_sized,
                "band {mask} consumes none of {jitter:?}, so it must not latch the window"
            );
            assert!(!h.scrolled.get(), "...and the body must not be told to bound itself");
            assert!(
                (w.size.x - x0).abs() < 2.0 && (w.size.y - y0).abs() < 2.0,
                "...and nothing was resized: {x0}x{y0} -> {}x{}",
                w.size.x,
                w.size.y
            );

            // NON-VACUITY: the SAME band on the SAME window latches on an in-axis move, so the
            // negative above is about the axis rather than about a band that never fired.
            drag_band(&mut h, &mut w, mask, along);
            h.frames(&mut w, 4);
            assert!(w.user_sized, "band {mask} must still latch on an in-axis drag of {along:?}");
        }
    }

    #[test]
    fn an_untouched_tool_window_still_takes_its_size_from_egui() {
        // The latch's NO-OP half, and the reason it is a latch. Nine tool kinds open at 560x400
        // and several bodies are naturally bigger, so a window pinned from frame one would open
        // into scrollbars. Until a band is grabbed the window must behave exactly as it did before
        // this change: unpinned, sized by egui, growing to fit.
        //
        // MUTATION that must redden this: make `pinning`/`pinning_now` true for every tool window
        // from frame one (`|| !fills` in place of `|| w.user_sized`). `w.size` is then never read
        // back and stays at the spawn size.
        let mut h = Harness::tool(vec2(620.0, 440.0));
        let spawn = vec2(400.0, 300.0);
        let w = settled_tool(&mut h, WinKind::News, pos2(300.0, 200.0), spawn);

        assert!(!w.user_sized, "nothing has been dragged, so nothing has latched");
        assert!(!h.scrolled.get(), "an unlatched body is never told to bound itself");
        assert!(
            w.size.y > spawn.y,
            "egui must still grow the window to its taller content: {} <= {}",
            w.size.y,
            spawn.y
        );
        assert!(w.size.x > spawn.x, "...and to its wider content: {} <= {}", w.size.x, spawn.x);
    }

    #[test]
    fn a_bare_click_on_a_band_does_not_latch_the_window() {
        // A `Sense::drag()` widget is `dragged()` from the PRESS frame, where `drag_delta()` is
        // ZERO (`egui-0.36.1/src/interaction.rs`: "This widget is just sensitive to drags, so we
        // can mark it as dragged right away"). Latching on `Some(..)` alone therefore turned ONE
        // accidental click on a tool window's 8pt edge into a permanent conversion to `fixed_size`
        // + scrolling for the rest of the session, with no way back. Every other test here DRAGS,
        // so none of them can tell a press from a press-and-move.
        //
        // MUTATION that must redden this: drop the per-axis NON-ZERO tests from the latch guard —
        // `let moved_the_edge = mask & (1 | 2 | 8) != 0;` — so the press frame's ZERO delta
        // latches. ⚠ This said "drop `&& d != egui::Vec2::ZERO` from the latch guard", which was
        // a leftover: that guard was rewritten to `moved_the_edge` and the string appears nowhere
        // in production, so the only instruction telling the next author how to prove this gate
        // was UNPERFORMABLE. Its sibling
        // (`cross_axis_jitter_on_a_band_does_not_latch_the_window`) names the same text as a
        // RESTORE, which is what it is — the old code. The re-worded mutation was RUN: it reddens
        // here with "a bare click must NOT latch the window" (and, expectedly, reddens that
        // sibling and `the_bottom_band_stays_inside_a_window_shorter_than_its_box` too).
        let mut h = Harness::tool(vec2(560.0, 520.0));
        let mut w = settled_tool(&mut h, WinKind::Trade, pos2(240.0, 140.0), vec2(600.0, 420.0));
        let at = h.band(&w, 8).expect("the bottom band must be allocated").center();

        h.move_to(at);
        h.frame(&mut w); // hover
        h.button(at, true);
        h.frame(&mut w); // press — `dragged()`, zero delta
        h.button(at, false);
        h.frame(&mut w); // release, pointer never moved
        h.frames(&mut w, 4);

        assert!(!w.user_sized, "a bare click must NOT latch the window");
        assert!(!h.scrolled.get(), "...and the body must not be told to bound itself");
    }

    #[test]
    fn a_press_and_a_move_coalesced_into_one_frame_keeps_the_delta_they_carried() {
        // `pinning` was captured BEFORE the resize block, so on the very frame a window latched
        // the end-of-frame read-back still ran and overwrote the delta just applied with egui's
        // pre-drag rect. With the latch moved onto MOVEMENT that frame ALWAYS carries a non-zero
        // delta, so this stopped being theoretical: a press and a move coalescing into one frame's
        // `RawInput` is routine with a high-polling mouse.
        //
        // ⚠ The move stays INSIDE the 8pt band on purpose: egui hit-tests with `interact_pos`,
        // which every pointer event in the frame updates, so a coalesced move that left the band
        // would never press the band at all.
        //
        // MUTATION that must redden this: use the pre-frame `pinning` for the read-back again
        // (`if !pinning` in place of `if !pinning_now`).
        let mut h = Harness::tool(vec2(560.0, 520.0));
        let mut w = settled_tool(&mut h, WinKind::Options, pos2(240.0, 140.0), vec2(600.0, 420.0));
        let band = h.band(&w, 8).expect("the bottom band must be allocated");
        let from = pos2(band.center().x, band.min.y + 1.0);
        let to = pos2(from.x, from.y + 6.0);

        h.move_to(from);
        h.frame(&mut w); // hover, so the press frame is the FIRST frame with a button down
        let h0 = w.size.y;
        h.press_and_move(from, to);
        h.frame(&mut w); // press AND move, one `RawInput`
        h.button(to, false);
        h.frame(&mut w); // release
        h.frames(&mut w, 4);

        assert!(w.user_sized, "a coalesced press+move is a movement, so it latches");
        assert!(
            (w.size.y - (h0 + 6.0)).abs() < 1.0,
            "the coalesced frame's delta must survive: {h0} -> {} (wanted {})",
            w.size.y,
            h0 + 6.0
        );
        assert!(
            (h.painted(&w).height() - w.size.y).abs() < 2.0,
            "...and the PAINTED window must agree: {} vs {}",
            h.painted(&w).height(),
            w.size.y
        );
    }

    #[test]
    fn the_bodys_id_is_stable_across_the_latch() {
        // egui keys widget state on `Ui::id`, so a `ScrollArea` INSERTED when the latch flips
        // changes the id of the whole tool body and discards every auto-id-keyed piece of state
        // inside it the first time a user drags an edge: the Studio code editor's cursor,
        // selection and UNDO STACK, every inner scroll offset, every `CollapsingState`, a
        // half-typed field in the Connections editor. `BodyBounds::show` therefore builds the
        // container UNCONDITIONALLY and toggles only its sizing.
        //
        // MUTATION that must redden this: spell `BodyBounds::show` as
        // `if self.scrolls { ScrollArea::both().auto_shrink(false).show(ui, body).inner } else {
        // body(ui) }` — the pre-latch id is then the Frame's child, not the scroll area's.
        let mut h = Harness::tool(vec2(560.0, 520.0));
        let mut w = settled_tool(&mut h, WinKind::Studio, pos2(240.0, 140.0), vec2(600.0, 420.0));
        let before = h.body_id.get().expect("the body was drawn");
        assert!(!h.scrolled.get(), "the fixture must start UNLATCHED for this to mean anything");

        drag_band(&mut h, &mut w, 8, vec2(0.0, -120.0));
        h.frames(&mut w, 4);

        assert!(h.scrolled.get(), "the fixture must have latched for this to mean anything");
        assert_eq!(
            h.body_id.get().expect("the body was drawn"),
            before,
            "the body's `Ui::id` must not change when the window latches — every auto-id widget \
             inside it (text cursors, undo stacks, collapsing headers, inner scroll offsets) is \
             keyed on it"
        );
    }

    #[test]
    fn the_side_bands_clear_the_scrollbars_and_the_bottom_band_overlaps_by_the_measured_2pt() {
        // The band-vs-scrollbar geometry, on a LATCHED window whose body overflows BOTH ways so
        // both floating bars are live. This is also the only test that drags the RIGHT band
        // (mask 2).
        //
        // ⚠ It models the PRODUCTION inset (`TOOL_BODY_MARGIN`), because a harness with no margin
        // is exactly the configuration this geometry says must never ship.
        //
        // MUTATION that must redden this: set `TOOL_BODY_MARGIN`'s `right` to 0 — the vertical
        // bar's strip then lands ON the right band.
        let mut h = Harness::tool(vec2(900.0, 700.0));
        let mut w =
            settled_tool(&mut h, WinKind::Connections, pos2(120.0, 100.0), vec2(600.0, 420.0));

        // Latch and shrink in BOTH axes, so the 900x700 body overflows the window both ways.
        drag_band(&mut h, &mut w, 8, vec2(0.0, -260.0));
        h.frames(&mut w, 4);
        drag_band(&mut h, &mut w, 1, vec2(200.0, 0.0));
        h.frames(&mut w, 4);
        assert!(h.scrolled.get(), "the window must be latched for the bars to exist");

        // THE RIGHT BAND. It must serve the drag even with the vertical bar alongside it.
        let left0 = w.pos.x;
        let right0 = w.pos.x + w.size.x;
        drag_band(&mut h, &mut w, 2, vec2(-90.0, 0.0));
        h.frames(&mut w, 4);
        assert!(
            (w.pos.x + w.size.x - (right0 - 90.0)).abs() < 2.0,
            "the right edge must follow the drag: {right0} -> {}",
            w.pos.x + w.size.x
        );
        assert!((w.pos.x - left0).abs() < 2.0, "a right drag pins the LEFT edge: {}", w.pos.x);

        // THE GEOMETRY. `body_max` is the `ScrollArea`'s `inner_rect`; a floating bar senses over
        // `max_bar_rect`, the outermost `BAR_W` points of it.
        let inner = h.body_max.get();
        let right_band = h.band(&w, 2).expect("the right band must be allocated");
        let bottom_band = h.band(&w, 8).expect("the bottom band must be allocated");

        assert!(
            inner.max.x <= right_band.min.x + 0.01,
            "the vertical bar's strip (out to x={}) must stay INBOARD of the right band (from \
             x={}) — that is what the 8pt side inset buys",
            inner.max.x,
            right_band.min.x
        );
        assert!(
            inner.max.x - BAR_W < right_band.min.x,
            "non-vacuous: the bar strip must actually sit against the band, not off in the middle \
             of the window ({} vs {})",
            inner.max.x - BAR_W,
            right_band.min.x
        );

        // ...and the ACCEPTED 2pt overlap at the bottom, where the inset is 6 rather than 8. The
        // bands are allocated LAST and `hit_test.rs`'s `find_closest_within` breaks a distance tie
        // by taking "the last one = the one on top", so the BAND wins those points and the
        // horizontal bar's grab area is 2pt thinner — never the other way round.
        let overlap = inner.max.y - bottom_band.min.y;
        assert!(
            (overlap - 2.0).abs() < 0.5,
            "the bottom band must overlap the horizontal bar's strip by the measured 2pt (inset 6 \
             vs band 8), not {overlap}"
        );
    }

    #[test]
    fn a_fill_window_never_takes_the_latch() {
        // The safety half. A `fills` kind (Chart/DOM/Polymarket) is app-owned from frame one and
        // its body sizes itself to the available rect — handing it `scrolls == true` would wrap a
        // self-filling body in a ScrollArea and break it. Its band drags must therefore keep
        // working while the latch stays untaken.
        //
        // MUTATIONS that must redden this: drop the `!fills` guard around `w.user_sized = true`
        // (the first half), or drop the `!fills` from `BodyBounds { scrolls: !fills && … }` (the
        // second half — which no latch guard can catch, because `user_sized` is set BY HAND there).
        let mut h = Harness::fill();
        let mut w = WinState::new(
            "chartfix",
            "BTCUSDT",
            "1m",
            WinKind::Chart,
            Rect::from_min_size(pos2(200.0, 150.0), vec2(700.0, 500.0)),
        );
        h.frames(&mut w, 14);

        let width0 = w.size.x;
        drag_band(&mut h, &mut w, 1, vec2(120.0, 0.0));

        // Non-vacuous: prove the band actually fired before believing what it did NOT set.
        assert!(
            w.size.x < width0 - 50.0,
            "the fill window must still resize from its left band: {width0} -> {}",
            w.size.x
        );
        assert!(!w.user_sized, "a fills kind must NEVER take the latch");
        assert!(!h.scrolled.get(), "...and must never be told to scroll its body");

        // ⚠ The assertion above could not fail on its own: `scrolls` is `!fills && user_sized` and
        // the latch guard already holds `user_sized` false, so it restates the line before it. Set
        // the flag BY HAND and the `scrolls` expression is falsified independently of the latch.
        w.user_sized = true;
        h.frame(&mut w);
        assert!(
            !h.scrolled.get(),
            "a `fills` kind must not scroll its body even with `user_sized` set by hand — the \
             `!fills` in `scrolls` is a second, independent guard"
        );
    }

    #[test]
    fn bands_do_not_register_while_maximized_or_pinned() {
        // A band must not fire while the window is maximized (its geometry is the full bounds
        // every frame) nor during the arrange/restore settle (`force_frames > 0`), where the app
        // is still committing a forced rect.
        //
        // MUTATIONS that must redden this: drop `!w.maximized` from `resizable_now` (the first
        // arm), or drop `w.force_frames == 0` (the second).
        //
        // ⚠ Each arm gets its OWN context, reads after several frames in the condition, and then
        // CLEARS the condition and asserts the band comes BACK. Without that second half,
        // `read_response(..).is_none()` is also the answer for a window that was never shown, an
        // id that changed, or a fixture too small for `window_resize_handles`' own size guard —
        // none of which is the thing under test. (`Context::read_response` also falls back to an
        // earlier pass, so a single frame could answer with a band allocated before the condition
        // was set.)
        fn left(h: &Harness, w: &WinState) -> Option<Rect> {
            h.band(w, 1u8)
        }
        fn fixture(id: &str) -> WinState {
            WinState::tool(
                id,
                WinKind::Data,
                Rect::from_min_size(pos2(200.0, 150.0), vec2(600.0, 420.0)),
            )
        }

        // CONTROL — the same fixture with neither guard active DOES allocate a band, so the two
        // negatives below cannot pass vacuously.
        let mut h = Harness::tool(vec2(560.0, 460.0));
        let w = settled_tool(&mut h, WinKind::Data, pos2(200.0, 150.0), vec2(600.0, 420.0));
        assert!(left(&h, &w).is_some(), "control: a settled tool window allocates its left band");

        // MAXIMIZED — and then un-maximized, which must bring the band back.
        let mut h = Harness::tool(vec2(560.0, 460.0));
        let mut w = fixture("maxed");
        w.maximized = true;
        h.frames(&mut w, 14);
        assert!(left(&h, &w).is_none(), "no band may be allocated while maximized");
        w.maximized = false;
        h.frames(&mut w, 14);
        assert!(left(&h, &w).is_some(), "...and the band must return once it is restored");

        // FORCE-PINNED: a freshly-opened window carries `OPEN_PIN_FRAMES`, so three frames in is
        // still mid-settle — and once the pin drains the band must appear.
        let mut h = Harness::tool(vec2(560.0, 460.0));
        let mut w = fixture("pinned");
        h.frames(&mut w, 3);
        assert!(
            w.force_frames > 0,
            "the fixture must still be mid-settle for this arm to mean anything"
        );
        assert!(left(&h, &w).is_none(), "no band may be allocated during a forced-geometry pin");
        h.frames(&mut w, 14);
        assert_eq!(w.force_frames, 0, "the pin must have drained");
        assert!(left(&h, &w).is_some(), "...and the band must appear once it has");
    }
}

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
/// fresh `WinState` is `None` (spot/legacy), and `Option<vike_catalog::AssetClass>` itself
/// round-trips through JSON losslessly (the property `WinSnap::asset_class` relies on).
#[cfg(test)]
mod asset_class_tests {
    use super::*;
    use egui::{Rect, pos2, vec2};
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
    use egui::{Rect, pos2, vec2};

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
