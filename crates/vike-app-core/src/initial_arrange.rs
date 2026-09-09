//! `initial_arrange` — the FIRST-FRAME layout decision: what the desktop does exactly once, on the
//! first frame whose window arena is wide enough to be real — which QA capture windows open and
//! with which feeds, whether the open windows are tiled or a lone one is maximized, and which of
//! the two QA hooks then run.
//!
//! Moved down out of `vike-app`'s `main.rs` — the `did_initial_arrange` block in `App`'s frame
//! loop — for the reason [`crate::startup`] and [`crate::window_spawn`] moved before it: that file
//! is in **no gate**. `xtask/src/ci/tables.rs`'s `EXCLUDE_FROM_CI` names `vike-app`, so the
//! DERIVED roster omits it, and the `app-check` job that DOES compile the crate executes nothing
//! in it beyond `crates/vike-app/src/chart_gpu.rs`'s byte-layout pins. This block is a
//! once-per-session branch over TEN environment knobs, and it is the STEP-2 row
//! [`crate::window_spawn`]'s module doc deferred by name: the `VIKE_TOOL` / `VIKE_TOOLS` QA arms
//! are the frame loop's two window-opening sites that planner deliberately left behind.
//!
//! **A separate planner, NOT a seventh [`crate::window_spawn::SpawnRequest`] arm — the fence is
//! behavioural.** The QA arms spend a `tool-{n}` id for EVERY kind (a DOM opened this way is
//! `tool-N` where the launcher's is `dom-N`, and egui keys a window's whole persisted state on
//! that id), and they place every window at the DESKTOP ORIGIN at the one generic tool size
//! instead of burning a cascade slot. Folding them into [`crate::window_spawn::plan_spawn`] would
//! silently rename and move every QA window, which is exactly the change that planner's module doc
//! forbade for its own move; `the_qa_dom_is_tool_prefixed_and_uncascaded_unlike_the_launcher_dom`
//! compares the two planners' DOM spawns directly, so the divergence is an observation a fold has
//! to redden rather than a prose warning it can skip.
//!
//! **Pinned, not endorsed** — today's reality is declared with its flaws, per the capability-map
//! playbook's STEP 1, each behind a named test a fix must redden:
//!
//!   * An unknown `VIKE_TOOL` slug falls back to the CALENDAR — the caller-chosen `unwrap_or`
//!     beside the explicit [`WinKind::from_slug`] parse, spelled exactly as the inline block
//!     spelled it.
//!   * `VIKE_DOM_VENUE` maps four venues and deliberately NOT `"binance"`: the seed only ever
//!     overrides, and the [`dom::DomState`] default already IS Binance, so a `"binance"` row would
//!     be a fifth spelling of a no-op. Casing is exact, like the match this replaces.
//!   * `VIKE_TOOL=chart` opens a TOOL-constructed chart — empty symbol, empty interval, NO feed
//!     ensured — unlike the launcher's Chart arm, which seeds BTCUSDT and subscribes its series. A
//!     capture pointed at it renders an empty pane, which is what it has always rendered.
//!   * A set-but-garbage `VIKE_CAL_PAGE` still ASSIGNS page 0 (a failed parse falls back to `0`),
//!     where a set-but-garbage `VIKE_DOM_GROUP` assigns NOTHING — two adjacent knobs, two error
//!     dispositions, both preserved byte-for-byte.
//!
//! **The dangerous half is the QA DOM's feed, and it was covered by NOTHING.** The DOM arm
//! hardcodes `"BTCUSDT"` / `"1m"`, and `crates/vike-app-core/src/feed_lifecycle.rs`'s
//! `live_window_keys` hardcodes `"{symbol}@1m"` for a DOM window — a one-character disagreement
//! between the two and `reap_orphaned_feeds` stops the freshly ensured stream on the frame AFTER
//! the window opens, with the one-shot ensure never re-requesting it. That is the same argument
//! [`crate::window_spawn`]'s module doc makes for the launcher's six sites, whose invariant test
//! could only ever cover its own half of the DOM-opening pair `orphaned_feed_keys` documents.
//! `every_qa_tool_kind_is_classified_for_the_feed_it_ensures` now drives the REAL
//! `live_window_keys` over the REAL planned window, never a restatement of its rule.
//!
//! **Data in, decisions out.** [`plan_initial_arrange`] is pure: the ALREADY-READ knob values
//! ([`ArrangeEnv`] — the `Injected` shape [`crate::startup`]'s `StartupEnv` established), the
//! window list, the desktop origin and the window counter in; an [`InitialArrangePlan`] out. The
//! ten reads stay in `vike-app`'s `main.rs`, so every `VIKE_*` settings-registry row stays
//! classified `vike-app` / `Layer::Binary`, and the tests below mutate no process environment. The
//! caller applies the plan in the order documented on [`InitialArrangePlan`]; the wins-plane
//! appliers ([`apply_arrange_action`], [`apply_qa_hooks`]) and the [`seed_dom_view`] ToolView
//! seeder live beside the planner so the apply stays a delegation, not a re-derivation.
//! [`tiling_memory`] is the shared "remember only tiling modes" rule — the menu's arrange arm and
//! this block carried two hand copies of it, the one-size-down instance of the drift
//! [`crate::window_spawn`] exists to stop.

use crate::startup::{FeedSpec, poly_cockpit_seed_token};
use crate::tool_views::POLY_PLACEHOLDER_TOKEN;
use crate::workspace::{self, Arrange, DEFAULT_VENUE, WinKind, WinState};
use vike_panels::dom;

