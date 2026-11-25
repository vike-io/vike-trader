//! `initial_arrange` — the FIRST-FRAME layout decision: what the desktop does exactly once, on the
//! first frame whose window arena is wide enough to be real — which QA capture windows open and
//! with which feeds, whether the open windows are tiled or a lone one is maximized, and which of
//! the two QA hooks then run.
//!
//! Moved down out of `vike-app`'s `main.rs` — the `did_initial_arrange` block in `App`'s frame
//! loop — for the reason [`crate::ui::startup`] and [`crate::ui::window::spawn`] moved before it: that file
//! is in **no gate**. `xtask/src/ci/tables/roster.rs`'s `EXCLUDE_FROM_CI` names the shell (`vike-desktop`;
//! `vike-app` when this moved), so the DERIVED roster omits it, and the `app-check` job that DOES
//! compile the crate executes nothing in it beyond `crates/vike-desktop/src/chart_gpu.rs`'s
//! byte-layout pins and one font-binding test. This block is a
//! once-per-session branch over SEVEN environment knobs, and it is the STEP-2 row
//! [`crate::ui::window::spawn`]'s module doc deferred by name: the `VIKE_TOOL` / `VIKE_TOOLS` QA arms
//! are the frame loop's two window-opening sites that planner deliberately left behind.
//!
//! **A separate planner, NOT a seventh [`crate::ui::window::spawn::WindowSpawnRequest`] arm — the fence is
//! behavioural.** The QA arms spend a `tool-{n}` id for EVERY kind (a Trade window opened this
//! way is `tool-N` where the launcher's is `trade-N`, and egui keys a window's whole persisted state
//! on that id), and they place every window at the DESKTOP ORIGIN at the one generic tool size
//! instead of burning a cascade slot. Folding them into [`crate::ui::window::spawn::plan_window_spawn`] would
//! silently rename and move every QA window, which is exactly the change that planner's module doc
//! forbade for its own move; `the_qa_trade_window_is_tool_prefixed_and_uncascaded_unlike_the_launcher_one`
//! compares the two planners' Trade spawns directly, so the divergence is an observation a fold has
//! to redden rather than a prose warning it can skip.
//!
//! **Pinned, not endorsed** — today's reality is declared with its flaws, per the capability-map
//! playbook's STEP 1, each behind a named test a fix must redden:
//!
//!   * An unknown `VIKE_TOOL` word is REFUSED, never read as another window: it opens no window,
//!     closes none, and says so at ERROR with every word it takes (`unknown_tool_refusal`). It fell
//!     back to the CALENDAR until 2026-11-18, so a retired word (`tearsheet`, once the window went into
//!     the Account window's History tab) silently opened the Calendar under its capture's name.
//!   * `VIKE_TOOL=chart` opens a TOOL-constructed chart — empty symbol, empty interval, NO feed
//!     ensured — unlike the launcher's Chart arm, which opens on the user's default series. A
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
//! ([`WindowInitialArrangeEnvironment`] — the `Injected` shape [`crate::ui::startup`]'s `StartupEnvironment` established), the
//! window list, the desktop origin and the window counter in; an [`WindowInitialArrangePlan`] out. The
//! seven reads stay in `vike-desktop`'s `main.rs`, so every `VIKE_*` settings-registry row stays
//! classified `vike-desktop` / `Layer::Binary` (`vike-app` until the shell's rename re-keyed them),
//! and the tests below mutate no process environment. The
//! caller applies the plan in the order documented on [`WindowInitialArrangePlan`]; the wins-plane
//! appliers ([`apply_arrange_action`], [`apply_qa_hooks`]) live beside the planner so the apply
//! stays a delegation, not a re-derivation.
//! [`tiling_memory`] is the shared "remember only tiling modes" rule — the menu's arrange arm and
//! this block carried two hand copies of it, the one-size-down instance of the drift
//! [`crate::ui::window::spawn`] exists to stop.

use crate::ui::startup::{SeriesSpec, polymarket_cockpit_seed_token};
use crate::ui::window::{self, WindowArrange, WindowKind, WindowState};

