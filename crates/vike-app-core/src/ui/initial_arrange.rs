//! `initial_arrange` — the FIRST-FRAME layout decision: what the desktop does exactly once, on the
//! first frame whose window arena is wide enough to be real — which QA capture windows open and
//! with which feeds, whether the open windows are tiled or a lone one is maximized, and which of
//! the two QA hooks then run.
//!
//! Moved down out of `vike-app`'s `main.rs` — the `did_initial_arrange` block in `App`'s frame
//! loop — for the reason [`crate::ui::startup`] and [`crate::ui::window_spawn`] moved before it: that file
//! is in **no gate**. `xtask/src/ci/tables/roster.rs`'s `EXCLUDE_FROM_CI` names the shell (`vike-desktop`;
//! `vike-app` when this moved), so the DERIVED roster omits it, and the `app-check` job that DOES
//! compile the crate executes nothing in it beyond `crates/vike-desktop/src/chart_gpu.rs`'s
//! byte-layout pins and one font-binding test. This block is a
//! once-per-session branch over SEVEN environment knobs, and it is the STEP-2 row
//! [`crate::ui::window_spawn`]'s module doc deferred by name: the `VIKE_TOOL` / `VIKE_TOOLS` QA arms
//! are the frame loop's two window-opening sites that planner deliberately left behind.
//!
//! **A separate planner, NOT a seventh [`crate::ui::window_spawn::SpawnRequest`] arm — the fence is
//! behavioural.** The QA arms spend a `tool-{n}` id for EVERY kind (a Trade window opened this
//! way is `tool-N` where the launcher's is `trade-N`, and egui keys a window's whole persisted state
//! on that id), and they place every window at the DESKTOP ORIGIN at the one generic tool size
//! instead of burning a cascade slot. Folding them into [`crate::ui::window_spawn::plan_spawn`] would
//! silently rename and move every QA window, which is exactly the change that planner's module doc
//! forbade for its own move; `the_qa_trade_window_is_tool_prefixed_and_uncascaded_unlike_the_launcher_one`
//! compares the two planners' Trade spawns directly, so the divergence is an observation a fold has
//! to redden rather than a prose warning it can skip.
//!
//! **Pinned, not endorsed** — today's reality is declared with its flaws, per the capability-map
//! playbook's STEP 1, each behind a named test a fix must redden:
//!
//!   * An unknown `VIKE_TOOL` slug falls back to the CALENDAR — the caller-chosen `unwrap_or`
//!     beside the explicit [`WinKind::from_slug`] parse, spelled exactly as the inline block
//!     spelled it.
//!   * `VIKE_TOOL=chart` opens a TOOL-constructed chart — empty symbol, empty interval, NO feed
//!     ensured — unlike the launcher's Chart arm, which seeds BTCUSDT and subscribes its series. A
//!     capture pointed at it renders an empty pane, which is what it has always rendered.
//!   * A set-but-garbage `VIKE_CAL_PAGE` still ASSIGNS page 0 (a failed parse falls back to `0`),
//!     preserved byte-for-byte.
//!
//! No QA tool ensures a bar feed: the QA Trade window's book comes from the depth stream the window
//! loop requests every frame, so there is no one-shot ensure for the reaper to strand.
//! `every_qa_tool_kind_is_classified_for_the_feed_it_ensures` drives the REAL `live_window_keys` over
//! the REAL planned window for the next kind that does, never a restatement of its rule.
//!
//! **Data in, decisions out.** [`plan_initial_arrange`] is pure: the ALREADY-READ knob values
//! ([`ArrangeEnv`] — the `Injected` shape [`crate::ui::startup`]'s `StartupEnv` established), the
//! window list, the desktop origin and the window counter in; an [`InitialArrangePlan`] out. The
//! seven reads stay in `vike-desktop`'s `main.rs`, so every `VIKE_*` settings-registry row stays
//! classified `vike-desktop` / `Layer::Binary` (`vike-app` until the shell's rename re-keyed them),
//! and the tests below mutate no process environment. The
//! caller applies the plan in the order documented on [`InitialArrangePlan`]; the wins-plane
//! appliers ([`apply_arrange_action`], [`apply_qa_hooks`]) live beside the planner so the apply
//! stays a delegation, not a re-derivation.
//! [`tiling_memory`] is the shared "remember only tiling modes" rule — the menu's arrange arm and
//! this block carried two hand copies of it, the one-size-down instance of the drift
//! [`crate::ui::window_spawn`] exists to stop.

use crate::ui::startup::{FeedSpec, poly_cockpit_seed_token};
use crate::ui::tool_views::POLY_PLACEHOLDER_TOKEN;
use crate::ui::workspace::{self, Arrange, DEFAULT_VENUE, WinKind, WinState};

/// The first-frame knobs `vike-desktop`'s `main.rs` reads off the process environment and hands down.
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
    /// `VIKE_CAL_PAGE` — the QA tool window's calendar page (an equity page, for captures).
    pub cal_page: Option<String>,
    /// `VIKE_MAX` is present — maximize the first OPEN window after the arrange.
    pub max: bool,
    /// `VIKE_MIN` is present — minimize windows 1..=3 by INDEX after the arrange.
    pub min: bool,
    /// `VIKE_POLY_COCKPIT_TOKEN`, raw — the trim / empty / placeholder ladder
    /// ([`poly_cockpit_seed_token`]) is applied HERE, and only in the Polymarket arm, so the QA
    /// cockpit and [`crate::ui::startup`]'s capture cockpit cannot drift about what a seed means.
    pub poly_cockpit_token: Option<String>,
}

/// One QA tool window and everything the caller must do around its push.
///
/// No derives: [`WinState`] carries live egui dialog/pane state and is neither `Clone` nor
/// `Debug`. Built once, consumed once — the same rule as [`crate::ui::window_spawn::SpawnPlan`].
pub struct QaToolSpawn {
    /// The window to publish, already seeded (venue and symbol for a Trade window, the cockpit
    /// token ladder for a Polymarket).
    pub win: WinState,
    /// The bar series the window needs, as `App::ensure_feed_on`'s argument list — BEFORE the
    /// push, like every subscribing site.
    pub ensure_feed: Option<FeedSpec>,
    /// The cockpit window whose YES token is still the placeholder and therefore needs background
    /// Gamma resolution (`App::spawn_poly_token_resolver`) — AFTER the push, as the inline arm
    /// spawned it.
    pub resolve_poly_token: Option<egui::Id>,
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
/// 3. [`spawns`](Self::spawns), in order; for each: `ensure_feed` → push the window →
///    `resolve_poly_token` → `cal_page`.
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
/// and `next_win_n` the shared window counter, exactly as [`crate::ui::window_spawn::plan_spawn`]
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
        if k == WinKind::Trade {
            // The QA Trade window opens on Binance BTCUSDT so a capture has a book to draw.
            ws.venue = DEFAULT_VENUE.to_string();
            ws.symbol = "BTCUSDT".to_string();
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
        spawns.push(QaToolSpawn { win: ws, ensure_feed: None, resolve_poly_token, cal_page });
    } else if env.tools {
        for k in [WinKind::Calendar, WinKind::Options, WinKind::News, WinKind::Data] {
            let r = egui::Rect::from_min_size(desktop_min, egui::vec2(560.0, 400.0));
            let ws = WinState::tool(&format!("tool-{n}"), k, r);
            n += 1;
            spawns.push(QaToolSpawn {
                win: ws,
                ensure_feed: None,
                resolve_poly_token: None,
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

#[path = "initial_arrange_tests.rs"]
#[cfg(test)]
mod initial_arrange_tests;