/// The first-frame knobs `vike-app`'s `main.rs` reads off the process environment and hands down.
///
/// Every field is the ALREADY-READ value, never a variable name — see the module doc for why the
/// read stays in the binary. `Default` is the "no knob set" configuration: no QA windows, the Grid
/// arrange mode, no hooks.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ArrangeEnv {
    /// `VIKE_ARRANGE` — `tilev` / `tileh` / `cascade`; anything else (unset included) means Grid.
    pub arrange: Option<String>,
    /// `VIKE_TOOL=<slug>` — close every existing window and open ONLY this one tool, for a clean
    /// maximized capture. Beats [`tools`](Self::tools) when both are set (the inline else-if).
    pub tool: Option<String>,
    /// `VIKE_TOOLS` is present — close the chart windows and open the four capture tools tiled.
    pub tools: bool,
    /// `VIKE_DOM_MODE` — `elite` opens the QA DOM straight in the Elite surface.
    pub dom_mode: Option<String>,
    /// `VIKE_DOM_GROUP` — the QA DOM's initial price grouping in ticks, clamped to ≥ 1.
    pub dom_group: Option<String>,
    /// `VIKE_DOM_VENUE` — `bybit` / `okx` / `aster` / `hyperliquid` select the displayed venue;
    /// anything else leaves the default (Binance) standing.
    pub dom_venue: Option<String>,
    /// `VIKE_CAL_PAGE` — the QA tool window's calendar page (an equity page, for captures).
    pub cal_page: Option<String>,
    /// `VIKE_MAX` is present — maximize the first OPEN window after the arrange.
    pub max: bool,
    /// `VIKE_MIN` is present — minimize windows 1..=3 by INDEX after the arrange.
    pub min: bool,
    /// `VIKE_POLY_COCKPIT_TOKEN`, raw — the trim / empty / placeholder ladder
    /// ([`poly_cockpit_seed_token`]) is applied HERE, and only in the Polymarket arm, so the QA
    /// cockpit and [`crate::startup`]'s capture cockpit cannot drift about what a seed means.
    pub poly_cockpit_token: Option<String>,
}

/// What the QA DOM's `VIKE_DOM_MODE` / `VIKE_DOM_GROUP` / `VIKE_DOM_VENUE` knobs resolved to.
/// `Some` on every DOM spawn — even all-inert — because the inline block created the window's
/// `ToolView` entry unconditionally for a DOM, and [`seed_dom_view`] must inherit that side
/// effect's trigger, not re-derive it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DomViewSeed {
    /// `VIKE_DOM_MODE=elite`, exactly — everything else leaves the Pro default.
    pub elite: bool,
    /// The parsed-and-clamped grouping; `None` when the knob was unset OR unparseable (a garbage
    /// value assigns nothing, unlike `cal_page`'s fallback — see the module doc's pin list).
    pub group: Option<i64>,
    /// The selected venue; `None` for unset, `"binance"`, or any unrecognised spelling.
    pub venue: Option<dom::DomVenue>,
}

/// One QA tool window and everything the caller must do around its push.
///
/// No derives: [`WinState`] carries live egui dialog/pane state and is neither `Clone` nor
/// `Debug`. Built once, consumed once — the same rule as [`crate::window_spawn::SpawnPlan`].
pub struct QaToolSpawn {
    /// The window to publish, already seeded (symbol for a DOM, the cockpit token ladder for a
    /// Polymarket).
    pub win: WinState,
    /// The bar series the window needs, as `App::ensure_feed_on`'s argument list — BEFORE the
    /// push, like every subscribing site.
    pub ensure_feed: Option<FeedSpec>,
    /// The venue L2 depth stream a DOM's ladder needs (`App::ensure_depth`), likewise pre-push.
    pub ensure_depth: Option<(dom::DomVenue, String)>,
    /// The cockpit window whose YES token is still the placeholder and therefore needs background
    /// Gamma resolution (`App::spawn_poly_token_resolver`) — AFTER the push, as the inline arm
    /// spawned it.
    pub resolve_poly_token: Option<egui::Id>,
    /// The DOM view seeding ([`seed_dom_view`] onto the window's `ToolView`), after the push.
    pub dom_view: Option<DomViewSeed>,
    /// `ToolView::cal_page`, assigned whenever `VIKE_CAL_PAGE` was set — for ANY tool kind,
    /// exactly as the inline block read it.
    pub cal_page: Option<u8>,
}

/// What the first frame does about window GEOMETRY once the QA spawns (if any) are published.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArrangeAction {
    /// Two or more windows are open: tile/cascade them all with the resolved mode.
    Arrange(Arrange),
    /// Exactly one window is open: maximize it instead of arranging. A LONE open window fills the
    /// desktop as a maximized tool (uniform margins, pinned to bounds every frame) rather than
    /// being Grid-tiled to a rect egui then auto-fits slightly off — the "chart overflows into the
    /// status bar" bug. No index is carried: the caller's first-open scan IS the original's own
    /// resolution step, performed by [`apply_arrange_action`].
    MaximizeLoneOpen,
    /// Nothing is open; touch nothing.
    None,
}

