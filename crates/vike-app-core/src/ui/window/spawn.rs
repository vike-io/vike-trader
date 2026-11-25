//! `window::spawn` — the NEW-WINDOW decision: which cascade slot a freshly opened window lands in,
//! what it is titled and seeded with, and which live subscriptions must already exist before it can
//! paint anything at all.
//!
//! Moved down out of `vike-app`'s `main.rs` (`vike-desktop`'s since #1727) for the reason
//! [`crate::ui::startup`] and [`crate::ui::feed_lifecycle`] moved before it: that file's logic is
//! in **no test gate**. `xtask/src/ci/tables/roster.rs`'s `EXCLUDE_FROM_CI` names `vike-desktop`, so the
//! DERIVED roster that both the justfile's `ci_crates` and CI's fast lane run omits it, and the
//! `app-check` job that DOES compile the crate runs its unit tests over
//! `crates/vike-desktop/src/chart_gpu.rs`'s GPU byte-layout pins plus the title-bar and style-menu
//! colour-role tests in `crates/vike-desktop/src/chart_window.rs`, and nothing else (a font test
//! in `main.rs` stood here until the bundled-fonts change deleted it). SIX separate places in that
//! file's frame loop CASCADE a new window, and
//! every one of them re-derived the cascade arithmetic, the [`WindowState`] construction and the
//! market-data subscription by hand.
//!
//! ⚠ SIX is the count of CASCADING sites, not of window-opening sites — the frame loop has two
//! more and they are deliberately NOT here. The `VIKE_TOOL` / `VIKE_TOOLS` QA hooks (both behind
//! `did_initial_arrange`, so both run once) place their windows at the desktop origin with no
//! cascade at all and spend a `tool-{n}` id for EVERY kind, so a Trade window opened that way is
//! `tool-N` where the launcher's is `trade-N`. Folding them into this planner would CHANGE
//! behaviour, and this move may not; the STEP-2 row that used to stand here has since LANDED as
//! `crates/vike-app-core/src/ui/window/initial_arrange.rs`'s `plan_initial_arrange` — a planner of its own,
//! deliberately separate so the `tool-{n}` ids and the origin placement stay byte-identical, with
//! a test comparing the two planners' Trade spawns so the divergence is observed rather than
//! narrated. That module's roster test runs the REAL `live_window_keys` over every QA spawn, the
//! same invariant `every_spawn_ensures_a_series_the_reaper_counts_as_live` holds for the six sites
//! here.
//!
//! **Six hand copies of one decision is the shape this tree keeps paying for**, and these had
//! already drifted apart in two ways — both PINNED here rather than fixed, which is the per-venue
//! capability-map playbook's STEP 1 (declare today's reality, contradictions included; STEP 2 flips
//! one row at a time, behind its own change and its own reddened test):
//!
//!   * Window ▸ New window cascades from `(40, 30)`; the other five sites cascade from `(50, 40)`.
//!     Nothing documented the difference and nothing depends on it, which is exactly why the
//!     tidy-up that unifies them has to redden a test instead of silently moving a window.
//!   * A title-bar CLONE copied its source window's symbol and interval but **not** its `venue`, so
//!     cloning an OKX chart yielded a window pointed at Binance carrying an OKX symbol, subscribed by
//!     nobody. That one was flipped on 2026-11-13, when a chart stopped having a default venue to fall
//!     back to (the owner's ruling, "remove all hardcoded symbols"): a clone keeps its source's venue.
//!     See `a_cloned_window_keeps_its_source_series_venue_included`.
//!
//! **No compiled symbol and no compiled venue** (that ruling): a chart opened with no series named
//! opens on [`default_series`] — the user's newest chart, else the first symbol of their first
//! DataSet that names a venue — or EMPTY, naming nothing, where the badge says how to pick one
//! (`crate::ui::series_follow::ChartSeriesFollowBadge::NoSymbol`) and a series the backend publishes is adopted.
//!
//! **The dangerous half is the subscription, not the geometry.** A window's feed and the key the
//! reaper counts as BACKING that feed are derived in two different places — the `ensure_feed`
//! `(venue, symbol, interval)` returned here, and the per-KIND rule in
//! `crates/vike-app-core/src/ui/feed_lifecycle/mod.rs`'s `live_window_keys` there — and a one-character
//! disagreement between them is not cosmetic:
//! `crates/vike-app-core/src/ui/feed_lifecycle/mod.rs`'s `reap_orphaned_feeds` runs EVERY frame and stops
//! any `spawned` key no window backs. A spawn's ensure is a ONE-SHOT at creation time, never
//! re-asserted per frame, so a mismatch kills the stream on the frame AFTER the window opens and
//! nothing ever re-requests it: a dead chart with no error anywhere. Nothing checked that the six
//! sites agreed with the reaper.
//! `every_spawn_ensures_a_series_the_reaper_counts_as_live` now does, by running the REAL
//! `live_window_keys` over the REAL window this planner returns, never over a restatement of its
//! rule.
//!
//! **Data in, decisions out.** [`plan_window_spawn`] is pure: the desktop origin, the window counter, the
//! size a Trade window opens at (measured in the look by [`trade_open_size`], since a planner holds
//! no type), the [`default_series`] the shell resolved and one [`WindowSpawnRequest`] in; a [`WindowSpawnPlan`] — the window, its feed and the Data
//! Manager pre-seed, as VALUES — out. It never touches `App`, opens a socket, or reads the
//! environment. The cockpit's `VIKE_POLY_COCKPIT_TOKEN` read stays in the binary and arrives as an
//! already-read seed string (the same `Injected` shape [`crate::ui::startup`] uses), so every
//! `VIKE_*` settings-registry row stays classified as the binary's (`vike-desktop` /
//! `Layer::Binary` today).
//! `crates/vike-desktop/src/main.rs`'s `apply_spawn` applies the plan in the order documented on
//! [`WindowSpawnPlan`], because a spawn's ORDER is load-bearing: every site that subscribed anything
//! subscribed BEFORE publishing its window.