/// The first-frame knobs `vike-desktop`'s `main.rs` reads off the process environment and hands down.
///
/// Every field is the ALREADY-READ value, never a variable name — see the module doc for why the
/// read stays in the binary. `Default` is the "no knob set" configuration: no QA windows, the Grid
/// arrange mode, no hooks.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct WindowInitialArrangeEnvironment {
    /// `VIKE_ARRANGE` — `tilev` / `tileh` / `cascade`; anything else (unset included) means Grid.
    pub arrange: Option<String>,
    /// `VIKE_TOOL=<slug>` — close every existing window and open ONLY this one tool, for a clean
    /// maximized capture. Beats [`tools`](Self::tools) when both are set (the inline else-if).
    pub tool: Option<String>,
    /// `VIKE_TOOLS` is present — close the chart windows and open the four capture tools tiled.
    pub tools: bool,
    /// `VIKE_CAL_PAGE` — the QA tool window's calendar page (an equity page, for captures).
    pub calendar_page: Option<String>,
    /// `VIKE_MAX` is present — maximize the first OPEN window after the arrange.
    pub max: bool,
    /// `VIKE_MIN` is present — minimize windows 1..=3 by INDEX after the arrange.
    pub min: bool,
    /// `VIKE_POLY_COCKPIT_TOKEN`, raw — the trim / empty / placeholder ladder
    /// ([`polymarket_cockpit_seed_token`]) is applied HERE, and only in the Polymarket arm, so the QA
    /// cockpit and [`crate::ui::startup`]'s capture cockpit cannot drift about what a seed means.
    pub polymarket_cockpit_token: Option<String>,
}

/// One QA tool window and everything the caller must do around its push.
///
/// No derives: [`WindowState`] carries live egui dialog/pane state and is neither `Clone` nor
/// `Debug`. Built once, consumed once — the same rule as [`crate::ui::window::spawn::WindowSpawnPlan`].
pub struct WindowInitialArrangeQaSpawn {
    /// The window to publish, already seeded (venue and symbol for a Trade window, the cockpit
    /// token ladder for a Polymarket).
    pub window: WindowState,
    /// The bar series the window needs, as `App::ensure_feed_on`'s argument list — BEFORE the
    /// push, like every subscribing site.
    pub ensure_feed: Option<SeriesSpec>,
    /// `WindowView::calendar`'s `page`, assigned whenever `VIKE_CAL_PAGE` was set — for ANY tool kind,
    /// exactly as the inline block read it.
    pub calendar_page: Option<u8>,
}

/// What the first frame does about window GEOMETRY once the QA spawns (if any) are published.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowInitialArrangeAction {
    /// Two or more windows are open: tile/cascade them all with the resolved mode.
    Arrange(WindowArrange),
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
/// 2. [`next_window_number`](Self::next_window_number) — adopt the advanced counter (the inline arm advanced it
///    before its ensures too).
/// 3. [`spawns`](Self::spawns), in order; for each: `ensure_feed` → push the window → `calendar_page`.
/// 4. [`arrange`](Self::arrange) — [`apply_arrange_action`] over the post-spawn window list.
/// 5. [`remember`](Self::remember) — assign `last_arrange` so an app-maximize can re-fill.
/// 6. [`qa_maximize_first_open`](Self::qa_maximize_first_open) /
///    [`qa_minimize_next_three`](Self::qa_minimize_next_three) — [`apply_qa_hooks`], LAST.
///
/// No derives, same reason as [`WindowInitialArrangeQaSpawn`].
pub struct WindowInitialArrangePlan {
    /// Close every window that already exists (either QA arm is active).
    pub close_existing: bool,
    /// The QA tool windows to publish — one for `VIKE_TOOL`, four for `VIKE_TOOLS`, none
    /// otherwise.
    pub spawns: Vec<WindowInitialArrangeQaSpawn>,
    /// `App::next_window_number` after the spawns: advanced EXACTLY once per window, because the counter
    /// is the `egui::Id` seed and a stalled step hands two windows one persisted state.
    pub next_window_number: u32,
    /// The geometry decision, computed from the open count AS THE CALLER WILL LEAVE IT (spawns
    /// open, pre-existing windows closed when [`close_existing`](Self::close_existing)) — sound
    /// because [`WindowState::tool`] constructs `open: true`.
    pub arrange: WindowInitialArrangeAction,
    /// `Some` only when [`arrange`](Self::arrange) fired with a tiling mode — the inline block set
    /// `last_arrange` only inside its ≥2-open arm, and [`tiling_memory`] is the shared rule.
    pub remember: Option<WindowArrange>,
    /// `VIKE_MAX` — see [`apply_qa_hooks`].
    pub qa_maximize_first_open: bool,
    /// `VIKE_MIN` — see [`apply_qa_hooks`].
    pub qa_minimize_next_three: bool,
}

