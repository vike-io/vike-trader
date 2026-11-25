//! Each Help row, clicked in the real menu bar, opens its window: the path the desktop runs
//! (`menu_bar` → [`HelpWindows::open`] → [`HelpWindows::show`]) less the shell's two call lines.

use super::*;
use crate::ui::menu_bar::menu_bar;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use tracing_subscriber::layer::SubscriberExt;

/// Stands in for the Studio's table, which the shell passes in.
const STUDIO: [(KeyboardShortcut, &str); 1] =
    [(KeyboardShortcut::new(Modifiers::COMMAND, Key::Enter), "Studio: run the backtest")];

/// The menu bar and the Help windows, as the desktop's frame draws them, with Help → `row` clicked.
fn opened(recent: RecentLines, row: &str) -> Harness<'static> {
    let mut help = HelpWindows::new(recent, &STUDIO, None);
    let mut h = Harness::builder().with_size(egui::vec2(1100.0, 760.0)).build_ui(move |ui| {
        // The menu draws submenu arrows, and their family is bound only by the app's type.
        if !vike_ui_theme::harness::type_ready(ui.ctx()) {
            return;
        }
        let menu = menu_bar(ui, &[], &[]);
        help.open(&menu);
        help.show(ui.ctx());
    });
    h.run();
    h.get_by_label("Help").click();
    h.run();
    h.get_by_label(row).click();
    h.run();
    h
}

#[test]
fn help_about_opens_the_about_window_with_the_version_the_commit_and_the_links() {
    let h = opened(RecentLines::default(), &format!("About {APP_NAME}"));
    // The menu closed on the click, so the one node with these words is the window's title.
    h.get_by_label(&format!("About {APP_NAME}"));
    h.get_by_label(env!("CARGO_PKG_VERSION"));
    h.get_by_label_contains(vike_buildinfo::GIT_SHA);
    h.get_by_label("Source code");
    h.get_by_label("Releases");
}

#[test]
fn help_keyboard_shortcuts_opens_the_list_of_the_handlers_own_bindings() {
    let h = opened(RecentLines::default(), "Keyboard shortcuts");
    h.get_by_label("Keyboard shortcuts");
    for (keys, does) in SHORTCUTS.iter().chain(&STUDIO) {
        h.get_by_label(&h.ctx.format_shortcut(keys));
        h.get_by_label(does);
    }
}

/// The tail `vike_log::init` would keep, holding what `log` logged.
fn logged(log: impl FnOnce()) -> RecentLines {
    let recent = RecentLines::default();
    vike_log::capture::scoped(tracing_subscriber::registry().with(recent.layer()), log);
    recent
}

#[test]
fn help_console_opens_the_log_and_filters_it_by_level() {
    let recent = logged(|| {
        tracing::info!("the first startup line");
        tracing::warn!("a warning line");
        tracing::error!("an error line");
    });
    let mut h = opened(recent, "Console");
    h.get_by_label("Console");
    for line in ["the first startup line", "a warning line", "an error line"] {
        h.get_by_label_contains(line);
    }
    // No file layer ran, so there is no folder to open, and the button says so.
    assert!(h.get_by_label("Open log folder").accesskit_node().is_disabled());
    h.get_by_label("Errors").click();
    h.run();
    assert!(h.query_by_label_contains("the first startup line").is_none());
    assert!(h.query_by_label_contains("a warning line").is_none());
    h.get_by_label_contains("an error line");
}

/// Lines from three crates and one the Console has no name for: this crate's own (`app-core`).
fn three_sources_and_an_unknown_one() -> Harness<'static> {
    let recent = logged(|| {
        tracing::info!(target: "vike_desktop::gpu_startup", "the desktop line");
        tracing::error!(target: "vike_desktop", "the desktop error");
        tracing::info!(target: "egui_wgpu", "the egui line");
        tracing::info!("the app-core line");
        tracing::error!("the app-core error");
        tracing::info!(target: "hyper::client", "the unknown line");
    });
    opened(recent, "Console")
}

/// Every line's words are on screen exactly when `shown` says so.
fn assert_shown(h: &Harness<'_>, shown: &[&str]) {
    for line in [
        "the desktop line",
        "the desktop error",
        "the egui line",
        "the app-core line",
        "the app-core error",
        "the unknown line",
    ] {
        let on_screen = h.query_by_label_contains(line).is_some();
        assert_eq!(on_screen, shown.contains(&line), "{line:?}");
    }
}

#[test]
fn help_console_has_one_button_per_source_with_its_count_and_an_unknown_target_is_other() {
    let h = three_sources_and_an_unknown_one();
    for button in ["All sources", "app-core 2", "desktop 2", "egui 1", "other 1"] {
        h.get_by_label(button);
    }
}

#[test]
fn help_console_source_buttons_toggle_their_lines() {
    let mut h = three_sources_and_an_unknown_one();
    h.get_by_label("desktop 2").click();
    h.run();
    assert_shown(&h, &["the desktop line", "the desktop error"]);
    h.get_by_label("egui 1").click();
    h.run();
    assert_shown(&h, &["the desktop line", "the desktop error", "the egui line"]);
    h.get_by_label("desktop 2").click();
    h.run();
    assert_shown(&h, &["the egui line"]);
    h.get_by_label("All sources").click();
    h.run();
    assert_shown(
        &h,
        &[
            "the desktop line",
            "the desktop error",
            "the egui line",
            "the app-core line",
            "the app-core error",
            "the unknown line",
        ],
    );
}

#[test]
fn help_console_source_filter_combines_with_errors() {
    let mut h = three_sources_and_an_unknown_one();
    h.get_by_label("app-core 2").click();
    h.run();
    h.get_by_label("Errors").click();
    h.run();
    assert_shown(&h, &["the app-core error"]);
}