use crate::ui::startup::SeriesSpec;
use crate::ui::window::{WindowKind, WindowState};
use crate::ui::windows::{DataManagerDestination, OptionsTab};
use vike_data::datasets::{SymbolSet, provider_names_a_venue};
use vike_ui_theme::value::workspace;

/// The interval an EMPTY chart starts at: a resolution, not a series. A pick or an adoption replaces
/// it with the series' own.
pub(crate) const EMPTY_CHART_INTERVAL: &str = "1m";

/// **The series a chart opened with no series named starts on — never a compiled one** (the owner's
/// ruling of 2026-11-13, "remove all hardcoded symbols"): the user's NEWEST chart that names a
/// symbol (its venue, symbol, interval and class: the workspace state that already persists), else
/// the first symbol of the first DataSet whose provider names a venue (the server's `symbol_set`
/// rows, on the set's interval), else `None`: the chart opens EMPTY and says how to pick one.
#[must_use]
pub fn default_series(wins: &[WindowState], sets: &[SymbolSet]) -> Option<SeriesSpec> {
    if let Some(w) = wins.iter().rev().find(|w| w.kind == WindowKind::Chart && !w.symbol.is_empty())
    {
        return Some(SeriesSpec {
            venue: w.venue.clone(),
            symbol: w.symbol.clone(),
            interval: w.interval.clone(),
            asset_class: w.asset_class,
        });
    }
    sets.iter().find_map(|s| {
        let symbol = s.symbols.first()?;
        (provider_names_a_venue(&s.provider) && !s.interval.trim().is_empty()).then(|| SeriesSpec {
            venue: s.provider.trim().to_string(),
            symbol: symbol.clone(),
            interval: s.interval.trim().to_string(),
            asset_class: None,
        })
    })
}

/// One `(venue, symbol)` a chart's pickers offer before the instrument list has loaded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuickPick {
    pub venue: String,
    pub symbol: String,
}

