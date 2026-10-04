//! `window_spawn` — the NEW-WINDOW decision: which cascade slot a freshly opened window lands in,
//! what it is titled and seeded with, and which live subscriptions must already exist before it can
//! paint anything at all.
//!
//! Moved down out of `vike-app`'s `main.rs` (`vike-desktop`'s since #1727) for the reason
//! [`crate::ui::startup`] and [`crate::ui::feed_lifecycle`] moved before it: that file's logic is
//! in **no test gate**. `xtask/src/ci/tables.rs`'s `EXCLUDE_FROM_CI` names `vike-desktop`, so the
//! DERIVED roster that both the justfile's `ci_crates` and CI's fast lane run omits it, and the
//! `app-check` job that DOES compile the crate runs its unit tests over
//! `crates/vike-desktop/src/chart_gpu.rs`'s GPU byte-layout pins plus the title-bar and style-menu
//! colour-role tests in `crates/vike-desktop/src/chart_window.rs`, and nothing else (a font test
//! in `main.rs` stood here until the bundled-fonts change deleted it). SIX separate places in that
//! file's frame loop CASCADE a new window, and
//! every one of them re-derived the cascade arithmetic, the [`WinState`] construction and the
//! market-data subscription by hand.
//!
//! ⚠ SIX is the count of CASCADING sites, not of window-opening sites — the frame loop has two
//! more and they are deliberately NOT here. The `VIKE_TOOL` / `VIKE_TOOLS` QA hooks (both behind
//! `did_initial_arrange`, so both run once) place their windows at the desktop origin with no
//! cascade at all and spend a `tool-{n}` id for EVERY kind, so a Trade window opened that way is
//! `tool-N` where the launcher's is `trade-N`. Folding them into this planner would CHANGE
//! behaviour, and this move may not; the STEP-2 row that used to stand here has since LANDED as
//! `crates/vike-app-core/src/ui/initial_arrange.rs`'s `plan_initial_arrange` — a planner of its own,
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
//!   * A title-bar CLONE copies its source window's symbol and interval but **not** its `venue`, so
//!     cloning an OKX chart yields a window pointed at Binance carrying an OKX symbol — and since a
//!     clone deliberately ensures no feed of its own, that window's key is subscribed by nobody
//!     and it paints nothing. See
//!     `a_cloned_window_keeps_symbol_and_interval_but_loses_its_source_venue`.
//!
//! **The dangerous half is the subscription, not the geometry.** A window's feed and the key the
//! reaper counts as BACKING that feed are derived in two different places — the `ensure_feed`
//! `(venue, symbol, interval)` returned here, and the per-KIND rule in
//! `crates/vike-app-core/src/ui/feed_lifecycle.rs`'s `live_window_keys` there — and a one-character
//! disagreement between them is not cosmetic:
//! `crates/vike-app-core/src/ui/feed_lifecycle.rs`'s `reap_orphaned_feeds` runs EVERY frame and stops
//! any `spawned` key no window backs. A spawn's ensure is a ONE-SHOT at creation time, never
//! re-asserted per frame, so a mismatch kills the stream on the frame AFTER the window opens and
//! nothing ever re-requests it: a dead chart with no error anywhere. Nothing checked that the six
//! sites agreed with the reaper.
//! `every_spawn_ensures_a_series_the_reaper_counts_as_live` now does, by running the REAL
//! `live_window_keys` over the REAL window this planner returns, never over a restatement of its
//! rule.
//!
//! **Data in, decisions out.** [`plan_spawn`] is pure: the desktop origin, the window counter, the
//! size a Trade window opens at (measured in the look by [`trade_open_size`], since a planner holds
//! no type) and one [`SpawnRequest`] in; a [`SpawnPlan`] — the window, the two subscriptions and
//! the background resolve, as VALUES — out. It never touches `App`, opens a socket, or reads the
//! environment. The cockpit's `VIKE_POLY_COCKPIT_TOKEN` read stays in the binary and arrives as an
//! already-read seed string (the same `Injected` shape [`crate::ui::startup`] uses), so every
//! `VIKE_*` settings-registry row stays classified as the binary's (`vike-desktop` /
//! `Layer::Binary` today).
//! `crates/vike-desktop/src/main.rs`'s `apply_spawn` applies the plan in the order documented on
//! [`SpawnPlan`], because a spawn's ORDER is load-bearing: every site that subscribed anything
//! subscribed BEFORE publishing its window, and the cockpit spawned its Gamma resolver AFTER
//! pushing.

