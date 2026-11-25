//! **The Settings window's Appearance section, driven end to end** (design system spec §5).
//!
//! `vike-desktop` is outside the derived CI roster, so the section's decisions live in
//! `vike-app-core` and are gated here through the REAL `settings_tool_content` and the real
//! `ThemeAppearanceSession`, read back off the accessibility tree and the egui style. Every write lands
//! in a `tempfile::tempdir()` holding a PLANTED settings database — never a real one.

use crate::a11y;
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use vike_app_core::test_support::{planted, preference};
use vike_app_core::ui::appearance_settings::{
    BUSY_NOTE, CARBON_COLOURBLIND_NOTE, GPU_CANDLES_LABEL, GPU_UNAVAILABLE, MIGRATION_NOTE,
    NO_DATABASE_NOTE, SAVED_NOTE, ThemeAppearanceSession, VSYNC_LABEL,
};
use vike_app_core::ui::claude_connect::{
    ClaudeConnectView, ClaudePlatform, ClaudeStatus, MAYBE_RESTART, NEXT_START, RESTART_CLAUDE,
};
use vike_app_core::ui::windows::{CONNECT_CLAUDE, settings_tool_content};
use vike_chart::chart::state::ChartState;
use vike_ui_theme::appearance::{ThemeAppearance, current, install};
use vike_ui_theme::metrics::ThemeDensity;
use vike_ui_theme::theme::{ThemePalette, ThemePaletteId};
use vike_ui_theme::time_zone::ThemeDisplayTimeZone;
use vike_ui_theme::type_scale::ThemeTextSize;

struct Screen {
    harness: Harness<'static, ()>,
    session: Arc<Mutex<ThemeAppearanceSession>>,
}

impl Screen {
    fn new(start: ThemeAppearance, dir: Option<&Path>) -> Self {
        Self::opened_with_vsync(start, dir, true)
    }

    /// A screen whose window was opened with `vsync` as its present mode.
    fn opened_with_vsync(start: ThemeAppearance, dir: Option<&Path>, vsync: bool) -> Self {
        let session =
            ThemeAppearanceSession::new(start, dir.map(Path::to_path_buf)).with_vsync(vsync);
        Self::of(start, session)
    }

    /// A screen over `session`, whose look is `start`; its AI section shows a connected Claude.
    fn of(start: ThemeAppearance, session: ThemeAppearanceSession) -> Self {
        let claude = ClaudeConnectView::showing(ClaudeStatus::Connected);
        Self::with_claude(start, session, claude, HashMap::new())
    }

    /// A screen over `session` whose AI section is `claude`, looking for Claude Desktop in `vars`.
    fn with_claude(
        start: ThemeAppearance,
        session: ThemeAppearanceSession,
        claude: ClaudeConnectView,
        vars: HashMap<String, String>,
    ) -> Self {
        let session = Arc::new(Mutex::new(session));
        let inner = Arc::clone(&session);
        let mut claude = claude;
        // 1400 tall: the Display and AI sections sit below Text size, and a control clipped out of
        // the screen cannot be clicked.
        let mut harness =
            Harness::builder().with_size(egui::vec2(760.0, 1400.0)).build_ui(move |ui| {
                let mut s = inner.lock().unwrap();
                settings_tool_content(ui, &mut s, &mut claude, &vars);
                // The shell's post-loop drain, as `crates/vike-desktop/src/app_ui/mod.rs`'s
                // `draw_windows` performs it (minus the journal, which the session's own tests
                // cover).
                let context = ui.ctx().clone();
                s.apply_pending(&context, None, 0);
            });
        install(&harness.ctx, &start);
        harness.run();
        Self { harness, session }
    }

    fn session(&self) -> MutexGuard<'_, ThemeAppearanceSession> {
        self.session.lock().unwrap()
    }

    /// Every accessibility node's text — labels file it under `value`, buttons under `label`.
    fn texts(&self) -> Vec<String> {
        a11y::texts(&self.harness)
    }

    fn contains(&self, needle: &str) -> bool {
        self.texts().iter().any(|t| t.contains(needle))
    }

    /// The role of the node named exactly `name`: `Label` for a selected option, `Button` for
    /// another, `CheckBox` for the gradient.
    fn role(&self, name: &str) -> Option<Role> {
        self.harness
            .root()
            .children_recursive()
            .find(|n| {
                let a = n.accesskit_node();
                a.label().as_deref() == Some(name) || a.value().as_deref() == Some(name)
            })
            .map(|n| n.accesskit_node().role())
    }

    fn click(&mut self, name: &str) {
        self.harness.get_by_label(name).click();
        self.harness.run();
    }
}