/// **What a chart's symbol and Compare pickers offer while no instrument list has loaded — the
/// user's own symbols, never a compiled list**: every open chart's `(venue, symbol)`, newest first,
/// then every symbol of every DataSet whose provider names a venue, each pair once. Empty on a fresh
/// install, where the picker says how to get a list instead.
#[must_use]
pub fn quick_picks(wins: &[WindowState], sets: &[SymbolSet]) -> Vec<QuickPick> {
    let charts = wins
        .iter()
        .rev()
        .filter(|w| w.kind == WindowKind::Chart && !w.symbol.is_empty() && !w.venue.is_empty())
        .map(|w| QuickPick { venue: w.venue.clone(), symbol: w.symbol.clone() });
    let listed = sets.iter().filter(|s| provider_names_a_venue(&s.provider)).flat_map(|s| {
        s.symbols.iter().map(|symbol| QuickPick {
            venue: s.provider.trim().to_string(),
            symbol: symbol.clone(),
        })
    });
    let mut out: Vec<QuickPick> = Vec::new();
    for p in charts.chain(listed) {
        if !out.contains(&p) {
            out.push(p);
        }
    }
    out
}

/// A CHART window on `series`, venue and class included, or an EMPTY chart (no venue, no symbol, at
/// [`EMPTY_CHART_INTERVAL`]) when `series` is `None`. `WindowState::new` names no venue, so the
/// venue is assigned and THEN the title redone.
///
/// ⚠ A window opened ON a series is a CHOICE ([`WindowState::series_pinned`]), however empty or
/// venue-less that series is: only an EMPTY open may adopt what the backend publishes. Unpinned, a
/// Data Manager open whose history was still loading was swapped onto the node's series (2026-11-16).
pub(crate) fn chart_on(
    id_source: &str,
    series: Option<&SeriesSpec>,
    rect: egui::Rect,
) -> WindowState {
    let (venue, symbol, interval) = series.map_or(("", "", EMPTY_CHART_INTERVAL), |s| {
        (s.venue.as_str(), s.symbol.as_str(), s.interval.as_str())
    });
    let mut w = WindowState::new(id_source, symbol, interval, WindowKind::Chart, rect);
    w.venue = venue.to_string();
    w.series_pinned = series.is_some();
    w.asset_class = series.and_then(|s| s.asset_class);
    w.retitle();
    w
}

/// The feed a chart on `series` must have before it is published: none for a series that names no
/// venue, which nothing can route.
fn feed_for(series: Option<&SeriesSpec>) -> Option<SeriesSpec> {
    series.filter(|s| !s.venue.is_empty() && !s.symbol.is_empty()).cloned()
}

// The cascade's step and origins, and the size each window kind opens at, are rows of
// `ui-theme.toml` (group `workspace`), read as `workspace::CASCADE_STEP`, `workspace::CASCADE_ORIGIN`
// (slot 0's offset from the desktop's top-left for five of the six spawn sites),
// `workspace::CASCADE_ORIGIN_NEW_WINDOW` (the sixth: Window ▸ New window has always started ten
// points further up and left, for no recorded reason — PINNED, not endorsed, see the module doc),
// one `workspace::*_WINDOW_SIZE` row per tool kind ([`tool_window_size`]), `workspace::TOOL_WINDOW_SIZE`
// (the floor under every one of them) and the chart and cockpit sizes.
//
// ⚠ `TOOL_WINDOW_SIZE` is a row, not a private number, because a headless test has to drive a
// window at a shipped size without writing the numbers down a second time
// (`crates/vike-app-core/tests/data_manager_credentials_panel.rs` is the caller). The reason is a
// measured one:
// every earlier test of the Connections credential panel drove it at 1000pt, which only ever
// reaches the two-column arm, while the window OPENED at 560 — so the arm the shipped window actually
// rendered had never been drawn by a test, and the edit control in it was unclickable for a whole
// release. The Data Manager opens wider now (`workspace::DATA_WINDOW_SIZE`); that test keeps the
// floor's width, the narrowest any tool window opens at, so the one-column arm stays drawn.