use crate::ui::startup::FeedSpec;
use crate::ui::tool_views::POLY_PLACEHOLDER_TOKEN;
use crate::ui::workspace::{DEFAULT_VENUE, WinKind, WinState};

/// How far each successive window steps down-and-right from the previous one, in points.
const CASCADE_STEP: f32 = 28.0;

/// How many cascade slots exist before the walk WRAPS and a window lands exactly on top of the one
/// eight opens ago. Today's behaviour, pinned by
/// `each_spawn_site_cascades_from_its_own_pinned_origin_and_size` — the two windows still carry
/// distinct `egui::Id`s, so egui keeps their state apart and the operator can drag one off the
/// other; only the opening rect collides.
const CASCADE_SLOTS: u32 = 8;

/// Slot 0's offset from the desktop's top-left, for five of the six spawn sites.
const CASCADE_ORIGIN: egui::Vec2 = egui::vec2(50.0, 40.0);

/// **The size a plain tool window opens at**, Connections included.
///
/// ⚠ `pub` and NAMED because a headless test has to be able to drive the shipped window at the
/// shipped size without writing the numbers down a second time.
/// `crates/vike-app-core/tests/connections_window.rs` is the caller, and the reason is a measured
/// one: every existing test of the Connections credential panel drove it at 1000pt, which only
/// ever reaches the two-column arm, while the window OPENS at 560 — so the arm the shipped window
/// actually renders had never been drawn by a test, and the edit control in it was unclickable for
/// a whole release.
pub const TOOL_WINDOW_SIZE: egui::Vec2 = egui::vec2(560.0, 400.0);

/// The Settings window's first size: a row of four previews (spec §5) and room below them.
pub const SETTINGS_WINDOW_SIZE: egui::Vec2 = egui::vec2(640.0, 600.0);

/// …and for the sixth. Window ▸ New window has always started ten points further up and left, for
/// no recorded reason. PINNED, not endorsed — see the module doc.
const CASCADE_ORIGIN_NEW_WINDOW: egui::Vec2 = egui::vec2(40.0, 30.0);

/// Which of the GUI's six CASCADING window-opening paths is asking, carrying only what that path
/// knows. (The two QA-hook arms that open a window without cascading are not here — module doc.)
///
/// One variant per call site in `crates/vike-desktop/src/main.rs`'s frame loop, deliberately NOT
/// collapsed onto a common shape: the sites genuinely disagree about geometry and about what they
/// subscribe, and a merged variant would hide which of them a future edit is changing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpawnRequest {
    /// The command palette's Enter, carrying the raw text AS TYPED — the trim / upper-case /
    /// quote-currency ladder is [`plan_spawn`]'s, so it is tested rather than inline in a GUI file.
    Palette { raw: String },
    /// Window ▸ New window. The symbol comes from the binary: its `SYMS` round-robin list is also
    /// `crates/vike-desktop/src/chart_window.rs`'s picker fallback, so the list belongs to
    /// `vike-desktop`, not to this planner — only the slot and the construction moved.
    NewWindowChart { symbol: String },
    /// A launcher icon or the Window menu opening a window of a given kind. `poly_seed_token` is
    /// read ONLY by the [`WinKind::Polymarket`] arm; every other kind ignores it, which
    /// `the_cockpit_resolves_only_a_still_placeholder_token` pins so a future arm cannot start
    /// quietly depending on it.
    Kind { kind: WinKind, poly_seed_token: String },
    /// The chart title-bar's clone button, carrying the SOURCE window's venue, symbol and
    /// interval.
    ///
    /// The venue is carried and then DROPPED on the floor — that is the pinned defect the module
    /// doc names. It is carried ANYWAY, and deliberately: with it in the request the STEP-2 fix is
    /// one line inside this CI-gated planner plus a reddened
    /// `a_cloned_window_keeps_symbol_and_interval_but_loses_its_source_venue`, and nothing in the
    /// `vike-desktop` shell moves at all. Without it the fix would have had to reopen the very file
    /// this module exists to empty, and the pin could only ever have asserted that
    /// [`WinState::new`] hardcodes [`DEFAULT_VENUE`] — which is a fact about `WinState`, not about
    /// the clone.
    CloneWindow { venue: String, symbol: String, interval: String },
    /// The Data Manager's Symbols tab, "Test symbol".
    TestSymbol { symbol: String },
    /// Data Manager ▸ Stored ▸ "Open in chart". `interval` is `None` for a tick-only series
    /// (quote/trade), which has no chart timeframe of its own.
    StoredOpen { venue: String, symbol: String, interval: Option<String> },
}