/// The shared "remember only tiling modes" rule: a tiling arrange is worth re-applying when the
/// app window later maximizes/restores, a Cascade is not (it opts out), and the two rail verbs
/// (MinimizeAll/RestoreAll) arrange nothing to remember. Two hand copies of this rule lived in
/// `vike-app`'s frame loop — the menu's arrange arm and the initial arrange — and both call sites
/// now delegate here, so the next mode added to [`WindowArrange`] gets classified once.
///
/// ⚠ An EXHAUSTIVE match, NOT the `matches!` both call sites spelled, and the difference is the
/// whole of what makes that last sentence true: `matches!` folds every unnamed variant into "not
/// remembered" silently, so a seventh [`WindowArrange`] would inherit that answer with nobody having
/// decided it — and `only_a_tiling_arrange_is_remembered` below is a HAND-WRITTEN table, which
/// cannot notice a variant it does not list either, so the two would agree and both be blind.
/// Spelled this way a new variant fails to COMPILE until it names its side, which is the
/// discipline [`WindowKind::slug`] already carries one file over for exactly this reason.
pub fn tiling_memory(a: WindowArrange) -> Option<WindowArrange> {
    match a {
        WindowArrange::TileVertical | WindowArrange::TileHorizontal | WindowArrange::Grid => {
            Some(a)
        }
        WindowArrange::Cascade | WindowArrange::MinimizeAll | WindowArrange::RestoreAll => None,
    }
}

