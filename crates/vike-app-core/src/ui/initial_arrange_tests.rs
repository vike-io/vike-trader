use super::*;
use crate::ui::feed_lifecycle::live_window_keys;
use crate::ui::window_spawn;
use crate::ui::workspace::series_key;
use std::collections::HashSet;

/// A deliberately NON-ZERO desktop origin, for the same reason `crate::ui::window_spawn`'s test
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
/// `crate::ui::window_spawn`'s counter test argues for.
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
    assert!(s.ensure_feed.is_none());
    assert!(s.resolve_poly_token.is_none() && s.cal_page.is_none());
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
/// Every [`WinKind`], driven through the `VIKE_TOOL` arm by its own slug: no QA tool ensures
/// anything (a QA Chart backs NO feed — pinned, not endorsed; the launcher's Chart arm differs),
/// and whatever IS ensured — the invariant for the next kind that does — must be a key the REAL
/// `live_window_keys` counts as backed by the REAL planned window — or `reap_orphaned_feeds`
/// stops the stream on the frame after the window opens and the one-shot ensure never
/// re-requests it.
#[test]
fn every_qa_tool_kind_is_classified_for_the_feed_it_ensures() {
    for kind in WinKind::ALL {
        let plan = plan_initial_arrange(&env_tool(kind.slug()), &[], DESKTOP, 2);
        assert_eq!(plan.spawns.len(), 1, "{kind:?}");
        let s = &plan.spawns[0];
        assert_eq!(s.win.kind, kind);
        assert!(s.ensure_feed.is_none(), "{kind:?}: a QA tool ensures no bar feed");
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

/// The QA Trade window's one hardcoding: it opens on Binance BTCUSDT so a capture has a book to
/// draw. It ensures no bar feed (Ruling R6); the window loop requests its depth stream.
#[test]
fn the_qa_trade_window_opens_on_binance_btcusdt_with_no_bar_feed() {
    let plan = plan_initial_arrange(&env_tool("trade"), &[], DESKTOP, 0);
    let s = &plan.spawns[0];
    assert_eq!((s.win.venue.as_str(), s.win.symbol.as_str()), (DEFAULT_VENUE, "BTCUSDT"));
    assert!(s.ensure_feed.is_none());
    assert!(live_window_keys(std::slice::from_ref(&s.win)).is_empty());
}

/// The divergence `crate::ui::window_spawn`'s module doc fenced, turned into an observation: the
/// QA Trade window and the launcher's at the SAME counter differ in id family (`tool-` vs
/// `trade-`), in placement (origin vs cascade slot) and in size (generic tool vs 600 × 560).
/// Folding the QA arm into the cascade planner reddens this test instead of silently renaming
/// every QA Trade window's persisted egui state.
#[test]
fn the_qa_trade_window_is_tool_prefixed_and_uncascaded_unlike_the_launcher_one() {
    let qa_plan = plan_initial_arrange(&env_tool("trade"), &[], DESKTOP, 3);
    let qa = &qa_plan.spawns[0].win;
    let req = window_spawn::SpawnRequest::Kind {
        kind: WinKind::Trade,
        poly_seed_token: String::new(),
        data_dest_seed: None,
    };
    // The launcher's size is measured in the look by its caller; the spec's is the smallest.
    let opens = vike_ui_theme::value::trade::BESIDE_SIZE;
    let launcher =
        window_spawn::plan_spawn(req, DESKTOP, 3, opens).win.expect("launcher Trade window");
    assert_eq!(qa.id, egui::Id::new("tool-3"));
    assert_eq!(launcher.id, egui::Id::new("trade-3"));
    assert_eq!(qa.pos, DESKTOP, "QA: the desktop origin, no cascade");
    assert_ne!(qa.pos, launcher.pos, "the launcher burns a cascade slot");
    assert_eq!(qa.size, egui::vec2(560.0, 400.0), "QA: the generic tool size");
    assert_ne!(qa.size, launcher.size, "the launcher opens at the Trade window's own size");
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

    let env =
        ArrangeEnv { poly_cockpit_token: Some("  0xfeed  ".to_string()), ..env_tool("polymarket") };
    let plan = plan_initial_arrange(&env, &[], DESKTOP, 1);
    assert_eq!(plan.spawns[0].win.symbol, "0xfeed", "the ladder trims");
    assert_eq!(plan.spawns[0].resolve_poly_token, None, "a seeded token is never resolved");

    let env = ArrangeEnv { poly_cockpit_token: Some("   ".to_string()), ..env_tool("polymarket") };
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
        assert!(s.ensure_feed.is_none());
        assert!(s.resolve_poly_token.is_none());
        assert!(s.cal_page.is_none());
    }
    assert_eq!(plan.next_win_n, 6, "EXACT: four ids for four windows");
    assert_eq!(plan.arrange, ArrangeAction::Arrange(Arrange::Grid));

    let both = ArrangeEnv { tools: true, ..env_tool("news") };
    let plan = plan_initial_arrange(&both, &wins, DESKTOP, 2);
    assert_eq!(plan.spawns.len(), 1, "VIKE_TOOL beats VIKE_TOOLS");
    assert_eq!(plan.spawns[0].win.kind, WinKind::News);
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
/// itself is `crate::ui::workspace::arrange`'s own tested concern, not re-asserted here.
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
