//! `window_spawn` — the NEW-WINDOW decision: which cascade slot a freshly opened window lands in,
//! what it is titled and seeded with, and which live subscriptions must already exist before it can
//! paint anything at all.
//!
//! Moved down out of `vike-app`'s `main.rs` for the reason [`crate::startup`] and
//! [`crate::feed_lifecycle`] moved before it: that file is in **no gate**.
//! `xtask/src/ci/tables.rs`'s `EXCLUDE_FROM_CI` names `vike-app`, so the DERIVED roster that both
//! the justfile's `ci_crates` and CI's fast lane run omits it, and the `app-check` job that DOES
//! compile the crate runs its unit tests over `crates/vike-desktop/src/chart_gpu.rs`'s GPU byte-layout
//! pins and nothing else. SIX separate places in that file's frame loop CASCADE a new window, and
//! every one of them re-derived the cascade arithmetic, the [`WinState`] construction and the
//! market-data subscription by hand.
//!
//! ⚠ SIX is the count of CASCADING sites, not of window-opening sites — the frame loop has two
//! more and they are deliberately NOT here. The `VIKE_TOOL` / `VIKE_TOOLS` QA hooks (both behind
//! `did_initial_arrange`, so both run once) place their windows at the desktop origin with no
//! cascade at all and spend a `tool-{n}` id for EVERY kind, so a DOM opened that way is `tool-N`
//! where the launcher's is `dom-N`. Folding them into this planner would CHANGE behaviour, and
//! this move may not; the STEP-2 row that used to stand here has since LANDED as
//! `crates/vike-app-core/src/initial_arrange.rs`'s `plan_initial_arrange` — a planner of its own,
//! deliberately separate so the `tool-{n}` ids and the origin placement stay byte-identical, with
//! a test comparing the two planners' DOM spawns so the divergence is observed rather than
//! narrated. `VIKE_TOOL=dom` is still the SECOND of the two DOM-opening sites
//! `crates/vike-app-core/src/feed_lifecycle.rs`'s `orphaned_feed_keys` documents, and its half of
//! the pair no longer rests on prose: that module's roster test runs the REAL `live_window_keys`
//! over the QA DOM spawn, the same invariant
//! `every_spawn_ensures_a_series_the_reaper_counts_as_live` holds for the six sites here.
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
//! `crates/vike-app-core/src/feed_lifecycle.rs`'s `live_window_keys` there — and a one-character
//! disagreement between them is not cosmetic:
//! `crates/vike-app-core/src/feed_lifecycle.rs`'s `reap_orphaned_feeds` runs EVERY frame and stops
//! any `spawned` key no window backs. A DOM window's ensure is a ONE-SHOT at creation time, never
//! re-asserted per frame — the argument is on the live-set computation itself,
//! `crates/vike-app-core/src/feed_lifecycle.rs`'s `orphaned_feed_keys`, not on the reaper that
//! consumes it — so a mismatch kills the stream on the frame AFTER the window opens and nothing
//! ever re-requests it: a dead ladder with no error anywhere. Nothing checked that the six sites
//! agreed with the reaper.
//! `every_spawn_ensures_a_series_the_reaper_counts_as_live` now does, by running the REAL
//! `live_window_keys` over the REAL window this planner returns, never over a restatement of its
//! rule.
//!
//! **Data in, decisions out.** [`plan_spawn`] is pure: the desktop origin, the window counter and
//! one [`SpawnRequest`] in; a [`SpawnPlan`] — the window, the two subscriptions and the background
//! resolve, as VALUES — out. It never touches `App`, opens a socket, or reads the environment. The
//! cockpit's `VIKE_POLY_COCKPIT_TOKEN` read stays in the binary and arrives as an already-read seed
//! string (the same `Injected` shape [`crate::startup`] uses), so every `VIKE_*` settings-registry
//! row stays classified `vike-app` / `Layer::Binary` exactly as today.
//! `crates/vike-desktop/src/main.rs`'s `apply_spawn` applies the plan in the order documented on
//! [`SpawnPlan`], because a spawn's ORDER is load-bearing: every site that subscribed anything
//! subscribed BEFORE publishing its window, and the cockpit spawned its Gamma resolver AFTER
//! pushing.