/// What one spawn produces — and, critically, **in what order the caller must apply it**.
///
/// The order below is the order `crates/vike-desktop/src/main.rs` performed these steps at every site
/// before the move, and it is reproduced rather than tidied:
///
/// 1. [`next_win_n`](Self::next_win_n) — adopt the advanced counter.
/// 2. [`ensure_feed`](Self::ensure_feed) — `App::ensure_feed_on` the spec, BEFORE the window is
///    published (every site that subscribed anything subscribed first).
/// 3. [`win`](Self::win) — push the window.
/// 4. [`resolve_poly_token`](Self::resolve_poly_token) — spawn the background Gamma resolver, which
///    the cockpit arm did AFTER its push.
///
/// No derives: [`WinState`] carries live egui dialog/pane state and is neither `Clone` nor `Debug`.
/// Built once, consumed once — the same shape [`crate::ui::startup`]'s `StartupLayout` takes, and for
/// the same reason.
pub struct SpawnPlan {
    /// The window to publish. `None` only for a blank palette submit, the one request that can
    /// decline to spawn — in which case `next_win_n` is unchanged and no window id is burned.
    pub win: Option<WinState>,
    /// The bar series the new window will read, as `App::ensure_feed_on`'s argument list.
    pub ensure_feed: Option<FeedSpec>,
    /// The cockpit window whose YES token is still the placeholder and therefore needs background
    /// Gamma resolution (`App::spawn_poly_token_resolver`).
    pub resolve_poly_token: Option<egui::Id>,
    /// `App::next_win_n` after this spawn.
    pub next_win_n: u32,
}

/// The size a new Trade window opens at in `ctx`'s look: the view it opens in, made tall enough to
/// show the full ticket's whole form (`vike_panels::trade::layout::window_size`, which measures the
/// type, so a frame's context is needed and [`plan_spawn`] takes the answer as data).
///
/// # Panics
///
/// With a context that has run no frame yet: egui has no fonts until one has, which is why the QA
/// capture arm (`crate::ui::startup`, planned before the first frame) opens at the spec's size.
pub fn trade_open_size(ctx: &egui::Context) -> egui::Vec2 {
    vike_panels::trade::layout::window_size(vike_panels::trade::TradeState::default().view, ctx)
}