#[test]
fn every_option_is_on_screen_and_the_current_one_is_a_label() {
    let screen = Screen::new(ThemeAppearance::default(), None);
    for name in [
        "Graphite",
        "Midnight",
        "Dusk",
        "Carbon",
        "Classic",
        "TradingView",
        "Exchange",
        "Colour-blind",
        "Compact",
        "Normal",
        "Comfortable",
        "Standard",
        "Large",
    ] {
        assert!(screen.role(name).is_some(), "{name} is not on screen: {:?}", screen.texts());
    }
    for selected in ["Graphite", "Classic", "Normal", "Standard"] {
        assert_eq!(screen.role(selected), Some(Role::Label), "{selected} is the current choice");
    }
    for other in ["Midnight", "TradingView", "Compact", "Large"] {
        assert_eq!(screen.role(other), Some(Role::Button), "{other} is a choice");
    }
    assert_eq!(screen.role("Gradient in window headers"), Some(Role::CheckBox));
    assert!(screen.contains(MIGRATION_NOTE), "{:?}", screen.texts());
}

/// Review Focus 4: the next frame already carries the new style — no further input needed.
#[test]
fn a_click_applies_the_theme_and_saves_it() {
    let dir = planted(tempfile::tempdir().expect("tempdir"));
    let mut screen = Screen::new(ThemeAppearance::default(), Some(dir.path()));
    screen.click("Midnight");
    assert_eq!(screen.session().appearance().theme, ThemePaletteId::Midnight);
    assert_eq!(current(&screen.harness.ctx).theme, ThemePaletteId::Midnight);
    let fill = screen.harness.ctx.global_style().visuals.panel_fill;
    assert_eq!(fill, ThemePalette::of(ThemePaletteId::Midnight).background);
    assert_eq!(preference(dir.path(), "theme").as_deref(), Some("\"midnight\""));
    assert!(screen.contains(SAVED_NOTE), "{:?}", screen.texts());
    assert_eq!(screen.role("Midnight"), Some(Role::Label), "the new choice reads as selected");
    assert_eq!(screen.role("Graphite"), Some(Role::Button));
}

#[test]
fn density_text_size_and_the_gradient_apply_and_save() {
    let dir = planted(tempfile::tempdir().expect("tempdir"));
    let mut screen = Screen::new(ThemeAppearance::default(), Some(dir.path()));
    screen.click("Large");
    screen.click("Compact");
    screen.click("Gradient in window headers");
    let a = screen.session().appearance();
    assert_eq!(
        (a.text_size, a.density, a.header_gradient),
        (ThemeTextSize::Large, ThemeDensity::Compact, true)
    );
    let style = screen.harness.ctx.global_style();
    assert_eq!(style.text_styles[&egui::TextStyle::Body].size, 14.0);
    assert_eq!(style.spacing.item_spacing, egui::vec2(4.0, 4.0));
    assert_eq!(preference(dir.path(), "text_size").as_deref(), Some("\"large\""));
    assert_eq!(preference(dir.path(), "density").as_deref(), Some("\"compact\""));
    assert_eq!(preference(dir.path(), "header_gradient").as_deref(), Some("true"));
}

#[test]
fn the_carbon_note_appears_for_carbon_with_colour_blind_only() {
    let mut screen = Screen::new(
        ThemeAppearance { theme: ThemePaletteId::Carbon, ..ThemeAppearance::default() },
        None,
    );
    assert!(!screen.contains(CARBON_COLOURBLIND_NOTE));
    screen.click("Colour-blind");
    assert!(screen.contains(CARBON_COLOURBLIND_NOTE), "{:?}", screen.texts());
    screen.click("Classic");
    assert!(!screen.contains(CARBON_COLOURBLIND_NOTE));
}