/// **The size a tool window of `kind` opens at — the one per-kind table.** Since 2026-11-16 a window
/// KEEPS its size (`window::show_window` pins it; its body wraps or scrolls inside), so this is
/// the size an operator sees until they drag an edge; `show_window` caps it at the arena. The
/// `fills` kinds (Chart, Trade, Polymarket) size themselves at their own spawn arms.
///
/// Each row is the size that kind's window SETTLED at when egui still sized it to its body,
/// MEASURED (2026-11-16) over typical content on a 1920 x 1080 screen and rounded up to ten points,
/// never under `workspace::TOOL_WINDOW_SIZE` and never over 80% of that screen's arena on either
/// axis; two are rulings instead: News (its measured size was the bug, one-line headlines grew it to
/// the whole screen) and the Calendar (its seven day cards, `fix/calendar-layout`).
/// `crates/vike-app-core/tests/screens/tool_window_fit.rs` draws each kind's real body at its row
/// and fails when the body overflows the window.
pub fn tool_window_size(kind: WindowKind) -> egui::Vec2 {
    match kind {
        WindowKind::Account => workspace::ACCOUNT_WINDOW_SIZE,
        WindowKind::Options => workspace::OPTIONS_WINDOW_SIZE,
        WindowKind::News => workspace::NEWS_WINDOW_SIZE,
        WindowKind::Calendar => workspace::CALENDAR_WINDOW_SIZE,
        WindowKind::Data => workspace::DATA_WINDOW_SIZE,
        WindowKind::Studio => workspace::STUDIO_WINDOW_SIZE,
        WindowKind::Settings => workspace::SETTINGS_WINDOW_SIZE,
        WindowKind::Chart | WindowKind::Trade | WindowKind::Polymarket => {
            workspace::TOOL_WINDOW_SIZE
        }
    }
}

/// How many cascade slots exist before the walk WRAPS and a window lands exactly on top of the one
/// eight opens ago. Today's behaviour, pinned by
/// `each_spawn_site_cascades_from_its_own_pinned_origin_and_size` — the two windows still carry
/// distinct `egui::Id`s, so egui keeps their state apart and the operator can drag one off the
/// other; only the opening rect collides.
const CASCADE_SLOTS: u32 = 8;

/// Which of the GUI's six CASCADING window-opening paths is asking, carrying only what that path
/// knows. (The two QA-hook arms that open a window without cascading are not here — module doc.)
///
/// One variant per call site in `crates/vike-desktop/src/main.rs`'s frame loop, deliberately NOT
/// collapsed onto a common shape: the sites genuinely disagree about geometry and about what they
/// subscribe, and a merged variant would hide which of them a future edit is changing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WindowSpawnRequest {
    /// The command palette's Enter, carrying the raw text AS TYPED — the trim / upper-case ladder
    /// is [`plan_window_spawn`]'s, so it is tested rather than inline in a GUI file. The typed symbol opens
    /// on the default series' venue (none when there is none); no quote currency is appended.
    Palette { raw: String },
    /// Window ▸ New window: a chart on the default series, or an empty one.
    NewWindowChart,
    /// A launcher icon or the Window menu opening a window of a given kind. `polymarket_seed_token` is
    /// read ONLY by the [`WindowKind::Polymarket`] arm; every other kind ignores it, which
    /// `the_cockpit_carries_its_seed_token_and_no_other_kind_reads_it` pins so a future arm cannot start
    /// quietly depending on it.
    ///
    /// `data_destination_seed` is the same shape, for [`WindowKind::Data`] alone: a freshly spawned Data
    /// Manager window normally opens on [`crate::ui::windows::DataManagerDestination::default`] (Overview),
    /// and a caller that wants it to land somewhere specific instead — the Trade ticket's Connect
    /// click wants `Gateway` — passes `Some` here rather than reopening `App::apply_spawn` to
    /// poke the result afterward. Every other kind ignores it, same discipline as the Polymarket
    /// seed above.
    Kind {
        kind: WindowKind,
        polymarket_seed_token: String,
        data_destination_seed: Option<DataManagerDestination>,
    },
    /// The chart title-bar's clone button, carrying the SOURCE window's venue, symbol and
    /// interval. The clone keeps all three (the module doc's flipped pin).
    CloneWindow { venue: String, symbol: String, interval: String },
    /// The Data Manager's Symbols tab, "Test symbol": the symbol on the DataSet's own provider (no
    /// venue for `Auto`) and interval.
    TestSymbol { series: SeriesSpec },
    /// Data Manager ▸ Stored ▸ "Open in chart". `interval` is `None` for a tick-only series
    /// (quote/trade), which has no chart timeframe of its own.
    StoredOpen { venue: String, symbol: String, interval: Option<String> },
    /// The Greeks launcher: an Options window on its Portfolio Greeks tab
    /// (`WindowSpawnPlan::options_tab_seed`), opened where and as large as any Options window.
    PortfolioGreeks,
}