/// What the first frame decides — and, critically, **in what order the caller must apply it**.
///
/// The order below is the order the inline block performed these steps every session before the
/// move, and it is reproduced rather than tidied:
///
/// 1. [`close_existing`](Self::close_existing) — close every pre-existing window (both QA arms
///    did this FIRST, before spawning anything).
/// 2. [`next_win_n`](Self::next_win_n) — adopt the advanced counter (the inline arm advanced it
///    before its ensures too).
/// 3. [`spawns`](Self::spawns), in order; for each: `ensure_feed` → `ensure_depth` → push the
///    window → `resolve_poly_token` → [`seed_dom_view`] → `cal_page`.
/// 4. [`arrange`](Self::arrange) — [`apply_arrange_action`] over the post-spawn window list.
/// 5. [`remember`](Self::remember) — assign `last_arrange` so an app-maximize can re-fill.
/// 6. [`qa_maximize_first_open`](Self::qa_maximize_first_open) /
///    [`qa_minimize_next_three`](Self::qa_minimize_next_three) — [`apply_qa_hooks`], LAST.
///
/// No derives, same reason as [`QaToolSpawn`].
pub struct InitialArrangePlan {
    /// Close every window that already exists (either QA arm is active).
    pub close_existing: bool,
    /// The QA tool windows to publish — one for `VIKE_TOOL`, four for `VIKE_TOOLS`, none
    /// otherwise.
    pub spawns: Vec<QaToolSpawn>,
    /// `App::next_win_n` after the spawns: advanced EXACTLY once per window, because the counter
    /// is the `egui::Id` seed and a stalled step hands two windows one persisted state.
    pub next_win_n: u32,
    /// The geometry decision, computed from the open count AS THE CALLER WILL LEAVE IT (spawns
    /// open, pre-existing windows closed when [`close_existing`](Self::close_existing)) — sound
    /// because [`WinState::tool`] constructs `open: true`.
    pub arrange: ArrangeAction,
    /// `Some` only when [`arrange`](Self::arrange) fired with a tiling mode — the inline block set
    /// `last_arrange` only inside its ≥2-open arm, and [`tiling_memory`] is the shared rule.
    pub remember: Option<Arrange>,
    /// `VIKE_MAX` — see [`apply_qa_hooks`].
    pub qa_maximize_first_open: bool,
    /// `VIKE_MIN` — see [`apply_qa_hooks`].
    pub qa_minimize_next_three: bool,
}

/// The shared "remember only tiling modes" rule: a tiling arrange is worth re-applying when the
/// app window later maximizes/restores, a Cascade is not (it opts out), and the two rail verbs
/// (MinimizeAll/RestoreAll) arrange nothing to remember. Two hand copies of this rule lived in
/// `vike-app`'s frame loop — the menu's arrange arm and the initial arrange — and both call sites
/// now delegate here, so the next mode added to [`Arrange`] gets classified once.
///
/// ⚠ An EXHAUSTIVE match, NOT the `matches!` both call sites spelled, and the difference is the
/// whole of what makes that last sentence true: `matches!` folds every unnamed variant into "not
/// remembered" silently, so a seventh [`Arrange`] would inherit that answer with nobody having
/// decided it — and `only_a_tiling_arrange_is_remembered` below is a HAND-WRITTEN table, which
/// cannot notice a variant it does not list either, so the two would agree and both be blind.
/// Spelled this way a new variant fails to COMPILE until it names its side, which is the
/// discipline [`WinKind::slug`] already carries one file over for exactly this reason.
pub fn tiling_memory(a: Arrange) -> Option<Arrange> {
    match a {
        Arrange::TileV | Arrange::TileH | Arrange::Grid => Some(a),
        Arrange::Cascade | Arrange::MinimizeAll | Arrange::RestoreAll => None,
    }
}

/// Decide the whole first-frame layout. Pure: same inputs ⇒ same [`InitialArrangePlan`].
///
/// `wins` is the window list AS OF the guard firing (only the open flags are read), `desktop_min`
/// the arena's top-left (`App::desktop`'s `min` — the QA arms place windows there, cascade-free),
/// and `next_win_n` the shared window counter, exactly as [`crate::window_spawn::plan_spawn`]
/// takes them.
pub fn plan_initial_arrange(
    env: &ArrangeEnv,
    wins: &[WinState],
    desktop_min: egui::Pos2,
    next_win_n: u32,
) -> InitialArrangePlan {
    let mode = match env.arrange.as_deref() {
        Some("tilev") => Arrange::TileV,
        Some("tileh") => Arrange::TileH,
        Some("cascade") => Arrange::Cascade,
        _ => Arrange::Grid,
    };
    let close_existing = env.tool.is_some() || env.tools;
    let mut n = next_win_n;
    let mut spawns: Vec<QaToolSpawn> = Vec::new();
    if let Some(only) = env.tool.as_deref() {
        // Explicit slug parse with the old silent `_ => Calendar` fallback spelled at the call
        // site — the shape the inline block already used.
        let k = WinKind::from_slug(only).unwrap_or(WinKind::Calendar);
        let r = egui::Rect::from_min_size(desktop_min, egui::vec2(560.0, 400.0));
        let mut ws = WinState::tool(&format!("tool-{n}"), k, r);
        n += 1;
        let mut ensure_feed = None;
        let mut ensure_depth = None;
        let mut dom_view = None;
        if k == WinKind::Dom {
            // DOM needs a symbol for the mark + order filter, its venue's L2 depth stream, and
            // the fixed "1m" bar feed `live_window_keys` hardcodes for a DOM window.
            ws.symbol = "BTCUSDT".to_string();
            ensure_feed = Some(FeedSpec {
                venue: DEFAULT_VENUE.to_string(),
                symbol: "BTCUSDT".to_string(),
                interval: "1m".to_string(),
                asset_class: None,
            });
            ensure_depth = Some((dom::DomVenue::Binance, "BTCUSDT".to_string()));
            dom_view = Some(DomViewSeed {
                elite: env.dom_mode.as_deref() == Some("elite"),
                group: env
                    .dom_group
                    .as_deref()
                    .and_then(|g| g.parse::<i64>().ok())
                    .map(|g| g.max(1)),
                venue: match env.dom_venue.as_deref() {
                    Some("bybit") => Some(dom::DomVenue::Bybit),
                    Some("okx") => Some(dom::DomVenue::Okx),
                    Some("aster") => Some(dom::DomVenue::Aster),
                    Some("hyperliquid") => Some(dom::DomVenue::Hyperliquid),
                    _ => None,
                },
            });
        }
        if k == WinKind::Polymarket {
            // The env token or the placeholder (Gamma-resolved) — the SAME ladder the launcher
            // and capture arms apply, so the three cockpit seeding paths cannot drift.
            ws.symbol = poly_cockpit_seed_token(env.poly_cockpit_token.as_deref());
        }
        let resolve_poly_token =
            (k == WinKind::Polymarket && ws.symbol == POLY_PLACEHOLDER_TOKEN).then_some(ws.id);
        // QA: open an equity page. Read for ANY tool kind, as inline; a failed parse ASSIGNS 0.
        let cal_page = env.cal_page.as_deref().map(|pg| pg.parse::<u8>().unwrap_or(0));
        spawns.push(QaToolSpawn {
            win: ws,
            ensure_feed,
            ensure_depth,
            resolve_poly_token,
            dom_view,
            cal_page,
        });
    } else if env.tools {
        for k in [WinKind::Calendar, WinKind::Options, WinKind::News, WinKind::Data] {
            let r = egui::Rect::from_min_size(desktop_min, egui::vec2(560.0, 400.0));
            let ws = WinState::tool(&format!("tool-{n}"), k, r);
            n += 1;
            spawns.push(QaToolSpawn {
                win: ws,
                ensure_feed: None,
                ensure_depth: None,
                resolve_poly_token: None,
                dom_view: None,
                cal_page: None,
            });
        }
    }
    // The open count AS THE CALLER WILL LEAVE IT: every spawn opens `true` and `close_existing`
    // zeroes every pre-existing flag, so the two worlds agree by construction. The count tests
    // `open` ALONE — an open-but-minimized window still counts, exactly as inline.
    let open_windows =
        if close_existing { spawns.len() } else { wins.iter().filter(|w| w.open).count() };
    // Only arrange when there are ≥2 windows to actually tile; a lone open window maximizes
    // instead (see `ArrangeAction::MaximizeLoneOpen` for the bug that rule exists to avoid).
    let arrange = if open_windows > 1 {
        ArrangeAction::Arrange(mode)
    } else if open_windows == 1 {
        ArrangeAction::MaximizeLoneOpen
    } else {
        ArrangeAction::None
    };
    let remember = match arrange {
        ArrangeAction::Arrange(m) => tiling_memory(m),
        _ => None,
    };
    InitialArrangePlan {
        close_existing,
        spawns,
        next_win_n: n,
        arrange,
        remember,
        qa_maximize_first_open: env.max,
        qa_minimize_next_three: env.min,
    }
}