/// Review Focus 1.
#[test]
fn with_no_database_a_click_applies_and_says_it_was_not_saved() {
    let dir = tempfile::tempdir().expect("a project directory with NO database");
    let mut screen = Screen::new(ThemeAppearance::default(), Some(dir.path()));
    // Final review, Important 2: the line naming where these settings live must not claim a
    // database that does not exist — before a click, or after one.
    assert!(!screen.contains("they are saved in"), "{:?}", screen.texts());
    assert!(screen.contains("lasts until the app closes"), "{:?}", screen.texts());
    screen.click("Dusk");
    assert_eq!(current(&screen.harness.ctx).theme, ThemePaletteId::Dusk, "applied anyway");
    assert!(screen.contains(NO_DATABASE_NOTE), "{:?}", screen.texts());
    assert!(!screen.contains("they are saved in"), "{:?}", screen.texts());
    assert!(!vike_secrets::db_path_in(dir.path()).exists(), "the app never creates the store");
}

/// Final review, Important 1: a save refused because another writer held the store offers
/// "Save again", and pressing it saves the look already in force.
#[test]
fn a_busy_save_offers_save_again_and_it_saves() {
    let dir = planted(tempfile::tempdir().expect("tempdir"));
    let mut screen = Screen::new(ThemeAppearance::default(), Some(dir.path()));
    assert_eq!(screen.role("Save again"), None, "nothing is unsaved yet");
    let lock = vike_secrets::hold_write_lock(dir.path());
    screen.click("Midnight");
    assert!(screen.contains(BUSY_NOTE), "{:?}", screen.texts());
    assert_eq!(screen.role("Save again"), Some(Role::Button), "{:?}", screen.texts());
    drop(lock);
    screen.click("Save again");
    assert_eq!(preference(dir.path(), "theme").as_deref(), Some("\"midnight\""));
    assert!(screen.contains(SAVED_NOTE), "{:?}", screen.texts());
    assert_eq!(screen.role("Save again"), None, "saved: nothing left to save");
}

/// The Display section's vsync control, driven end to end: a checkbox that says "requires restart"
/// before it is touched, a click that SAVES the row and does not change what is running, a note
/// that a restart is owed, and a second click that puts it back.
#[test]
fn the_vsync_checkbox_saves_a_row_and_says_a_restart_is_owed() {
    let dir = planted(tempfile::tempdir().expect("tempdir"));
    let mut screen = Screen::new(ThemeAppearance::default(), Some(dir.path()));
    assert_eq!(screen.role(VSYNC_LABEL), Some(Role::CheckBox), "{:?}", screen.texts());
    assert!(screen.contains("144 frames a second"), "{:?}", screen.texts());
    assert!(!screen.contains("this run keeps"), "nothing is owed yet: {:?}", screen.texts());

    screen.click(VSYNC_LABEL);
    assert_eq!(preference(dir.path(), "vsync").as_deref(), Some("false"));
    assert!(!screen.session().vsync());
    assert!(screen.session().vsync_in_force(), "the running window keeps its present mode");
    assert!(
        screen.contains("this run keeps vertical sync on"),
        "a restart is owed: {:?}",
        screen.texts()
    );
    assert!(screen.contains(SAVED_NOTE), "{:?}", screen.texts());
    assert_eq!(
        current(&screen.harness.ctx),
        ThemeAppearance::default(),
        "a vsync choice changes no look"
    );

    screen.click(VSYNC_LABEL);
    assert_eq!(preference(dir.path(), "vsync").as_deref(), Some("true"));
    assert!(!screen.contains("this run keeps"), "back to what is running: {:?}", screen.texts());
}

/// A window opened with vsync OFF shows the box unticked and owes nothing; ticking it is what is
/// saved, and the note then names the mode the running window keeps.
#[test]
fn a_window_opened_with_vsync_off_owes_no_restart_until_the_box_is_ticked() {
    let dir = planted(tempfile::tempdir().expect("tempdir"));
    let mut screen = Screen::opened_with_vsync(ThemeAppearance::default(), Some(dir.path()), false);
    // One lock for both reads: the session sits behind a std `Mutex`, which is not reentrant.
    let (chosen, owed) = {
        let s = screen.session();
        (s.vsync(), s.vsync_needs_restart())
    };
    assert!(!chosen && !owed);
    assert!(!screen.contains("this run keeps"), "{:?}", screen.texts());

    screen.click(VSYNC_LABEL);
    assert_eq!(preference(dir.path(), "vsync").as_deref(), Some("true"));
    assert!(
        screen.contains("this run keeps vertical sync off"),
        "the note names the mode the window keeps: {:?}",
        screen.texts()
    );
}