/// What a launcher opens: a window of its kind on that window's first page — or, for the Greeks
/// launcher, an Options window on its Portfolio Greeks tab. The Greeks window became that tab on
/// 2026-11-17 and its launcher kept its icon and its meaning; the retired kind's name is read by
/// nothing else (no saved layout holds a tool window, and `VIKE_TOOL=greeks` falls back like any
/// unknown slug).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowSpawnTarget {
    Window(WindowKind),
    PortfolioGreeks,
}

impl WindowSpawnTarget {
    /// The spawn a click on a launcher of this target asks for. `polymarket_cockpit_token` is the
    /// shell's `VIKE_POLY_COCKPIT_TOKEN`, read ONLY for a Polymarket window and run through
    /// [`crate::ui::startup::polymarket_cockpit_seed_token`], the ladder every cockpit seeding path
    /// shares; every other target ignores it.
    pub fn request(self, polymarket_cockpit_token: Option<&str>) -> WindowSpawnRequest {
        match self {
            WindowSpawnTarget::Window(kind) => WindowSpawnRequest::Kind {
                kind,
                polymarket_seed_token: if kind == WindowKind::Polymarket {
                    crate::ui::startup::polymarket_cockpit_seed_token(polymarket_cockpit_token)
                } else {
                    String::new()
                },
                data_destination_seed: None,
            },
            WindowSpawnTarget::PortfolioGreeks => WindowSpawnRequest::PortfolioGreeks,
        }
    }
}

/// What one spawn produces — and, critically, **in what order the caller must apply it**.
///
/// The order below is the order `crates/vike-desktop/src/main.rs` performed these steps at every site
/// before the move, and it is reproduced rather than tidied:
///
/// 1. [`next_window_number`](Self::next_window_number) — adopt the advanced counter.
/// 2. [`ensure_feed`](Self::ensure_feed) — `App::ensure_feed_on` the spec, BEFORE the window is
///    published (every site that subscribed anything subscribed first).
/// 3. [`window`](Self::window) — push the window.
///
/// No derives: [`WindowState`] carries live egui dialog/pane state and is neither `Clone` nor `Debug`.
/// Built once, consumed once — the same shape [`crate::ui::startup`]'s `StartupLayout` takes, and for
/// the same reason.
pub struct WindowSpawnPlan {
    /// The window to publish. `None` only for a blank palette submit, the one request that can
    /// decline to spawn — in which case `next_window_number` is unchanged and no window id is burned.
    pub window: Option<WindowState>,
    /// The bar series the new window will read, as `App::ensure_feed_on`'s argument list.
    pub ensure_feed: Option<SeriesSpec>,
    /// The Data Manager window — paired with its freshly minted id — whose `WindowView::data_destination`
    /// the caller must pre-seed before the first frame, the same way
    /// `crates/vike-desktop/src/main.rs`'s startup planner pre-seeds a restored/QA-captured
    /// window's destination (`app.window_views.entry(wid).or_default().data_destination = destination`). `None`
    /// for every request that did not ask for one (`WindowSpawnRequest::Kind`'s `data_destination_seed` is the
    /// only source), which leaves the window on its default destination exactly as before this
    /// field existed.
    pub data_destination_seed: Option<(egui::Id, DataManagerDestination)>,
    /// The Options window — paired with its freshly minted id — whose `WindowView::options_tab` the
    /// caller must pre-seed before the first frame, exactly as [`Self::data_destination_seed`].
    /// `Some` only for [`WindowSpawnRequest::PortfolioGreeks`]; every other Options window opens on the
    /// Chain tab.
    pub(crate) options_tab_seed: Option<(egui::Id, OptionsTab)>,
    /// `App::next_window_number` after this spawn.
    pub next_window_number: u32,
}