/// Apply the geometry decision over the (post-spawn) window list — the wins-plane half the caller
/// delegates back here, so the lone-open resolution stays beside the rule that produced it.
pub fn apply_arrange_action(wins: &mut [WinState], desktop: egui::Rect, action: ArrangeAction) {
    match action {
        ArrangeAction::Arrange(mode) => workspace::apply_arrange(wins, desktop, mode),
        ArrangeAction::MaximizeLoneOpen => {
            if let Some(i) = wins.iter().position(|w| w.open) {
                workspace::maximize(&mut wins[i], desktop);
            }
        }
        ArrangeAction::None => {}
    }
}

/// The two QA hooks, applied AFTER the arrange in this order: `VIKE_MAX` maximizes the first OPEN
/// window (a tool in `VIKE_TOOLS` mode), then `VIKE_MIN` minimizes windows 1..=3 **by index over
/// the full list, open or not** — a pinned asymmetry the capture scripts depend on, not an
/// oversight to tidy.
pub fn apply_qa_hooks(
    wins: &mut [WinState],
    desktop: egui::Rect,
    maximize_first_open: bool,
    minimize_next_three: bool,
) {
    if maximize_first_open && let Some(i) = wins.iter().position(|w| w.open) {
        workspace::maximize(&mut wins[i], desktop);
    }
    if minimize_next_three {
        for w in wins.iter_mut().skip(1).take(3) {
            w.minimized = true;
        }
    }
}