/// Review Focus 3.
#[test]
fn the_screen_says_whose_settings_these_are() {
    let dir = planted(tempfile::tempdir().expect("tempdir"));
    let screen = Screen::new(ThemeAppearance::default(), Some(dir.path()));
    let db = vike_secrets::db_path_in(dir.path()).display().to_string();
    assert!(screen.contains(&db), "{:?}", screen.texts());
    assert!(screen.contains("not on the backend"), "{:?}", screen.texts());
}

/// The GPU candle control (moved from the caption), driven end to end from a fresh database's
/// defaults: ticked, the chart layer on; a click turns the layer off at once and saves the row; a
/// second click puts both back.
#[test]
fn the_gpu_candle_checkbox_flips_the_chart_layer_and_saves_its_row() {
    let dir = planted(tempfile::tempdir().expect("tempdir"));
    let fresh = vike_config::Preferences::default();
    let session =
        ThemeAppearanceSession::new(ThemeAppearance::default(), Some(dir.path().to_path_buf()))
            .with_display((fresh.gpu_candles, &fresh.display_timezone), true);
    let mut screen = Screen::of(ThemeAppearance::default(), session);
    assert_eq!(screen.role(GPU_CANDLES_LABEL), Some(Role::CheckBox), "{:?}", screen.texts());
    assert!(screen.contains("tens of thousands of candles"), "{:?}", screen.texts());
    assert!(!screen.contains(GPU_UNAVAILABLE), "{:?}", screen.texts());
    assert!(screen.session().draws_gpu_candles(), "a fresh database draws candles on the GPU");

    screen.click(GPU_CANDLES_LABEL);
    assert!(!screen.session().draws_gpu_candles(), "off from the next frame, no restart");
    assert_eq!(preference(dir.path(), "gpu_candles").as_deref(), Some("false"));
    assert!(screen.contains(SAVED_NOTE), "{:?}", screen.texts());

    screen.click(GPU_CANDLES_LABEL);
    assert!(screen.session().draws_gpu_candles());
    assert_eq!(preference(dir.path(), "gpu_candles").as_deref(), Some("true"));
}

/// With no GPU candle path the control is disabled and the screen says why, in the words the
/// caption's toggle used.
#[test]
fn with_no_gpu_path_the_candle_checkbox_is_disabled_and_says_why() {
    let session = ThemeAppearanceSession::new(ThemeAppearance::default(), None)
        .with_display((true, "Local"), false);
    let screen = Screen::of(ThemeAppearance::default(), session);
    assert!(screen.contains(GPU_UNAVAILABLE), "{:?}", screen.texts());
    let node = screen.harness.get_by_label(GPU_CANDLES_LABEL);
    assert!(node.accesskit_node().is_disabled(), "the box cannot be ticked without a GPU path");
    assert!(!screen.session().draws_gpu_candles());
}

/// The time zone (moved from View → Timezone) is a row of the Display section: every zone the menu
/// had, Local selected on a fresh session. A click puts the app in that zone at once and saves the
/// row, and a chart the shell re-times that frame (`ThemeAppearanceSession::retime`) draws its axis in it.
#[test]
fn a_time_zone_click_saves_its_row_and_moves_the_chart_axis_labels() {
    let dir = planted(tempfile::tempdir().expect("tempdir"));
    let mut screen = Screen::new(ThemeAppearance::default(), Some(dir.path()));
    for zone in ThemeDisplayTimeZone::CURATED {
        assert!(screen.role(zone.label()).is_some(), "{zone:?}: {:?}", screen.texts());
    }
    assert_eq!(screen.role("Local"), Some(Role::Label), "Local is the default");
    // One-minute bars from 2023-11-14 22:13:20 UTC, drawn in UTC: the 23:00 hour is labelled.
    let mut chart = vike_chart::test_support::make_state(vike_chart::test_support::flat_bars(120));
    let utc = axis_texts(&chart);
    assert!(utc.iter().any(|t| t == "23:00"), "{utc:?}");

    screen.click("Tokyo");
    let tokyo = ThemeDisplayTimeZone::parse("Asia/Tokyo");
    assert_eq!(screen.role("Tokyo"), Some(Role::Label), "{:?}", screen.texts());
    assert_eq!(screen.session().display_time_zone(), tokyo);
    assert_eq!(preference(dir.path(), "display_timezone").as_deref(), Some("\"Asia/Tokyo\""));
    screen.session().retime([&mut chart]);
    assert_eq!(chart.time_zone(), tokyo);
    let labels = axis_texts(&chart);
    assert!(labels.iter().any(|t| t == "08:00"), "23:00 UTC is 08:00 in Tokyo: {labels:?}");
    assert!(!labels.iter().any(|t| t == "23:00"), "{labels:?}");
}