impl WindowSpawnPlan {
    /// Write the page the new window opens on — [`Self::data_destination_seed`],
    /// `Self::options_tab_seed` — into its view state in `window_views` (`App::window_views`, keyed by
    /// window id), where the window loop's `entry(..).or_default()` finds it on the first frame.
    pub fn seed_window_views(
        &self,
        window_views: &mut std::collections::HashMap<egui::Id, crate::ui::windows::WindowView>,
    ) {
        if let Some((wid, destination)) = self.data_destination_seed {
            window_views.entry(wid).or_default().data_destination = destination;
        }
        if let Some((wid, tab)) = self.options_tab_seed {
            window_views.entry(wid).or_default().options_tab = tab;
        }
    }
}

/// The size a new Trade window opens at in `context`'s look: the view it opens in, made tall enough to
/// show the full ticket's whole form (`vike_order_entry::trade_window::layout::window_size`, which measures the
/// type, so a frame's context is needed and [`plan_window_spawn`] takes the answer as data).
///
/// # Panics
///
/// With a context that has run no frame yet: egui has no fonts until one has, which is why the QA
/// capture arm (`crate::ui::startup`, planned before the first frame) opens at the spec's size.
pub fn trade_open_size(context: &egui::Context) -> egui::Vec2 {
    vike_order_entry::trade_window::layout::window_size(
        vike_order_entry::trade_window::TradeWindowState::default().view,
        context,
    )
}