use crate::startup::FeedSpec;
use crate::tool_views::POLY_PLACEHOLDER_TOKEN;
use crate::workspace::{DEFAULT_VENUE, WinKind, WinState};
use vike_panels::dom;

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
    /// `crates/vike-desktop/src/chart_window.rs`'s picker fallback, so the list belongs to `vike-app`,
    /// not to this planner — only the slot and the construction moved.
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
    /// `vike-app` shell moves at all. Without it the fix would have had to reopen the very file
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
/// 3. [`ensure_depth`](Self::ensure_depth) — the DOM's L2 stream, likewise before publication.
/// 4. [`win`](Self::win) — push the window.
/// 5. [`resolve_poly_token`](Self::resolve_poly_token) — spawn the background Gamma resolver, which
///    the cockpit arm did AFTER its push.
///
/// No derives: [`WinState`] carries live egui dialog/pane state and is neither `Clone` nor `Debug`.
/// Built once, consumed once — the same shape [`crate::startup`]'s `StartupLayout` takes, and for
/// the same reason.
pub struct SpawnPlan {
    /// The window to publish. `None` only for a blank palette submit, the one request that can
    /// decline to spawn — in which case `next_win_n` is unchanged and no window id is burned.
    pub win: Option<WinState>,
    /// The bar series the new window will read, as `App::ensure_feed_on`'s argument list.
    pub ensure_feed: Option<FeedSpec>,
    /// The venue L2 depth stream a DOM window's ladder needs (`App::ensure_depth`).
    pub ensure_depth: Option<(dom::DomVenue, String)>,
    /// The cockpit window whose YES token is still the placeholder and therefore needs background
    /// Gamma resolution (`App::spawn_poly_token_resolver`).
    pub resolve_poly_token: Option<egui::Id>,
    /// `App::next_win_n` after this spawn.
    pub next_win_n: u32,
}