/// Settings > AI, end to end over a planted Windows profile (a classic Claude Desktop whose config
/// holds another server): the section says not connected and offers Connect Claude; the click merges
/// the entry into Claude's file beside the other server, keeps a backup, and the section then says
/// connected, with the line about what to do next.
#[test]
fn connect_claude_merges_the_entry_into_claudes_config_and_says_connected() {
    let profile = tempfile::tempdir().expect("tempdir");
    let claude_dir = profile.path().join("Roaming").join("Claude");
    std::fs::create_dir_all(&claude_dir).expect("mkdir");
    let config = claude_dir.join("claude_desktop_config.json");
    std::fs::write(&config, r#"{"mcpServers": {"other": {"command": "x"}}}"#).expect("plant");
    let vars = HashMap::from([
        ("APPDATA".to_string(), profile.path().join("Roaming").display().to_string()),
        ("LOCALAPPDATA".to_string(), profile.path().join("Local").display().to_string()),
    ]);
    let settings = profile.path().join("project").join("settings");
    let session = ThemeAppearanceSession::new(ThemeAppearance::default(), Some(settings.clone()));
    let claude = ClaudeConnectView::on(ClaudePlatform::Windows);
    let mut screen = Screen::with_claude(ThemeAppearance::default(), session, claude, vars);
    assert!(screen.contains("Claude is not connected to this app."), "{:?}", screen.texts());
    assert_eq!(screen.role(CONNECT_CLAUDE), Some(Role::Button));

    screen.click(CONNECT_CLAUDE);
    let written: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&config).expect("the file")).expect("json");
    let exe = std::env::current_exe().expect("exe").display().to_string();
    let settings = settings.display().to_string();
    assert_eq!(
        written,
        serde_json::json!({ "mcpServers": {
            "other": { "command": "x" },
            "vike": { "command": exe, "args": ["--mcp"], "env": { "VIKE_SETTINGS_DIR": settings } },
        } })
    );
    let backups = std::fs::read_dir(&claude_dir).expect("list").count();
    assert_eq!(backups, 2, "the file and its one backup");
    assert!(screen.contains("Claude is connected to this app."), "{:?}", screen.texts());
    let next = [NEXT_START, RESTART_CLAUDE, MAYBE_RESTART];
    assert!(next.iter().any(|line| screen.contains(line)), "{:?}", screen.texts());
    assert_eq!(screen.role(CONNECT_CLAUDE), None, "a connected Claude is offered no button");
}

/// Every text one headless chart frame paints (the third, once the plot has settled its bounds).
fn axis_texts(state: &ChartState) -> Vec<String> {
    use vike_app_core::ui::caption::test_shapes::pass;
    use vike_chart::chart::harness::{ChartHeadless, bind_chart_font_families};
    let context = egui::Context::default();
    bind_chart_font_families(&context);
    let mut chart = ChartHeadless::default();
    let options = vike_chart::ChartSettings::default();
    let style = vike_chart::chart::style::ChartStyle::Candles;
    let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(960.0, 620.0));
    let mut shapes = Vec::new();
    for _ in 0..3 {
        let input = egui::RawInput { screen_rect: Some(screen), ..Default::default() };
        shapes = pass(&context, input, |ui| {
            vike_chart::chart::draw(ui, chart.inputs(state, style, &[], &options));
        });
    }
    vike_ui_theme::painted::texts(&shapes).into_iter().map(|(t, _, _)| t).collect()
}