/// Decide one window spawn. Pure: same inputs ⇒ same [`WindowSpawnPlan`].
///
/// `desktop_min` is the window arena's top-left (`App::desktop`'s `min`) and `next_window_number` the
/// shared window counter, which supplies BOTH the cascade slot and the `egui::Id` seed — the id
/// PREFIX differs per family (`chart-` / `trade-` / `polymarket-` / `tool-`) but the counter does not, so
/// the prefix is decoration and the counter is the whole of what keeps two windows apart.
/// `trade_size` is the size a Trade window opens at ([`trade_open_size`], measured by the caller:
/// it follows the look, and this planner holds no type to measure with). `default` is
/// [`default_series`] over the shell's windows and DataSets: what a chart opened with no series
/// named opens on, and the venue a typed palette symbol opens on.
pub fn plan_window_spawn(
    req: WindowSpawnRequest,
    desktop_min: egui::Pos2,
    next_window_number: u32,
    trade_size: egui::Vec2,
    default: Option<&SeriesSpec>,
) -> WindowSpawnPlan {
    let n = next_window_number;
    let origin = workspace::CASCADE_ORIGIN;
    let mut plan = WindowSpawnPlan {
        window: None,
        ensure_feed: None,
        data_destination_seed: None,
        options_tab_seed: None,
        next_window_number: n + 1,
    };
    match req {
        WindowSpawnRequest::Palette { raw } => {
            let raw = raw.trim().to_uppercase();
            if raw.is_empty() {
                // Nothing typed: no window, and no cascade slot burned either. The counter IS the
                // id seed, so spending one here would leave a permanent hole in the id sequence
                // for every stray Enter in an empty palette.
                plan.next_window_number = n;
                return plan;
            }
            // The symbol AS TYPED (no quote currency is guessed onto it), on the venue of the
            // default series; with none, no venue, so no feed — the chart says what is missing.
            let series = SeriesSpec {
                venue: default.map(|d| d.venue.clone()).unwrap_or_default(),
                symbol: raw,
                interval: EMPTY_CHART_INTERVAL.to_string(),
                asset_class: None,
            };
            plan.ensure_feed = feed_for(Some(&series));
            plan.window = Some(chart_on(
                &format!("chart-{n}"),
                Some(&series),
                cascade_rect(desktop_min, n, origin, workspace::CHART_WINDOW_SIZE),
            ));
        }
        WindowSpawnRequest::NewWindowChart => {
            plan.ensure_feed = feed_for(default);
            plan.window = Some(chart_on(
                &format!("chart-{n}"),
                default,
                cascade_rect(
                    desktop_min,
                    n,
                    workspace::CASCADE_ORIGIN_NEW_WINDOW,
                    workspace::NEW_WINDOW_CHART_SIZE,
                ),
            ));
        }
        // An EXHAUSTIVE match, no `_` arm: the inline `else` this replaced meant a new `WindowKind`
        // silently inherited the generic tool geometry, and a kind that needs a symbol or a stream
        // (as Trade and Polymarket both do) would have opened blank with nothing to say so. A new
        // variant now fails to COMPILE here until somebody classifies it.
        WindowSpawnRequest::Kind { kind, polymarket_seed_token, data_destination_seed } => {
            match kind {
                WindowKind::Chart => {
                    plan.ensure_feed = feed_for(default);
                    plan.window = Some(chart_on(
                        &format!("chart-{n}"),
                        default,
                        cascade_rect(desktop_min, n, origin, workspace::CHART_WINDOW_SIZE),
                    ));
                }
                WindowKind::Trade => {
                    // The ladder, the ticket and the status strip at the side-panel size, as tall as
                    // the whole form needs (`trade_size`). No symbol yet: the glue seeds the last-used
                    // instrument, or the backend's primary market, on the first frame (spec §3.11). No
                    // bar feed (Ruling R6); the depth stream is requested by the window loop once the
                    // symbol is known.
                    let mut ws = WindowState::tool(
                        &format!("trade-{n}"),
                        kind,
                        cascade_rect(desktop_min, n, origin, trade_size),
                    );
                    ws.symbol = String::new();
                    ws.title = "Trade".to_string();
                    plan.window = Some(ws);
                }
                WindowKind::Polymarket => {
                    // The cockpit is narrow + tall. `symbol` is the YES-outcome token-id:
                    // the caller seeds it from VIKE_POLY_COCKPIT_TOKEN when supplied, else the
                    // placeholder. The window loop's `ensure_polymarket_book` subscribes whichever token
                    // ends up here.
                    let mut ws = WindowState::tool(
                        &format!("poly-{n}"),
                        kind,
                        cascade_rect(desktop_min, n, origin, workspace::COCKPIT_WINDOW_SIZE),
                    );
                    ws.symbol = polymarket_seed_token;
                    ws.title = "Polymarket · Cockpit".to_string();
                    plan.window = Some(ws);
                }
                WindowKind::Data => {
                    let ws = WindowState::tool(
                        &format!("tool-{n}"),
                        kind,
                        cascade_rect(desktop_min, n, origin, tool_window_size(kind)),
                    );
                    // Same shape as the Polymarket seed above: carried out as a VALUE
                    // (`WindowSpawnPlan::data_destination_seed`) because this planner never touches `App` — the
                    // caller is the one holding `App::window_views` to pre-seed into.
                    if let Some(destination) = data_destination_seed {
                        plan.data_destination_seed = Some((ws.id, destination));
                    }
                    plan.window = Some(ws);
                }
                WindowKind::Account
                | WindowKind::Options
                | WindowKind::News
                | WindowKind::Calendar
                | WindowKind::Studio
                | WindowKind::Settings => {
                    plan.window = Some(WindowState::tool(
                        &format!("tool-{n}"),
                        kind,
                        cascade_rect(desktop_min, n, origin, tool_window_size(kind)),
                    ));
                }
            }
        }
        WindowSpawnRequest::CloneWindow { venue, symbol, interval } => {
            // No feed: the clone keeps its source's venue, symbol and interval, so its key IS the
            // source's and it reads the stream the source already pays for; a second `ensure`
            // would be pure duplication. (Until the source's venue was kept, a non-Binance clone
            // had the un-namespaced key, was backed by no subscription and painted nothing.)
            let series = SeriesSpec { venue, symbol, interval, asset_class: None };
            plan.window = Some(chart_on(
                &format!("chart-{n}"),
                Some(&series),
                cascade_rect(desktop_min, n, origin, workspace::CLONE_CHART_SIZE),
            ));
        }
        WindowSpawnRequest::TestSymbol { series } => {
            plan.ensure_feed = feed_for(Some(&series));
            plan.window = Some(chart_on(
                &format!("chart-{n}"),
                Some(&series),
                cascade_rect(desktop_min, n, origin, workspace::TEST_CHART_SIZE),
            ));
        }
        WindowSpawnRequest::StoredOpen { venue, symbol, interval } => {
            // Bar series open on their own interval; a tick-only series (quote/trade) has no chart
            // timeframe of its own, so it falls back to "1m" — the venue's kline feed for that
            // symbol, the same default as every other chart-open path. The reopen carries no
            // asset-class information (spot/legacy routing), byte-identical to before the feature.
            let iv = interval.unwrap_or_else(|| "1m".to_string());
            let series = SeriesSpec { venue, symbol, interval: iv, asset_class: None };
            plan.ensure_feed = Some(series.clone());
            plan.window = Some(chart_on(
                &format!("chart-{n}"),
                Some(&series),
                cascade_rect(desktop_min, n, origin, workspace::STORED_CHART_SIZE),
            ));
        }
        WindowSpawnRequest::PortfolioGreeks => {
            let kind = WindowKind::Options;
            let ws = WindowState::tool(
                &format!("tool-{n}"),
                kind,
                cascade_rect(desktop_min, n, origin, tool_window_size(kind)),
            );
            plan.options_tab_seed = Some((ws.id, OptionsTab::PortfolioGreeks));
            plan.window = Some(ws);
        }
    }
    plan
}