/// Decide the whole first-frame layout. Pure: same inputs ⇒ same [`WindowInitialArrangePlan`].
///
/// `wins` is the window list AS OF the guard firing (only the open flags are read), `desktop_min`
/// the arena's top-left (`App::desktop`'s `min` — the QA arms place windows there, cascade-free),
/// and `next_window_number` the shared window counter, exactly as [`crate::ui::window::spawn::plan_window_spawn`]
/// takes them.
pub fn plan_initial_arrange(
    env: &WindowInitialArrangeEnvironment,
    wins: &[WindowState],
    desktop_min: egui::Pos2,
    next_window_number: u32,
) -> WindowInitialArrangePlan {
    let mode = match env.arrange.as_deref() {
        Some("tilev") => WindowArrange::TileVertical,
        Some("tileh") => WindowArrange::TileHorizontal,
        Some("cascade") => WindowArrange::Cascade,
        _ => WindowArrange::Grid,
    };
    // `VIKE_TOOL` still beats `VIKE_TOOLS` when both are set, a refused word included.
    let tool = env.tool.as_deref().map(|only| (only, WindowKind::from_slug(only)));
    if let Some((only, None)) = tool {
        tracing::error!("{}", unknown_tool_refusal(only));
    }
    let close_existing = match tool {
        Some((_, kind)) => kind.is_some(),
        None => env.tools,
    };
    let mut n = next_window_number;
    let mut spawns: Vec<WindowInitialArrangeQaSpawn> = Vec::new();
    if let Some((_, Some(k))) = tool {
        let r = egui::Rect::from_min_size(desktop_min, egui::vec2(560.0, 400.0));
        let mut ws = WindowState::tool(&format!("tool-{n}"), k, r);
        n += 1;
        // The QA Trade window opens with no instrument, exactly as the launcher's does: the glue
        // seeds the last-used one, or the backend's primary market, on its first frame. No symbol
        // is compiled in (the owner's ruling of 2026-11-13; it opened on Binance BTCUSDT until then).
        if k == WindowKind::Polymarket {
            // The env token or the placeholder — the SAME ladder the launcher
            // and capture arms apply, so the three cockpit seeding paths cannot drift.
            ws.symbol = polymarket_cockpit_seed_token(env.polymarket_cockpit_token.as_deref());
        }
        // QA: open an equity page. Read for ANY tool kind, as inline; a failed parse ASSIGNS 0.
        let calendar_page = env.calendar_page.as_deref().map(|pg| pg.parse::<u8>().unwrap_or(0));
        spawns.push(WindowInitialArrangeQaSpawn { window: ws, ensure_feed: None, calendar_page });
    } else if tool.is_none() && env.tools {
        for k in [WindowKind::Calendar, WindowKind::Options, WindowKind::News, WindowKind::Data] {
            let r = egui::Rect::from_min_size(desktop_min, egui::vec2(560.0, 400.0));
            let ws = WindowState::tool(&format!("tool-{n}"), k, r);
            n += 1;
            spawns.push(WindowInitialArrangeQaSpawn {
                window: ws,
                ensure_feed: None,
                calendar_page: None,
            });
        }
    }
    // The open count AS THE CALLER WILL LEAVE IT: every spawn opens `true` and `close_existing`
    // zeroes every pre-existing flag, so the two worlds agree by construction. The count tests
    // `open` ALONE — an open-but-minimized window still counts, exactly as inline.
    let open_windows =
        if close_existing { spawns.len() } else { wins.iter().filter(|w| w.open).count() };
    // Only arrange when there are ≥2 windows to actually tile; a lone open window maximizes
    // instead (see `WindowInitialArrangeAction::MaximizeLoneOpen` for the bug that rule exists to avoid).
    let arrange = if open_windows > 1 {
        WindowInitialArrangeAction::Arrange(mode)
    } else if open_windows == 1 {
        WindowInitialArrangeAction::MaximizeLoneOpen
    } else {
        WindowInitialArrangeAction::None
    };
    let remember = match arrange {
        WindowInitialArrangeAction::Arrange(m) => tiling_memory(m),
        _ => None,
    };
    WindowInitialArrangePlan {
        close_existing,
        spawns,
        next_window_number: n,
        arrange,
        remember,
        qa_maximize_first_open: env.max,
        qa_minimize_next_three: env.min,
    }
}

/// What the log says, at ERROR, when `VIKE_TOOL` names no window: the word, that nothing opens, and
/// every word it does take (each kind's [`WindowKind::slug`]).
pub(crate) fn unknown_tool_refusal(word: &str) -> String {
    let words: Vec<&str> = WindowKind::ALL.iter().map(|k| k.slug()).collect();
    format!(
        "VIKE_TOOL={word:?} names no window, so no QA window opens and none closes. It takes one of: {}.",
        words.join(", ")
    )
}

/// Apply the geometry decision over the (post-spawn) window list — the wins-plane half the caller
/// delegates back here, so the lone-open resolution stays beside the rule that produced it.
pub fn apply_arrange_action(
    wins: &mut [WindowState],
    desktop: egui::Rect,
    action: WindowInitialArrangeAction,
) {
    match action {
        WindowInitialArrangeAction::Arrange(mode) => window::apply_arrange(wins, desktop, mode),
        WindowInitialArrangeAction::MaximizeLoneOpen => {
            if let Some(i) = wins.iter().position(|w| w.open) {
                window::maximize(&mut wins[i], desktop);
            }
        }
        WindowInitialArrangeAction::None => {}
    }
}

/// The two QA hooks, applied AFTER the arrange in this order: `VIKE_MAX` maximizes the first OPEN
/// window (a tool in `VIKE_TOOLS` mode), then `VIKE_MIN` minimizes windows 1..=3 **by index over
/// the full list, open or not** — a pinned asymmetry the capture scripts depend on, not an
/// oversight to tidy.
pub fn apply_qa_hooks(
    wins: &mut [WindowState],
    desktop: egui::Rect,
    maximize_first_open: bool,
    minimize_next_three: bool,
) {
    if maximize_first_open && let Some(i) = wins.iter().position(|w| w.open) {
        window::maximize(&mut wins[i], desktop);
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
