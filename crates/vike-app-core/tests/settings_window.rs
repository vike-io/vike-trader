//! **The Settings window's Appearance section, driven end to end** (design system spec §5).
//!
//! `vike-desktop` is outside the derived CI roster, so the section's decisions live in
//! `vike-app-core` and are gated here through the REAL `settings_tool_content` and the real
//! `AppearanceSession`, read back off the accessibility tree and the egui style. Every write lands
//! in a `tempfile::tempdir()` holding a PLANTED settings database — never a real one.

use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use vike_app_core::ui::appearance_settings::{
    AppearanceSession, BUSY_NOTE, CARBON_COLOURBLIND_NOTE, MIGRATION_NOTE, NO_DATABASE_NOTE,
    SAVED_NOTE, VSYNC_LABEL,
};
use vike_app_core::ui::tool_views::settings_tool_content;
use vike_ui_theme::appearance::{Appearance, current, install};
use vike_ui_theme::metrics::Density;
use vike_ui_theme::theme::{Theme, ThemeId};
use vike_ui_theme::type_scale::TextSize;

struct Screen {
    harness: Harness<'static, ()>,
    session: Arc<Mutex<AppearanceSession>>,
}

impl Screen {
    fn new(start: Appearance, dir: Option<&Path>) -> Self {
        Self::opened_with_vsync(start, dir, true)
    }

    /// A screen whose window was opened with `vsync` as its present mode.
    fn opened_with_vsync(start: Appearance, dir: Option<&Path>, vsync: bool) -> Self {
        let session = Arc::new(Mutex::new(
            AppearanceSession::new(start, dir.map(Path::to_path_buf)).with_vsync(vsync),
        ));
        let inner = Arc::clone(&session);
        // 1000 tall: the Graphics section sits below Text size, and a control clipped out of the
        // screen cannot be clicked.
        let mut harness =
            Harness::builder().with_size(egui::vec2(760.0, 1000.0)).build_ui(move |ui| {
                let mut s = inner.lock().unwrap();
                settings_tool_content(ui, &mut s);
                // The shell's post-loop drain, as `crates/vike-desktop/src/app_ui.rs`'s
                // `draw_windows` performs it (minus the journal, which the session's own tests
                // cover).
                let ctx = ui.ctx().clone();
                s.apply_pending(&ctx, None, 0);
            });
        install(&harness.ctx, &start);
        harness.run();
        Self { harness, session }
    }