/// Decide one window spawn. Pure: same inputs ⇒ same [`SpawnPlan`].
///
/// `desktop_min` is the window arena's top-left (`App::desktop`'s `min`) and `next_win_n` the
/// shared window counter, which supplies BOTH the cascade slot and the `egui::Id` seed — the id
/// PREFIX differs per family (`chart-` / `trade-` / `poly-` / `tool-`) but the counter does not, so
/// the prefix is decoration and the counter is the whole of what keeps two windows apart.
/// `trade_size` is the size a Trade window opens at ([`trade_open_size`], measured by the caller:
/// it follows the look, and this planner holds no type to measure with).
pub fn plan_spawn(
    req: SpawnRequest,
    desktop_min: egui::Pos2,
    next_win_n: u32,
    trade_size: egui::Vec2,
) -> SpawnPlan {
    let n = next_win_n;
    let mut plan =
        SpawnPlan { win: None, ensure_feed: None, resolve_poly_token: None, next_win_n: n + 1 };
    match req {
        SpawnRequest::Palette { raw } => {
            let raw = raw.trim().to_uppercase();
            if raw.is_empty() {
                // Nothing typed: no window, and no cascade slot burned either. The counter IS the
                // id seed, so spending one here would leave a permanent hole in the id sequence
                // for every stray Enter in an empty palette.
                plan.next_win_n = n;
                return plan;
            }
            // A naive `ends_with` suffix, pinned WITH its flaw: `BTCUSD` becomes `BTCUSDUSDT`.
            let sym = if raw.ends_with("USDT") { raw } else { format!("{raw}USDT") };
            plan.ensure_feed = Some(binance_1m(&sym));
            plan.win = Some(WinState::new(
                &format!("chart-{n}"),
                &sym,
                "1m",
                WinKind::Chart,
                cascade_rect(desktop_min, n, CASCADE_ORIGIN, egui::vec2(640.0, 420.0)),
            ));
        }
        SpawnRequest::NewWindowChart { symbol } => {
            plan.ensure_feed = Some(binance_1m(&symbol));
            plan.win = Some(WinState::new(
                &format!("chart-{n}"),
                &symbol,
                "1m",
                WinKind::Chart,
                cascade_rect(desktop_min, n, CASCADE_ORIGIN_NEW_WINDOW, egui::vec2(700.0, 440.0)),
            ));
        }
        // An EXHAUSTIVE match, no `_` arm: the inline `else` this replaced meant a new `WinKind`
        // silently inherited the generic tool geometry, and a kind that needs a symbol or a stream
        // (as Trade and Polymarket both do) would have opened blank with nothing to say so. A new
        // variant now fails to COMPILE here until somebody classifies it.
        SpawnRequest::Kind { kind, poly_seed_token } => match kind {
            WinKind::Chart => {
                plan.ensure_feed = Some(binance_1m("BTCUSDT"));
                plan.win = Some(WinState::new(
                    &format!("chart-{n}"),
                    "BTCUSDT",
                    "1m",
                    kind,
                    cascade_rect(desktop_min, n, CASCADE_ORIGIN, egui::vec2(640.0, 420.0)),
                ));
            }
            WinKind::Trade => {
                // The ladder, the ticket and the status strip at the side-panel size, as tall as
                // the whole form needs (`trade_size`). No symbol yet: the glue seeds the last-used
                // instrument, or the backend's primary market, on the first frame (spec §3.11). No
                // bar feed (Ruling R6); the depth stream is requested by the window loop once the
                // symbol is known.
                let mut ws = WinState::tool(
                    &format!("trade-{n}"),
                    kind,
                    cascade_rect(desktop_min, n, CASCADE_ORIGIN, trade_size),
                );
                ws.symbol = String::new();
                ws.title = "Trade".to_string();
                plan.win = Some(ws);
            }
            WinKind::Polymarket => {
                // The cockpit is narrow + tall. `symbol` is the YES-outcome token-id:
                // the caller seeds it from VIKE_POLY_COCKPIT_TOKEN when supplied, else the
                // placeholder — which is the ONLY case that asks for a background Gamma resolve,
                // because that resolve OVERWRITES `symbol` and would otherwise move an operator's
                // deliberately-seeded window to a different market. The window loop's
                // `ensure_poly_book` subscribes whichever token ends up here.
                let mut ws = WinState::tool(
                    &format!("poly-{n}"),
                    kind,
                    cascade_rect(desktop_min, n, CASCADE_ORIGIN, egui::vec2(340.0, 640.0)),
                );
                ws.symbol = poly_seed_token;
                ws.title = "Polymarket · Cockpit".to_string();
                if ws.symbol == POLY_PLACEHOLDER_TOKEN {
                    plan.resolve_poly_token = Some(ws.id);
                }
                plan.win = Some(ws);
            }
            WinKind::Account
            | WinKind::Options
            | WinKind::Greeks
            | WinKind::News
            | WinKind::Calendar
            | WinKind::Data
            | WinKind::Studio
            | WinKind::Connections
            | WinKind::Tearsheet => {
                plan.win = Some(WinState::tool(
                    &format!("tool-{n}"),
                    kind,
                    cascade_rect(desktop_min, n, CASCADE_ORIGIN, TOOL_WINDOW_SIZE),
                ));
            }
            WinKind::Settings => {
                plan.win = Some(WinState::tool(
                    &format!("tool-{n}"),
                    kind,
                    cascade_rect(desktop_min, n, CASCADE_ORIGIN, SETTINGS_WINDOW_SIZE),
                ));
            }
        },
        // ⚠ `venue: _source_venue` IS the pinned defect, spelled at the site where it happens
        // rather than argued about elsewhere: the source window's venue arrives and is discarded,
        // so `WinState::new`'s hardcoded `DEFAULT_VENUE` stands. STEP 2 is
        // `w.venue = _source_venue; w.retitle();` on the window below — one line here, nothing in
        // `vike-desktop` — plus the reddened pin that makes it a deliberate change.
        SpawnRequest::CloneWindow { venue: _source_venue, symbol, interval } => {
            // No feed, and the argument for that is venue-conditional. For a BINANCE source
            // `WinState::new` reproduces the source's key exactly (`SYMBOL@interval`), so the
            // clone reads the stream the source already pays for and a second `ensure` would be
            // pure duplication. For any other venue it does NOT: the source's key is
            // `venue:SYMBOL@interval` and the clone's is the un-namespaced form, so the clone is
            // backed by no subscription, nothing ever requests one, and it paints nothing. A
            // feed-less clone is only sound while it reproduces its source's key.
            plan.win = Some(WinState::new(
                &format!("chart-{n}"),
                &symbol,
                &interval,
                WinKind::Chart,
                cascade_rect(desktop_min, n, CASCADE_ORIGIN, egui::vec2(620.0, 380.0)),
            ));
        }
        SpawnRequest::TestSymbol { symbol } => {
            plan.ensure_feed = Some(binance_1m(&symbol));
            plan.win = Some(WinState::new(
                &format!("chart-{n}"),
                &symbol,
                "1m",
                WinKind::Chart,
                cascade_rect(desktop_min, n, CASCADE_ORIGIN, egui::vec2(700.0, 440.0)),
            ));
        }
        SpawnRequest::StoredOpen { venue, symbol, interval } => {
            // Bar series open on their own interval; a tick-only series (quote/trade) has no chart
            // timeframe of its own, so it falls back to "1m" — the venue's kline feed for that
            // symbol, the same default as every other chart-open path. The reopen carries no
            // asset-class information (spot/legacy routing), byte-identical to before the feature.
            let iv = interval.unwrap_or_else(|| "1m".to_string());
            plan.ensure_feed = Some(FeedSpec {
                venue: venue.clone(),
                symbol: symbol.clone(),
                interval: iv.clone(),
                asset_class: None,
            });
            let mut w = WinState::new(
                &format!("chart-{n}"),
                &symbol,
                &iv,
                WinKind::Chart,
                cascade_rect(desktop_min, n, CASCADE_ORIGIN, egui::vec2(700.0, 440.0)),
            );
            // The ONE site that overrides the venue, so it is also the one that must retitle:
            // `WinState::new` titled the window as Binance, and a non-Binance chart carries an
            // upper-cased venue prefix (`title_for`). Assign, THEN retitle — in that order.
            w.venue = venue;
            w.retitle();
            plan.win = Some(w);
        }
    }
    plan
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
    let off = CASCADE_STEP * (n % CASCADE_SLOTS) as f32;
    egui::Rect::from_min_size(desktop_min + egui::vec2(origin.x + off, origin.y + off), size)
}

/// The spot 1-minute Binance series — [`DEFAULT_VENUE`] plus `"1m"`, the venue-less default four
/// of the six sites want. (`App::ensure_feed`, the wrapper that used to spell this shape in
/// `vike-app`, was deleted when its last caller moved into
/// `crates/vike-app-core/src/ui/initial_arrange.rs`.)
fn binance_1m(symbol: &str) -> FeedSpec {
    FeedSpec {
        venue: DEFAULT_VENUE.to_string(),
        symbol: symbol.to_string(),
        interval: "1m".to_string(),
        asset_class: None,
    }
}

#[path = "window_spawn_tests.rs"]
#[cfg(test)]
mod window_spawn_tests;