/// **The Studio is ONE window** (the desktop holds one Studio session): a request to open it while a
/// Studio window exists — open, minimized, closed or behind another — brings THAT one back instead of
/// stacking a second. Returns its id for the caller to raise (`super::arrange::raise`), which
/// also draws it over a maximized window (`super::arrange::hidden_by_maximized`) and leaves that one
/// maximized (the owner's pass of 2026-11-21); `None` for any other request or while there is no
/// Studio window: then the request spawns as before. The live desktop held three Studio windows, each
/// opened behind a maximized chart.
pub fn reopen_studio(wins: &mut [WindowState], req: &WindowSpawnRequest) -> Option<egui::Id> {
    if !matches!(req, WindowSpawnRequest::Kind { kind: WindowKind::Studio, .. }) {
        return None;
    }
    let studio = wins.iter_mut().rev().find(|w| w.kind == WindowKind::Studio)?;
    studio.open = true;
    studio.minimized = false;
    Some(studio.id)
}

/// The cascade rect for slot `n`: `origin` is the per-site first-slot offset from the desktop's
/// top-left, `size` the per-site window size.
///
/// The two offsets are summed into ONE `Vec2` before being added to `desktop_min`, exactly as the
/// six inline copies wrote it (`self.desktop.min + egui::vec2(50.0 + off, 40.0 + off)`). Not a
/// style choice — `f32` addition is not associative, and adding `origin` and `off` to the position
/// in two steps could differ in the last bit from what has shipped.
fn cascade_rect(
    desktop_min: egui::Pos2,
    n: u32,
    origin: egui::Vec2,
    size: egui::Vec2,
) -> egui::Rect {
    let off = workspace::CASCADE_STEP * (n % CASCADE_SLOTS) as f32;
    egui::Rect::from_min_size(desktop_min + egui::vec2(origin.x + off, origin.y + off), size)
}

#[path = "spawn_tests.rs"]
#[cfg(test)]
mod spawn_tests;