    fn session(&self) -> MutexGuard<'_, AppearanceSession> {
        self.session.lock().unwrap()
    }

    /// Every accessibility node's text — labels file it under `value`, buttons under `label`.
    fn texts(&self) -> Vec<String> {
        self.harness
            .root()
            .children_recursive()
            .filter_map(|n| {
                let a = n.accesskit_node();
                a.label().map(|s| s.to_string()).or_else(|| a.value().map(|s| s.to_string()))
            })
            .collect()
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

fn planted() -> tempfile::TempDir {
    let d = tempfile::tempdir().expect("tempdir");
    vike_secrets::plant_settings_rows(
        d.path(),
        &vike_secrets::StoredSettings {
            settings: vec![vike_secrets::SettingRow {
                section: "preferences".to_string(),
                key: "log_level".to_string(),
                value: "\"info\"".to_string(),
            }],
            ..Default::default()
        },
    )
    .expect("a fresh store plants");
    d
}

fn row(dir: &Path, key: &str) -> Option<String> {
    let read = vike_secrets::read_settings_in(dir).ok()?;
    read.rows()?
        .settings
        .iter()
        .find(|r| r.section == "preferences" && r.key == key)
        .map(|r| r.value.clone())
}

#[test]
fn every_option_is_on_screen_and_the_current_one_is_a_label() {
    let screen = Screen::new(Appearance::default(), None);
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
    let dir = planted();
    let mut screen = Screen::new(Appearance::default(), Some(dir.path()));
    screen.click("Midnight");
    assert_eq!(screen.session().appearance().theme, ThemeId::Midnight);
    assert_eq!(current(&screen.harness.ctx).theme, ThemeId::Midnight);
    let fill = screen.harness.ctx.global_style().visuals.panel_fill;
    assert_eq!(fill, Theme::of(ThemeId::Midnight).bg);
    assert_eq!(row(dir.path(), "theme").as_deref(), Some("\"midnight\""));
    assert!(screen.contains(SAVED_NOTE), "{:?}", screen.texts());
    assert_eq!(screen.role("Midnight"), Some(Role::Label), "the new choice reads as selected");
    assert_eq!(screen.role("Graphite"), Some(Role::Button));
}

#[test]
fn density_text_size_and_the_gradient_apply_and_save() {
    let dir = planted();
    let mut screen = Screen::new(Appearance::default(), Some(dir.path()));
    screen.click("Large");
    screen.click("Compact");
    screen.click("Gradient in window headers");
    let a = screen.session().appearance();
    assert_eq!(
        (a.text_size, a.density, a.header_gradient),
        (TextSize::Large, Density::Compact, true)
    );
    let style = screen.harness.ctx.global_style();
    assert_eq!(style.text_styles[&egui::TextStyle::Body].size, 14.0);
    assert_eq!(style.spacing.item_spacing, egui::vec2(4.0, 4.0));
    assert_eq!(row(dir.path(), "text_size").as_deref(), Some("\"large\""));
    assert_eq!(row(dir.path(), "density").as_deref(), Some("\"compact\""));
    assert_eq!(row(dir.path(), "header_gradient").as_deref(), Some("true"));
}

#[test]
fn the_carbon_note_appears_for_carbon_with_colour_blind_only() {
    let mut screen =
        Screen::new(Appearance { theme: ThemeId::Carbon, ..Appearance::default() }, None);
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
    let mut screen = Screen::new(Appearance::default(), Some(dir.path()));
    // Final review, Important 2: the line naming where these settings live must not claim a
    // database that does not exist — before a click, or after one.
    assert!(!screen.contains("they are saved in"), "{:?}", screen.texts());
    assert!(screen.contains("lasts until the app closes"), "{:?}", screen.texts());
    screen.click("Dusk");
    assert_eq!(current(&screen.harness.ctx).theme, ThemeId::Dusk, "applied anyway");
    assert!(screen.contains(NO_DATABASE_NOTE), "{:?}", screen.texts());
    assert!(!screen.contains("they are saved in"), "{:?}", screen.texts());
    assert!(!vike_secrets::db_path_in(dir.path()).exists(), "the app never creates the store");
}

/// Final review, Important 1: a save refused because another writer held the store offers
/// "Save again", and pressing it saves the look already in force.
#[test]
fn a_busy_save_offers_save_again_and_it_saves() {
    let dir = planted();
    let mut screen = Screen::new(Appearance::default(), Some(dir.path()));
    assert_eq!(screen.role("Save again"), None, "nothing is unsaved yet");
    let lock = vike_secrets::hold_write_lock(dir.path());
    screen.click("Midnight");
    assert!(screen.contains(BUSY_NOTE), "{:?}", screen.texts());
    assert_eq!(screen.role("Save again"), Some(Role::Button), "{:?}", screen.texts());
    drop(lock);
    screen.click("Save again");
    assert_eq!(row(dir.path(), "theme").as_deref(), Some("\"midnight\""));
    assert!(screen.contains(SAVED_NOTE), "{:?}", screen.texts());
    assert_eq!(screen.role("Save again"), None, "saved: nothing left to save");
}

/// The Graphics section's one control, driven end to end: a checkbox that says "requires restart"
/// before it is touched, a click that SAVES the row and does not change what is running, a note
/// that a restart is owed, and a second click that puts it back.
#[test]
fn the_vsync_checkbox_saves_a_row_and_says_a_restart_is_owed() {
    let dir = planted();
    let mut screen = Screen::new(Appearance::default(), Some(dir.path()));
    assert_eq!(screen.role(VSYNC_LABEL), Some(Role::CheckBox), "{:?}", screen.texts());
    assert!(screen.contains("144 frames a second"), "{:?}", screen.texts());
    assert!(!screen.contains("this run keeps"), "nothing is owed yet: {:?}", screen.texts());

    screen.click(VSYNC_LABEL);
    assert_eq!(row(dir.path(), "vsync").as_deref(), Some("false"));
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
        Appearance::default(),
        "a vsync choice changes no look"
    );

    screen.click(VSYNC_LABEL);
    assert_eq!(row(dir.path(), "vsync").as_deref(), Some("true"));
    assert!(!screen.contains("this run keeps"), "back to what is running: {:?}", screen.texts());
}

/// A window opened with vsync OFF shows the box unticked and owes nothing; ticking it is what is
/// saved, and the note then names the mode the running window keeps.
#[test]
fn a_window_opened_with_vsync_off_owes_no_restart_until_the_box_is_ticked() {
    let dir = planted();
    let mut screen = Screen::opened_with_vsync(Appearance::default(), Some(dir.path()), false);
    // One lock for both reads: the session sits behind a std `Mutex`, which is not reentrant.
    let (chosen, owed) = {
        let s = screen.session();
        (s.vsync(), s.vsync_needs_restart())
    };
    assert!(!chosen && !owed);
    assert!(!screen.contains("this run keeps"), "{:?}", screen.texts());

    screen.click(VSYNC_LABEL);
    assert_eq!(row(dir.path(), "vsync").as_deref(), Some("true"));
    assert!(
        screen.contains("this run keeps vertical sync off"),
        "the note names the mode the window keeps: {:?}",
        screen.texts()
    );
}

/// Review Focus 3.
#[test]
fn the_screen_says_whose_settings_these_are() {
    let dir = planted();
    let screen = Screen::new(Appearance::default(), Some(dir.path()));
    let db = vike_secrets::db_path_in(dir.path()).display().to_string();
    assert!(screen.contains(&db), "{:?}", screen.texts());
    assert!(screen.contains("not on the backend"), "{:?}", screen.texts());
}