/// Decide one window spawn. Pure: same inputs ⇒ same [`SpawnPlan`].
///
/// `desktop_min` is the window arena's top-left (`App::desktop`'s `min`) and `next_win_n` the
/// shared window counter, which supplies BOTH the cascade slot and the `egui::Id` seed — the id
/// PREFIX differs per family (`chart-` / `dom-` / `poly-` / `tool-`) but the counter does not, so
/// the prefix is decoration and the counter is the whole of what keeps two windows apart.
pub fn plan_spawn(req: SpawnRequest, desktop_min: egui::Pos2, next_win_n: u32) -> SpawnPlan {
    let n = next_win_n;
    let mut plan = SpawnPlan {
        win: None,
        ensure_feed: None,
        ensure_depth: None,
        resolve_poly_token: None,
        next_win_n: n + 1,
    };
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
        // (as Dom and Polymarket both do) would have opened blank with nothing to say so. A new
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
            WinKind::Dom => {
                // DOM needs a symbol (for the mark + working-order filter), a bar feed so marks
                // flow, and the live L2 depth stream for the ladder; it is narrow + tall. The
                // `"1m"` is not a preference — it is the interval `live_window_keys` hardcodes for
                // a DOM window, and a bar feed on any other interval is reaped the next frame.
                plan.ensure_feed = Some(binance_1m("BTCUSDT"));
                plan.ensure_depth = Some((dom::DomVenue::Binance, "BTCUSDT".to_string()));
                let mut ws = WinState::tool(
                    &format!("dom-{n}"),
                    kind,
                    cascade_rect(desktop_min, n, CASCADE_ORIGIN, egui::vec2(320.0, 560.0)),
                );
                ws.symbol = "BTCUSDT".to_string();
                ws.title = "DOM · Pro".to_string();
                plan.win = Some(ws);
            }
            WinKind::Polymarket => {
                // The cockpit is narrow + tall like the DOM. `symbol` is the YES-outcome token-id:
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
            WinKind::Trade
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
                    cascade_rect(desktop_min, n, CASCADE_ORIGIN, egui::vec2(560.0, 400.0)),
                ));
            }
        },
        // ⚠ `venue: _source_venue` IS the pinned defect, spelled at the site where it happens
        // rather than argued about elsewhere: the source window's venue arrives and is discarded,
        // so `WinState::new`'s hardcoded `DEFAULT_VENUE` stands. STEP 2 is
        // `w.venue = _source_venue; w.retitle();` on the window below — one line here, nothing in
        // `vike-app` — plus the reddened pin that makes it a deliberate change.
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
/// `crates/vike-app-core/src/initial_arrange.rs`; the QA DOM there builds the same `FeedSpec`,
/// and both spellings are pinned against `live_window_keys`.)
fn binance_1m(symbol: &str) -> FeedSpec {
    FeedSpec {
        venue: DEFAULT_VENUE.to_string(),
        symbol: symbol.to_string(),
        interval: "1m".to_string(),
        asset_class: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feed_lifecycle::live_window_keys;
    use crate::workspace::series_key;
    use std::collections::HashSet;

    /// A deliberately NON-ZERO desktop origin: a planner that dropped `desktop_min` on the floor
    /// would satisfy every geometry assertion below if the arena happened to start at (0, 0).
    const DESKTOP: egui::Pos2 = egui::pos2(12.0, 7.0);

    /// What each request is DECLARED to do about the subscription its window needs. Named per row
    /// rather than inferred, so a NEW [`SpawnRequest`] arm has to be classified instead of quietly
    /// joining the "ensures nothing" column — the same "a named row proves it was classified,
    /// not forgotten" discipline the per-venue capability tables use.
    ///
    /// ⚠ "has to" is only literal because [`the_roster_exercises_every_spawn_request_variant`]
    /// makes it so. [`roster`] is a hand-written list with no roster constant behind it (a
    /// data-carrying enum has no `ALL`), so on its own it would let a new variant take its
    /// [`plan_spawn`] arm and be executed by no test in this module at all — absent, in
    /// particular, from [`every_spawn_ensures_a_series_the_reaper_counts_as_live`], the one
    /// assertion here that is an invariant rather than a pin.
    enum FeedDuty {
        /// Ensures exactly the series its window reads — checked against the REAL
        /// `crates/vike-app-core/src/feed_lifecycle.rs`'s `live_window_keys`, never a restatement
        /// of its rule.
        Ensures,
        /// Ensures nothing, and the argument for why that is correct rather than a hole.
        NoFeed(&'static str),
        /// Declines to spawn at all.
        NoWindow,
    }

    /// Every spawn path, once each — the six requests plus one row per [`WinKind`], so the launcher
    /// arm is covered variant-by-variant rather than by whichever kind a test happened to pick.
    fn roster() -> Vec<(&'static str, SpawnRequest, FeedDuty)> {
        let mut rows = vec![
            ("palette/blank", SpawnRequest::Palette { raw: "   ".to_string() }, FeedDuty::NoWindow),
            ("palette/symbol", SpawnRequest::Palette { raw: "eth".to_string() }, FeedDuty::Ensures),
            (
                "menu/new-window",
                SpawnRequest::NewWindowChart { symbol: "BTCUSDT".to_string() },
                FeedDuty::Ensures,
            ),
            (
                "title-bar/clone",
                SpawnRequest::CloneWindow {
                    venue: DEFAULT_VENUE.to_string(),
                    symbol: "BTCUSDT".to_string(),
                    interval: "5m".to_string(),
                },
                FeedDuty::NoFeed(
                    "a BINANCE source already ensured this exact key — the clone copies its \
                     symbol and interval, so `spawned` holds it already. ⚠ the reason is \
                     venue-CONDITIONAL: on any other venue the clone's key drops the `venue:` \
                     namespace and is backed by nobody, which is why this row is spelled with \
                     the default venue and why the loss is pinned separately",
                ),
            ),
            (
                "symbols/test-symbol",
                SpawnRequest::TestSymbol { symbol: "SOLUSDT".to_string() },
                FeedDuty::Ensures,
            ),
            (
                "stored/open-bar-series",
                SpawnRequest::StoredOpen {
                    venue: "binance".to_string(),
                    symbol: "BTCUSDT".to_string(),
                    interval: Some("5m".to_string()),
                },
                FeedDuty::Ensures,
            ),
            (
                "stored/open-tick-only-series",
                SpawnRequest::StoredOpen {
                    venue: "okx".to_string(),
                    symbol: "BTC-USDT".to_string(),
                    interval: None,
                },
                FeedDuty::Ensures,
            ),
        ];
        for kind in WinKind::ALL {
            let duty = match kind {
                WinKind::Chart | WinKind::Dom => FeedDuty::Ensures,
                _ => FeedDuty::NoFeed("a tool window drives no bar series of its own"),
            };
            rows.push((
                kind.slug(),
                SpawnRequest::Kind { kind, poly_seed_token: POLY_PLACEHOLDER_TOKEN.to_string() },
                duty,
            ));
        }
        rows
    }

    /// How many [`SpawnRequest`] variants [`variant_index`] knows about. Bumped by the author a
    /// stale value has already stopped — see [`the_roster_exercises_every_spawn_request_variant`]
    /// for the three-step ladder this number sits in the middle of.
    const REQUEST_VARIANTS: usize = 6;

    /// One dense index per [`SpawnRequest`] variant. EXHAUSTIVE and deliberately index-valued
    /// rather than name-valued: a name-returning match plus a hand-written list of expected names
    /// is not a gate at all — a new variant would be missing from BOTH sides and the comparison
    /// would still hold. An index into a fixed-width array cannot be missing from both.
    fn variant_index(req: &SpawnRequest) -> usize {
        match req {
            SpawnRequest::Palette { .. } => 0,
            SpawnRequest::NewWindowChart { .. } => 1,
            SpawnRequest::Kind { .. } => 2,
            SpawnRequest::CloneWindow { .. } => 3,
            SpawnRequest::TestSymbol { .. } => 4,
            SpawnRequest::StoredOpen { .. } => 5,
        }
    }

    /// The half a compile error cannot give you: being sent to the right FILE is not the same as
    /// having added a row.
    ///
    /// [`plan_spawn`]'s `match req` already refuses to compile on a new [`SpawnRequest`] variant,
    /// which is what lands the author here — but [`roster`] is a hand-written `Vec`, so a variant
    /// can take its planner arm, take a [`variant_index`] arm, and still be exercised by no test
    /// in this module. It would be absent above all from
    /// [`every_spawn_ensures_a_series_the_reaper_counts_as_live`], and a spawn path whose feed
    /// nobody checks against the reaper is precisely the shape that opens a window which paints
    /// for one frame and then goes dark.
    ///
    /// The ladder, deliberately three rungs so no single omission is silent: the new variant fails
    /// to COMPILE in [`variant_index`]; adding an arm there with a stale [`REQUEST_VARIANTS`]
    /// fails THIS test by name; bumping the constant then fails this test again until a [`roster`]
    /// row exists. Each rung's failure message names the next.
    #[test]
    fn the_roster_exercises_every_spawn_request_variant() {
        let mut seen = [false; REQUEST_VARIANTS];
        for (label, req, _) in roster() {
            let i = variant_index(&req);
            assert!(
                i < REQUEST_VARIANTS,
                "{label}: `variant_index` returned {i} — `REQUEST_VARIANTS` is stale, bump it to \
                 the number of `SpawnRequest` variants"
            );
            seen[i] = true;
        }
        let missing: Vec<usize> = (0..REQUEST_VARIANTS).filter(|i| !seen[*i]).collect();
        assert!(
            missing.is_empty(),
            "`SpawnRequest` variant index/indices {missing:?} are exercised by no `roster()` row \
             — add one (with its `FeedDuty`) or every test in this module skips that spawn path"
        );
    }

    /// **The invariant this module exists to hold, and the only test here that is not a pin.**
    ///
    /// A spawned window and the feed it asks for are decided in two different places — this
    /// planner's `ensure_feed`, and `live_window_keys`'s per-KIND rule — and a one-character
    /// disagreement is not cosmetic: `reap_orphaned_feeds` runs every frame and stops any
    /// `spawned` key no window backs, so the new window's stream dies on the frame after it opens
    /// and nothing re-requests it (a DOM's ensure is a ONE-SHOT at creation; see that function's
    /// doc). Driven over the REAL reaper input, so it cannot drift with a copy of the rule.
    #[test]
    fn every_spawn_ensures_a_series_the_reaper_counts_as_live() {
        for (label, req, duty) in roster() {
            let plan = plan_spawn(req, DESKTOP, 4);
            let win = plan.win;
            let feed = plan.ensure_feed;
            match duty {
                FeedDuty::NoWindow => {
                    assert!(win.is_none(), "{label}: declared NoWindow but spawned one");
                    assert!(feed.is_none(), "{label}: a feed with no window to read it");
                }
                FeedDuty::NoFeed(why) => {
                    assert!(win.is_some(), "{label}: declared a window and spawned none");
                    assert!(feed.is_none(), "{label}: unexpected feed — {why}");
                }
                FeedDuty::Ensures => {
                    let win = win.unwrap_or_else(|| panic!("{label}: Ensures, but no window"));
                    let f = feed.unwrap_or_else(|| panic!("{label}: Ensures, but no feed"));
                    let key = series_key(&f.venue, &f.symbol, &f.interval);
                    let live = live_window_keys(std::slice::from_ref(&win));
                    assert!(
                        live.contains(&key),
                        "{label}: ensures `{key}`, which no window backs — `reap_orphaned_feeds` \
                         stops it on the next frame and nothing re-requests it (live: {live:?})"
                    );
                }
            }
        }
    }

    /// A spawn that forgets to advance the counter hands the next window the SAME `egui::Id`, and
    /// egui keys a window's whole persisted state on that id — two windows would share geometry,
    /// and `App::tool_views` (keyed by `WinState::id`) would hand them one view state. The id
    /// PREFIX differing per family is what made the six inline copies look safe; the COUNTER is
    /// shared, so the prefix is decoration and this is the property that actually holds.
    #[test]
    fn spawning_the_whole_roster_back_to_back_never_reuses_an_egui_id() {
        let mut n = 0;
        let mut ids = Vec::new();
        for (label, req, _) in roster() {
            let plan = plan_spawn(req, DESKTOP, n);
            // EXACT, not `>=`. A `>=` passes on an arm that forgets to advance, and whether the
            // duplicate id that follows is actually OBSERVED then depends on the NEXT roster row
            // happening to share an id prefix — `chart-` follows `chart-`, but a `tool-` arm that
            // stalls is followed by the `dom-` row and the collision never materialises. That is
            // an accident of this list's order, not a property, so the step is asserted directly:
            // one id per window published, and none per window declined.
            let expected = if plan.win.is_some() { n + 1 } else { n };
            assert_eq!(plan.next_win_n, expected, "{label}: wrong window-counter step");
            n = plan.next_win_n;
            if let Some(w) = plan.win {
                ids.push(w.id);
            }
        }
        let unique: HashSet<egui::Id> = ids.iter().copied().collect();
        assert_eq!(unique.len(), ids.len(), "two windows in one session share an egui::Id");
    }

    /// A stray Enter in an empty palette must cost nothing — no window, no live subscribe, and no
    /// window id, because the counter IS the id seed and a burned one is a permanent hole in the
    /// sequence. Without the guard the mutated build opens a chart titled `USDT · 1m` and issues a
    /// real Binance subscribe for a symbol named `USDT`.
    #[test]
    fn a_blank_palette_spawns_nothing_and_burns_no_cascade_slot() {
        for raw in ["", "   ", "\t\n "] {
            let plan = plan_spawn(SpawnRequest::Palette { raw: raw.to_string() }, DESKTOP, 5);
            assert!(plan.win.is_none(), "{raw:?} spawned a window");
            assert!(plan.ensure_feed.is_none(), "{raw:?} subscribed a feed");
            assert_eq!(plan.next_win_n, 5, "{raw:?} burned a window id");
        }
    }

    /// The palette's symbol ladder, pinned INCLUDING the part that is wrong: the quote-currency
    /// suffix is a naive `ends_with`, so `btcusd` becomes `BTCUSDUSDT` rather than being corrected
    /// or refused. That is today's behaviour and this move preserves it byte-for-byte; the row is
    /// here so a fix is a deliberate edit to a red test rather than an accident.
    #[test]
    fn the_palette_trims_uppercases_and_appends_the_quote_currency() {
        for (raw, expect) in [
            ("btc", "BTCUSDT"),
            ("  eth  ", "ETHUSDT"),
            ("solusdt", "SOLUSDT"),
            ("SOLUSDT", "SOLUSDT"),
            ("btcusd", "BTCUSDUSDT"),
        ] {
            let plan = plan_spawn(SpawnRequest::Palette { raw: raw.to_string() }, DESKTOP, 0);
            let win = plan.win.expect("a non-blank palette entry spawns a chart");
            assert_eq!(win.symbol, expect, "palette {raw:?}");
            assert_eq!(win.interval, "1m", "palette {raw:?}");
            assert_eq!(win.kind, WinKind::Chart, "palette {raw:?}");
            let f = plan.ensure_feed.expect("the chart needs its own feed");
            assert_eq!(
                (f.venue.as_str(), f.symbol.as_str(), f.interval.as_str()),
                (DEFAULT_VENUE, expect, "1m"),
                "palette {raw:?}"
            );
        }
    }

    /// The exhaustive-match half: every [`WinKind`] opens SOMETHING with a non-zero size and spends
    /// exactly one id. The `_ => tool` arm this replaced would have let a new kind inherit the tool
    /// geometry silently; the compiler stops that now, and this stops a new arm from being wired to
    /// spawn nothing at all.
    #[test]
    fn every_window_kind_spawns_exactly_one_window_and_consumes_one_id() {
        for kind in WinKind::ALL {
            let req = SpawnRequest::Kind { kind, poly_seed_token: String::new() };
            let plan = plan_spawn(req, DESKTOP, 3);
            let win = plan.win.unwrap_or_else(|| panic!("{kind:?} spawned no window"));
            assert_eq!(win.kind, kind);
            assert_eq!(plan.next_win_n, 4, "{kind:?}");
            assert!(win.size.x > 0.0 && win.size.y > 0.0, "{kind:?} opened at zero size");
        }
    }

    /// The cockpit's background Gamma resolve OVERWRITES the window's `symbol`. Requesting it for a
    /// real seeded token would silently move the operator's window to a different market; never
    /// requesting it leaves a placeholder window permanently STALE. The gate is the seed still
    /// being the placeholder — and it is per-KIND, so a non-cockpit launcher ignores the seed.
    #[test]
    fn the_cockpit_resolves_only_a_still_placeholder_token() {
        let ph_req = SpawnRequest::Kind {
            kind: WinKind::Polymarket,
            poly_seed_token: POLY_PLACEHOLDER_TOKEN.to_string(),
        };
        let placeholder = plan_spawn(ph_req, DESKTOP, 2);
        let win = placeholder.win.expect("cockpit window");
        assert_eq!(placeholder.resolve_poly_token, Some(win.id));
        assert_eq!(win.symbol, POLY_PLACEHOLDER_TOKEN);
        assert_eq!(win.title, "Polymarket · Cockpit");

        let seeded_req =
            SpawnRequest::Kind { kind: WinKind::Polymarket, poly_seed_token: "0xfeed".to_string() };
        let seeded = plan_spawn(seeded_req, DESKTOP, 2);
        assert_eq!(seeded.resolve_poly_token, None, "a seeded token must not be resolved over");
        assert_eq!(seeded.win.expect("cockpit window").symbol, "0xfeed");

        for kind in WinKind::ALL {
            if kind == WinKind::Polymarket {
                continue;
            }
            let req =
                SpawnRequest::Kind { kind, poly_seed_token: POLY_PLACEHOLDER_TOKEN.to_string() };
            let plan = plan_spawn(req, DESKTOP, 2);
            assert_eq!(plan.resolve_poly_token, None, "{kind:?} must ignore the cockpit seed");
        }
    }

    /// A DOM opens with three things or it is useless: a symbol (the mark + working-order filter),
    /// its venue's L2 depth stream (the ladder), and a 1m bar feed (so marks and paper fills flow).
    /// The interval is not a preference — `live_window_keys` hardcodes `"{symbol}@1m"` for a DOM.
    #[test]
    fn a_dom_launcher_seeds_its_symbol_its_depth_stream_and_its_bar_feed() {
        let req = SpawnRequest::Kind { kind: WinKind::Dom, poly_seed_token: String::new() };
        let plan = plan_spawn(req, DESKTOP, 1);
        let win = plan.win.expect("DOM window");
        assert_eq!(win.symbol, "BTCUSDT");
        assert_eq!(win.title, "DOM · Pro");
        assert_eq!(win.id, egui::Id::new("dom-1"));
        assert_eq!(plan.ensure_depth, Some((dom::DomVenue::Binance, "BTCUSDT".to_string())));
        let f = plan.ensure_feed.expect("the DOM's own 1m bar feed");
        assert_eq!(f.interval, "1m", "the interval `live_window_keys` hardcodes for a DOM");
    }

    /// Stored ▸ Open in chart is the ONE site that overrides `venue`, so it is the one that must
    /// retitle — and the one whose feed key is venue-namespaced. A tick-only series
    /// (`interval: None`) falls back to the venue's 1m klines.
    #[test]
    fn a_stored_open_carries_its_venue_into_the_window_title_and_the_feed() {
        let req = SpawnRequest::StoredOpen {
            venue: "okx".to_string(),
            symbol: "BTC-USDT".to_string(),
            interval: None,
        };
        let plan = plan_spawn(req, DESKTOP, 0);
        let win = plan.win.expect("stored open spawns a chart");
        assert_eq!(win.venue, "okx");
        assert_eq!(win.interval, "1m", "a tick-only series has no timeframe of its own");
        assert_eq!(win.title, "OKX BTC-USDT · 1m", "retitle() ran AFTER the venue was assigned");
        assert_eq!(win.key(), "okx:BTC-USDT@1m");
        let f = plan.ensure_feed.expect("the reopened series' feed");
        assert_eq!(series_key(&f.venue, &f.symbol, &f.interval), win.key());

        // …and a Binance stored open stays byte-identical to every other chart open.
        let plain_req = SpawnRequest::StoredOpen {
            venue: "binance".to_string(),
            symbol: "BTCUSDT".to_string(),
            interval: Some("5m".to_string()),
        };
        let plain = plan_spawn(plain_req, DESKTOP, 0).win.expect("stored open spawns a chart");
        assert_eq!(plain.title, "BTCUSDT · 5m");
        assert_eq!(plain.key(), "BTCUSDT@5m");
    }

    /// **A PIN, not an endorsement.** A title-bar clone copies its source window's symbol and
    /// interval but NOT its venue, so cloning an OKX chart yields a window pointed at Binance
    /// carrying an OKX symbol — and because a clone deliberately ensures no feed (the source's key
    /// is already in `spawned`), the clone's own key is subscribed by nobody and it paints nothing.
    /// That is the behaviour of every build that has shipped; this move preserves it exactly. The
    /// fix is a STEP-2 change that must edit this test, which is the entire point of writing it.
    ///
    /// The request is spelled with a NON-default venue on purpose. A clone request that carried no
    /// venue at all could only ever have proved that [`WinState::new`] hardcodes [`DEFAULT_VENUE`]
    /// — a fact about `WinState`, true whatever the clone does — so the test would have worn a
    /// name it could not check. Handing it `okx` is what makes "loses its source venue" an
    /// observation rather than a caption, and the two key assertions below are what makes the
    /// CONSEQUENCE observable rather than merely asserted in prose: the clone's key is not the
    /// series its source ensured, so nothing backs it.
    #[test]
    fn a_cloned_window_keeps_symbol_and_interval_but_loses_its_source_venue() {
        let req = SpawnRequest::CloneWindow {
            venue: "okx".to_string(),
            symbol: "BTC-USDT".to_string(),
            interval: "5m".to_string(),
        };
        let plan = plan_spawn(req, DESKTOP, 0);
        let win = plan.win.expect("clone spawns a chart");
        assert_eq!(win.symbol, "BTC-USDT");
        assert_eq!(win.interval, "5m");
        assert_eq!(
            win.venue, DEFAULT_VENUE,
            "PINNED: the request CARRIES `okx` and the clone still opens on binance"
        );
        assert!(plan.ensure_feed.is_none(), "PINNED: a clone subscribes nothing of its own");
        assert_eq!(win.key(), "BTC-USDT@5m", "PINNED: the un-namespaced Binance key form");
        assert_ne!(
            win.key(),
            series_key("okx", "BTC-USDT", "5m"),
            "PINNED: and therefore NOT the series the source window ensured — a feed-less clone \
             is only sound while it reproduces its source's key, and this one does not"
        );
        assert_eq!(win.title, "BTC-USDT · 5m", "PINNED: the OKX title prefix goes with the venue");
    }

    /// The cascade table, pinned per site — including the ONE site that disagrees with the other
    /// five about where slot 0 sits. Window ▸ New window has always cascaded from (40, 30) and
    /// every other site from (50, 40); nothing documented the difference and nothing depended on
    /// it, which is exactly why the tidy-up that unifies them must redden a test instead of
    /// silently moving a window ten points.
    #[test]
    fn each_spawn_site_cascades_from_its_own_pinned_origin_and_size() {
        // n = 3 ⇒ slot 3 ⇒ 28 × 3 = 84 points along the cascade. DESKTOP is (12, 7), so the
        // five-site origin lands at (12+50+84, 7+40+84) and New window's at (12+40+84, 7+30+84).
        let at = |req: SpawnRequest| plan_spawn(req, DESKTOP, 3).win.expect("a window");
        let launch = |k: WinKind| SpawnRequest::Kind { kind: k, poly_seed_token: String::new() };

        let palette = at(SpawnRequest::Palette { raw: "btc".to_string() });
        assert_eq!(palette.pos, egui::pos2(146.0, 131.0));
        assert_eq!(palette.size, egui::vec2(640.0, 420.0));
        assert_eq!(palette.id, egui::Id::new("chart-3"));

        let new_window = at(SpawnRequest::NewWindowChart { symbol: "BTCUSDT".to_string() });
        assert_eq!(
            new_window.pos,
            egui::pos2(136.0, 121.0),
            "PINNED divergence: this site alone cascades from (40, 30)"
        );
        assert_eq!(new_window.size, egui::vec2(700.0, 440.0));

        let chart = at(launch(WinKind::Chart));
        assert_eq!((chart.pos, chart.size), (egui::pos2(146.0, 131.0), egui::vec2(640.0, 420.0)));

        let d = at(launch(WinKind::Dom));
        assert_eq!((d.pos, d.size), (egui::pos2(146.0, 131.0), egui::vec2(320.0, 560.0)));
        assert_eq!(d.id, egui::Id::new("dom-3"));

        let p = at(launch(WinKind::Polymarket));
        assert_eq!((p.pos, p.size), (egui::pos2(146.0, 131.0), egui::vec2(340.0, 640.0)));
        assert_eq!(p.id, egui::Id::new("poly-3"));

        let t = at(launch(WinKind::News));
        assert_eq!((t.pos, t.size), (egui::pos2(146.0, 131.0), egui::vec2(560.0, 400.0)));
        assert_eq!(t.id, egui::Id::new("tool-3"));

        let c = at(SpawnRequest::CloneWindow {
            venue: DEFAULT_VENUE.to_string(),
            symbol: "BTCUSDT".to_string(),
            interval: "1m".to_string(),
        });
        assert_eq!((c.pos, c.size), (egui::pos2(146.0, 131.0), egui::vec2(620.0, 380.0)));

        let ts = at(SpawnRequest::TestSymbol { symbol: "SOLUSDT".to_string() });
        assert_eq!((ts.pos, ts.size), (egui::pos2(146.0, 131.0), egui::vec2(700.0, 440.0)));

        // Eight slots, then the walk WRAPS: slot 8 lands exactly on slot 0. Today's behaviour —
        // the ids still differ, so egui keeps the two windows' state apart and either can be
        // dragged off the other.
        let sym = || SpawnRequest::NewWindowChart { symbol: "BTCUSDT".to_string() };
        let slot0 = plan_spawn(sym(), DESKTOP, 0).win.expect("a window");
        let slot8 = plan_spawn(sym(), DESKTOP, 8).win.expect("a window");
        assert_eq!(slot0.pos, slot8.pos, "PINNED: the cascade wraps every 8 windows");
        assert_ne!(slot0.id, slot8.id);
    }
}