/// Seed one DOM window's view state from the resolved knobs: exactly the seeded fields move, and
/// an all-inert seed leaves the [`dom::DomState`] default byte-intact (the entry's CREATION is the
/// caller's `or_default`, which every DOM spawn triggers — see [`DomViewSeed`]).
pub fn seed_dom_view(dom_state: &mut dom::DomState, seed: &DomViewSeed) {
    if seed.elite {
        dom_state.mode = dom::DomMode::Elite;
    }
    if let Some(g) = seed.group {
        dom_state.group = g;
    }
    if let Some(v) = seed.venue {
        dom_state.venue = v;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feed_lifecycle::live_window_keys;
    use crate::window_spawn;
    use crate::workspace::series_key;
    use std::collections::HashSet;

    /// A deliberately NON-ZERO desktop origin, for the same reason `crate::window_spawn`'s test
    /// module uses one: a planner that dropped `desktop_min` on the floor would pass every
    /// position assertion below if the arena happened to start at (0, 0).
    const DESKTOP: egui::Pos2 = egui::pos2(12.0, 7.0);

    fn desk() -> egui::Rect {
        egui::Rect::from_min_size(DESKTOP, egui::vec2(800.0, 600.0))
    }

    fn chart(id: &str) -> WinState {
        let r = egui::Rect::from_min_size(egui::pos2(30.0, 40.0), egui::vec2(300.0, 200.0));
        WinState::new(id, "BTCUSDT", "1m", WinKind::Chart, r)
    }

    fn env_tool(slug: &str) -> ArrangeEnv {
        ArrangeEnv { tool: Some(slug.to_string()), ..ArrangeEnv::default() }
    }

    /// The `VIKE_ARRANGE` table, pinned INCLUDING the two rows that surprise: `"grid"` is not in
    /// the match (Grid is the catch-all, so the literal spelling was never recognised) and the
    /// parse is case-sensitive. Each row also pins that `remember` follows the fired mode through
    /// [`tiling_memory`].
    #[test]
    fn the_arrange_mode_table_pins_the_default_and_the_three_overrides() {
        for (raw, expect) in [
            (Some("tilev"), Arrange::TileV),
            (Some("tileh"), Arrange::TileH),
            (Some("cascade"), Arrange::Cascade),
            (None, Arrange::Grid),
            (Some("grid"), Arrange::Grid),
            (Some("TILEV"), Arrange::Grid),
        ] {
            let env = ArrangeEnv { arrange: raw.map(str::to_string), ..ArrangeEnv::default() };
            let wins = [chart("c0"), chart("c1")];
            let plan = plan_initial_arrange(&env, &wins, DESKTOP, 0);
            assert_eq!(plan.arrange, ArrangeAction::Arrange(expect), "mode for {raw:?}");
            assert_eq!(plan.remember, tiling_memory(expect), "remember for {raw:?}");
        }
    }

    /// All SIX [`Arrange`] variants through the shared rule: the three tiling modes remember
    /// themselves; Cascade opts out, and the two rail verbs (which arrive via the menu — the
    /// second call site) arrange nothing to remember.
    ///
    /// ⚠ SIX is THIS TABLE'S OWN count and nothing here checks it — a table cannot notice a
    /// variant it does not list. [`tiling_memory`]'s exhaustive match is what forces a seventh
    /// variant to name its side; this test pins which side each of today's six landed on, which
    /// is the half a compiler cannot answer.
    #[test]
    fn only_a_tiling_arrange_is_remembered() {
        for (mode, expect) in [
            (Arrange::TileV, Some(Arrange::TileV)),
            (Arrange::TileH, Some(Arrange::TileH)),
            (Arrange::Grid, Some(Arrange::Grid)),
            (Arrange::Cascade, None),
            (Arrange::MinimizeAll, None),
            (Arrange::RestoreAll, None),
        ] {
            assert_eq!(tiling_memory(mode), expect, "{mode:?}");
        }
    }

    /// The no-QA launch: 0 open windows touch nothing, exactly 1 maximizes instead of tiling, and
    /// ≥2 arrange — with an open-but-MINIMIZED window still counting toward the ≥2 arm, because
    /// the count tests `open` alone, exactly as the inline block did.
    #[test]
    fn an_ordinary_launch_arranges_by_how_many_windows_are_open() {
        let env = ArrangeEnv::default();

        let mut closed = chart("c0");
        closed.open = false;
        let plan = plan_initial_arrange(&env, std::slice::from_ref(&closed), DESKTOP, 7);
        assert_eq!(plan.arrange, ArrangeAction::None);
        assert!(plan.spawns.is_empty());
        assert!(!plan.close_existing);
        assert_eq!(plan.next_win_n, 7, "no spawn, no id burned");
        assert_eq!(plan.remember, None);

        let mut wins = [chart("c0"), chart("c1"), chart("c2")];
        wins[0].open = false;
        wins[2].open = false;
        let plan = plan_initial_arrange(&env, &wins, DESKTOP, 7);
        assert_eq!(plan.arrange, ArrangeAction::MaximizeLoneOpen);
        assert_eq!(plan.remember, None, "a lone-open maximize remembers nothing");

        let wins = [chart("c0"), chart("c1")];
        let plan = plan_initial_arrange(&env, &wins, DESKTOP, 7);
        assert_eq!(plan.arrange, ArrangeAction::Arrange(Arrange::Grid));
        assert_eq!(plan.remember, Some(Arrange::Grid));

        let mut wins = [chart("c0"), chart("c1")];
        wins[0].minimized = true;
        let plan = plan_initial_arrange(&env, &wins, DESKTOP, 7);
        assert_eq!(
            plan.arrange,
            ArrangeAction::Arrange(Arrange::Grid),
            "PINNED: the count tests `open` alone — minimized-but-open still counts"
        );
    }

    /// The single-tool capture: everything else closes, one window of the named kind opens AT THE
    /// DESKTOP ORIGIN at the generic tool size, and the post-close open count (exactly 1) selects
    /// the lone-open maximize. The counter assertion is EXACT, not `>=` — the discipline
    /// `crate::window_spawn`'s counter test argues for.
    #[test]
    fn a_qa_tool_capture_closes_the_desktop_and_opens_that_tool_at_the_origin() {
        let wins = [chart("c0"), chart("c1")];
        let plan = plan_initial_arrange(&env_tool("news"), &wins, DESKTOP, 4);
        assert!(plan.close_existing);
        assert_eq!(plan.spawns.len(), 1);
        let s = &plan.spawns[0];
        assert_eq!(s.win.kind, WinKind::News);
        assert_eq!(s.win.id, egui::Id::new("tool-4"));
        assert_eq!(s.win.pos, DESKTOP, "the desktop origin — no cascade slot");
        assert_eq!(s.win.size, egui::vec2(560.0, 400.0));
        assert_eq!(s.win.title, "News");
        assert!(s.ensure_feed.is_none() && s.ensure_depth.is_none());
        assert!(s.resolve_poly_token.is_none() && s.dom_view.is_none() && s.cal_page.is_none());
        assert_eq!(plan.arrange, ArrangeAction::MaximizeLoneOpen);
        assert_eq!(plan.next_win_n, 5, "EXACT: one id per spawned window");
    }

    /// A PIN, not an endorsement: an unknown slug opens the CALENDAR, the caller-chosen
    /// `unwrap_or` beside the explicit `WinKind::from_slug` parse.
    #[test]
    fn an_unknown_tool_slug_falls_back_to_the_calendar() {
        let plan = plan_initial_arrange(&env_tool("nope"), &[], DESKTOP, 0);
        assert_eq!(plan.spawns[0].win.kind, WinKind::Calendar);
    }

    /// **The invariant this module exists to hold, and the only test here that is not a pin.**
    /// Every [`WinKind`], driven through the `VIKE_TOOL` arm by its own slug: exactly the DOM
    /// ensures anything (a QA Chart backs NO feed — pinned, not endorsed; the launcher's Chart arm
    /// differs), and whatever IS ensured must be a key the REAL `live_window_keys` counts as
    /// backed by the REAL planned window — or `reap_orphaned_feeds` stops the stream on the frame
    /// after the window opens and the one-shot ensure never re-requests it.
    #[test]
    fn every_qa_tool_kind_is_classified_for_the_feed_it_ensures() {
        for kind in WinKind::ALL {
            let plan = plan_initial_arrange(&env_tool(kind.slug()), &[], DESKTOP, 2);
            assert_eq!(plan.spawns.len(), 1, "{kind:?}");
            let s = &plan.spawns[0];
            assert_eq!(s.win.kind, kind);
            if kind == WinKind::Dom {
                assert!(s.ensure_feed.is_some(), "the QA DOM must ensure its own bar feed");
                assert!(s.ensure_depth.is_some(), "…and its venue L2 depth stream");
            } else {
                assert!(s.ensure_feed.is_none(), "{kind:?}: a QA tool ensures no bar feed");
                assert!(s.ensure_depth.is_none(), "{kind:?}: nor a depth stream");
            }
            if let Some(f) = &s.ensure_feed {
                let key = series_key(&f.venue, &f.symbol, &f.interval);
                let live = live_window_keys(std::slice::from_ref(&s.win));
                assert!(
                    live.contains(&key),
                    "{kind:?}: ensures `{key}`, which no window backs — `reap_orphaned_feeds` \
                     stops it on the next frame and nothing re-requests it (live: {live:?})"
                );
            }
        }
    }

    /// The QA DOM's three hardcodings, pinned individually so the invariant above has a named row
    /// to point at when one of them moves: the symbol, the `(venue, symbol, interval)` of its bar
    /// feed (the venue-less default and the `"1m"` the reaper hardcodes for a DOM), and the
    /// Binance depth stream.
    #[test]
    fn the_qa_dom_seeds_symbol_feed_and_the_1m_the_reaper_hardcodes() {
        let plan = plan_initial_arrange(&env_tool("dom"), &[], DESKTOP, 0);
        let s = &plan.spawns[0];
        assert_eq!(s.win.symbol, "BTCUSDT");
        let f = s.ensure_feed.as_ref().expect("the QA DOM's own bar feed");
        assert_eq!(
            (f.venue.as_str(), f.symbol.as_str(), f.interval.as_str(), f.asset_class),
            (DEFAULT_VENUE, "BTCUSDT", "1m", None)
        );
        assert_eq!(s.ensure_depth, Some((dom::DomVenue::Binance, "BTCUSDT".to_string())));
        assert!(live_window_keys(std::slice::from_ref(&s.win)).contains("BTCUSDT@1m"));
    }

    /// The divergence `crate::window_spawn`'s module doc fenced, turned into an observation: the
    /// QA DOM and the launcher DOM at the SAME counter differ in id family (`tool-` vs `dom-`),
    /// in placement (origin vs cascade slot) and in size (generic tool vs narrow+tall). Folding
    /// the QA arm into the cascade planner reddens this test instead of silently renaming every
    /// QA DOM's persisted egui state.
    #[test]
    fn the_qa_dom_is_tool_prefixed_and_uncascaded_unlike_the_launcher_dom() {
        let qa_plan = plan_initial_arrange(&env_tool("dom"), &[], DESKTOP, 3);
        let qa = &qa_plan.spawns[0].win;
        let req =
            window_spawn::SpawnRequest::Kind { kind: WinKind::Dom, poly_seed_token: String::new() };
        let launcher = window_spawn::plan_spawn(req, DESKTOP, 3).win.expect("launcher DOM");
        assert_eq!(qa.id, egui::Id::new("tool-3"));
        assert_eq!(launcher.id, egui::Id::new("dom-3"));
        assert_eq!(qa.pos, DESKTOP, "QA: the desktop origin, no cascade");
        assert_ne!(qa.pos, launcher.pos, "the launcher burns a cascade slot");
        assert_eq!(qa.size, egui::vec2(560.0, 400.0), "QA: the generic tool size, even for a DOM");
        assert_ne!(qa.size, launcher.size, "the launcher's DOM is narrow+tall");
    }

    /// The cockpit seed ladder at THIS spawn site: an unset/whitespace token seeds the placeholder
    /// and asks for background Gamma resolution; a real token seeds trimmed and must NOT be
    /// resolved over (the resolve OVERWRITES `symbol`, and would move an operator's deliberately
    /// seeded window to a different market); a non-cockpit kind ignores the seed entirely.
    #[test]
    fn the_qa_cockpit_applies_the_seed_ladder_and_resolves_only_the_placeholder() {
        let plan = plan_initial_arrange(&env_tool("polymarket"), &[], DESKTOP, 1);
        let s = &plan.spawns[0];
        assert_eq!(s.win.symbol, POLY_PLACEHOLDER_TOKEN);
        assert_eq!(s.resolve_poly_token, Some(s.win.id));

        let env = ArrangeEnv {
            poly_cockpit_token: Some("  0xfeed  ".to_string()),
            ..env_tool("polymarket")
        };
        let plan = plan_initial_arrange(&env, &[], DESKTOP, 1);
        assert_eq!(plan.spawns[0].win.symbol, "0xfeed", "the ladder trims");
        assert_eq!(plan.spawns[0].resolve_poly_token, None, "a seeded token is never resolved");

        let env =
            ArrangeEnv { poly_cockpit_token: Some("   ".to_string()), ..env_tool("polymarket") };
        let plan = plan_initial_arrange(&env, &[], DESKTOP, 1);
        assert_eq!(plan.spawns[0].win.symbol, POLY_PLACEHOLDER_TOKEN, "whitespace-only = unset");
        assert!(plan.spawns[0].resolve_poly_token.is_some());

        let env = ArrangeEnv { poly_cockpit_token: Some("0xfeed".to_string()), ..env_tool("news") };
        let plan = plan_initial_arrange(&env, &[], DESKTOP, 1);
        assert_eq!(plan.spawns[0].win.symbol, "", "a non-cockpit kind ignores the seed");
        assert_eq!(plan.spawns[0].resolve_poly_token, None);
    }

    /// `VIKE_TOOLS`: the four capture tools in their pinned order, sequential `tool-{n}` ids, no
    /// extras, and the post-close open count (4) selects the arrange arm. `VIKE_TOOL` beats
    /// `VIKE_TOOLS` when both are set — the inline else-if, preserved.
    #[test]
    fn vike_tools_opens_the_four_capture_tools_and_tiles_them() {
        let wins = [chart("c0")];
        let env = ArrangeEnv { tools: true, ..ArrangeEnv::default() };
        let plan = plan_initial_arrange(&env, &wins, DESKTOP, 2);
        assert!(plan.close_existing);
        let kinds: Vec<WinKind> = plan.spawns.iter().map(|s| s.win.kind).collect();
        assert_eq!(kinds, [WinKind::Calendar, WinKind::Options, WinKind::News, WinKind::Data]);
        for (i, s) in plan.spawns.iter().enumerate() {
            assert_eq!(s.win.id, egui::Id::new(format!("tool-{}", 2 + i)), "sequential ids");
            assert_eq!(s.win.pos, DESKTOP);
            assert!(s.ensure_feed.is_none() && s.ensure_depth.is_none());
            assert!(s.resolve_poly_token.is_none() && s.dom_view.is_none());
            assert!(s.cal_page.is_none());
        }
        assert_eq!(plan.next_win_n, 6, "EXACT: four ids for four windows");
        assert_eq!(plan.arrange, ArrangeAction::Arrange(Arrange::Grid));

        let both = ArrangeEnv { tools: true, ..env_tool("news") };
        let plan = plan_initial_arrange(&both, &wins, DESKTOP, 2);
        assert_eq!(plan.spawns.len(), 1, "VIKE_TOOL beats VIKE_TOOLS");
        assert_eq!(plan.spawns[0].win.kind, WinKind::News);
    }

    /// The DOM view seeding table — parse, clamp, and the two PINS: `"binance"` is deliberately
    /// absent from the venue map (the default already IS Binance), and a garbage group assigns
    /// NOTHING (unlike `cal_page`'s zero fallback). Then the applier: exactly the seeded fields
    /// move onto a default [`dom::DomState`], and an all-inert seed leaves it byte-intact.
    #[test]
    fn dom_view_seeding_parses_clamps_and_leaves_the_default_for_garbage() {
        let seed_for = |mode: Option<&str>, group: Option<&str>, venue: Option<&str>| {
            let env = ArrangeEnv {
                dom_mode: mode.map(str::to_string),
                dom_group: group.map(str::to_string),
                dom_venue: venue.map(str::to_string),
                ..env_tool("dom")
            };
            let plan = plan_initial_arrange(&env, &[], DESKTOP, 0);
            plan.spawns[0].dom_view.clone().expect("every QA DOM carries a seed, inert or not")
        };
        assert!(seed_for(Some("elite"), None, None).elite);
        assert!(!seed_for(Some("pro"), None, None).elite);
        assert!(!seed_for(Some("ELITE"), None, None).elite, "case-sensitive, like the match");
        assert!(!seed_for(None, None, None).elite);

        assert_eq!(seed_for(None, Some("7"), None).group, Some(7));
        assert_eq!(seed_for(None, Some("0"), None).group, Some(1), "clamped to >= 1");
        assert_eq!(seed_for(None, Some("-3"), None).group, Some(1));
        assert_eq!(seed_for(None, Some("abc"), None).group, None, "garbage assigns nothing");
        assert_eq!(seed_for(None, None, None).group, None);

        assert_eq!(seed_for(None, None, Some("bybit")).venue, Some(dom::DomVenue::Bybit));
        assert_eq!(seed_for(None, None, Some("okx")).venue, Some(dom::DomVenue::Okx));
        assert_eq!(seed_for(None, None, Some("aster")).venue, Some(dom::DomVenue::Aster));
        assert_eq!(
            seed_for(None, None, Some("hyperliquid")).venue,
            Some(dom::DomVenue::Hyperliquid)
        );
        assert_eq!(
            seed_for(None, None, Some("binance")).venue,
            None,
            "PIN: absent from the map — the default already IS Binance"
        );
        assert_eq!(seed_for(None, None, Some("BYBIT")).venue, None, "case-sensitive");

        let plan = plan_initial_arrange(&env_tool("news"), &[], DESKTOP, 0);
        assert!(plan.spawns[0].dom_view.is_none(), "a non-DOM tool carries no seed at all");

        let mut st = dom::DomState::default();
        let seed = DomViewSeed { elite: true, group: Some(7), venue: Some(dom::DomVenue::Okx) };
        seed_dom_view(&mut st, &seed);
        assert_eq!(st.mode, dom::DomMode::Elite);
        assert_eq!(st.group, 7);
        assert_eq!(st.venue, dom::DomVenue::Okx);

        let mut st = dom::DomState::default();
        seed_dom_view(&mut st, &DomViewSeed { elite: false, group: None, venue: None });
        assert_eq!(st.mode, dom::DomMode::Pro, "an inert seed leaves the default standing");
        assert_eq!(st.group, 1);
        assert_eq!(st.venue, dom::DomVenue::Binance);
    }

    /// `VIKE_CAL_PAGE`'s error disposition, pinned: set-but-garbage still ASSIGNS page 0 (the
    /// inline `unwrap_or(0)`), unset assigns nothing — and the knob applies to ANY `VIKE_TOOL`
    /// kind, not just the Calendar, exactly as the inline block read it.
    #[test]
    fn cal_page_parses_with_a_zero_fallback_only_when_the_variable_is_set() {
        let page_for = |raw: Option<&str>| {
            let env = ArrangeEnv { cal_page: raw.map(str::to_string), ..env_tool("calendar") };
            plan_initial_arrange(&env, &[], DESKTOP, 0).spawns[0].cal_page
        };
        assert_eq!(page_for(Some("2")), Some(2));
        assert_eq!(page_for(Some("abc")), Some(0), "PIN: garbage still ASSIGNS page 0");
        assert_eq!(page_for(Some("300")), Some(0), "a u8 overflow is a failed parse, so 0 too");
        assert_eq!(page_for(None), None, "unset assigns nothing — the ToolView default stands");

        let env = ArrangeEnv { cal_page: Some("3".to_string()), ..env_tool("news") };
        assert_eq!(plan_initial_arrange(&env, &[], DESKTOP, 0).spawns[0].cal_page, Some(3));
    }

    /// The counter steps EXACTLY once per spawned window — the counter is the `egui::Id` seed, so
    /// a stalled step hands two windows one persisted state. Exact equalities, never `>=`: a `>=`
    /// would let a stalled counter pass by roster-order accident.
    #[test]
    fn the_window_counter_steps_once_per_spawned_window_and_never_reuses_an_id() {
        let plan = plan_initial_arrange(&ArrangeEnv::default(), &[], DESKTOP, 9);
        assert!(plan.spawns.is_empty());
        assert_eq!(plan.next_win_n, 9, "no spawn, no id burned");

        let plan = plan_initial_arrange(&env_tool("options"), &[], DESKTOP, 9);
        assert_eq!(plan.next_win_n, 10);

        let env = ArrangeEnv { tools: true, ..ArrangeEnv::default() };
        let plan = plan_initial_arrange(&env, &[], DESKTOP, 9);
        assert_eq!(plan.next_win_n, 13);
        let ids: HashSet<egui::Id> = plan.spawns.iter().map(|s| s.win.id).collect();
        assert_eq!(ids.len(), plan.spawns.len(), "two QA windows share an egui::Id");
    }

    /// The applier half of the lone-open rule: MaximizeLoneOpen maximizes the FIRST open window
    /// (pre-max rect captured, pending set to the desktop) and touches nothing else; `None`
    /// mutates nothing; a real arrange writes pending rects without maximizing. The tile geometry
    /// itself is `crate::workspace::arrange`'s own tested concern, not re-asserted here.
    #[test]
    fn maximize_lone_open_maximizes_the_first_open_window() {
        let mut wins = vec![chart("c0"), chart("c1")];
        wins[0].open = false;
        let old = egui::Rect::from_min_size(wins[1].pos, wins[1].size);
        apply_arrange_action(&mut wins, desk(), ArrangeAction::MaximizeLoneOpen);
        assert!(wins[1].maximized, "the first OPEN window maximizes");
        assert_eq!(wins[1].pending, Some(desk()));
        assert_eq!(wins[1].pre_max, Some(old));
        assert!(!wins[0].maximized, "the closed window is untouched");
        assert_eq!(wins[0].pending, None);

        let mut wins = vec![chart("c0"), chart("c1")];
        apply_arrange_action(&mut wins, desk(), ArrangeAction::None);
        assert!(wins.iter().all(|w| w.pending.is_none() && !w.maximized), "None mutates nothing");

        let mut wins = vec![chart("c0"), chart("c1")];
        apply_arrange_action(&mut wins, desk(), ArrangeAction::Arrange(Arrange::TileV));
        assert!(wins.iter().all(|w| w.pending.is_some() && !w.maximized));
    }

    /// The two QA hooks' index-vs-openness asymmetry, PINNED: `VIKE_MIN` minimizes indices 1..=3
    /// of the FULL list, open or not, while `VIKE_MAX` maximizes the first OPEN window, skipping
    /// closed ones. The capture scripts position windows by index; "improving" either rule moves
    /// their shots.
    #[test]
    fn the_qa_hooks_act_by_position_not_by_openness() {
        let mut wins: Vec<WinState> = (0..5).map(|i| chart(&format!("c{i}"))).collect();
        wins[0].open = false;
        wins[2].open = false;
        apply_qa_hooks(&mut wins, desk(), false, true);
        assert!(!wins[0].minimized, "index 0 is never minimized");
        assert!(wins[1].minimized && wins[2].minimized && wins[3].minimized, "1..=3, open or not");
        assert!(!wins[4].minimized, "…and exactly three");
        assert!(wins.iter().all(|w| !w.maximized), "the maximize hook was off");

        let mut wins = vec![chart("c0"), chart("c1")];
        wins[0].open = false;
        apply_qa_hooks(&mut wins, desk(), true, false);
        assert!(!wins[0].maximized, "index 0 is CLOSED, so the maximize hook skips it");
        assert!(wins[1].maximized, "the first OPEN window maximizes");
    }
}
