//! **The two side panels hide and come back, and the results take the width they leave.** The owner's
//! ask of 2026-11-20: the script editor on the left and the settings panel on the right each hide by a
//! clear button on its inner edge, by its shortcut (Ctrl+[ and Ctrl+]) and, for the settings panel,
//! by its highlighted rail icon; a hidden panel leaves a strip whose arrow brings it back; the editor's
//! splitter width is the state's, kept by the workspace.
//!
//! The results' extent is read off the getting-started line of the empty results panel, which is drawn
//! centred: its centre is the centre of the results area, so the area's two edges move it by half of
//! what each panel gave up.

use egui::{Key, KeyboardShortcut, Modifiers};
use egui_kittest::kittest::NodeT;
use vike_studio::StudioTab;
use vike_ui_theme::icons;
use vike_ui_theme::value::studio;

use super::support::{
    button, harness_over, harness_sized, has_button, label_values, nodes, settle,
};
use crate::common::{seeded_store, state};

/// The empty results panel's centred getting-started line.
const STEPS: &str = "1  Pick a data slice    2  Write or load a strategy    3  Run";

/// The centre of the results area: the getting-started line's centre.
fn results_centre(h: &egui_kittest::Harness<'static, vike_studio::StudioState>) -> f32 {
    let lines: Vec<f32> = h
        .root()
        .children_recursive()
        .filter(|n| {
            let a = n.accesskit_node();
            a.role() == egui::accesskit::Role::Label && a.value().as_deref() == Some(STEPS)
        })
        .map(|n| n.rect().center().x)
        .collect();
    // egui files a label's node twice over in places (its words and what it names), at one rect.
    assert!(!lines.is_empty(), "the empty results panel's line must be on screen");
    assert!(lines.iter().all(|x| (x - lines[0]).abs() < 0.5), "one line, one centre: {lines:?}");
    lines[0]
}

fn shortcut(h: &egui_kittest::Harness<'static, vike_studio::StudioState>, key: Key) -> String {
    let words = h.ctx.format_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, key));
    words.replace(key.name(), key.symbol_or_name())
}

#[test]
fn both_side_panels_hide_and_the_results_take_the_width() {
    let (dir, store) = seeded_store();
    let mut h = harness_over(state(&store, &dir, StudioTab::Sweep, Default::default()));
    settle(&mut h);
    let open = results_centre(&h);
    assert!(label_values(&h).iter().any(|t| t == "Param scan & Validate"), "the control");

    // Each panel's Hide sits on its inner edge.
    button(&h, &icons::COLLAPSE_PANEL.accessible_label("Hide")).click();
    settle(&mut h);
    button(&h, &icons::EXPAND_PANEL.accessible_label("Hide")).click();
    settle(&mut h);
    assert!(h.state().editor_collapsed && h.state().tools_collapsed);
    assert!(
        !label_values(&h).iter().any(|t| t == "Param scan & Validate"),
        "the settings panel's pane is gone"
    );
    // ...and each strip says how to bring its panel back.
    let (editor_key, tools_key) = (shortcut(&h, Key::OpenBracket), shortcut(&h, Key::CloseBracket));
    // The key's symbol, not egui's name for it (`Ctrl+OpenBracket`).
    assert_eq!((editor_key.as_str(), tools_key.as_str()), ("Ctrl+[", "Ctrl+]"));
    assert!(has_button(&h, &format!("Show the script editor ({editor_key})")));
    assert!(has_button(&h, &format!("Show the settings panel ({tools_key})")));

    // The results grew by both: their centre moved by half of what each side gave up.
    let left = studio::EDITOR_DEFAULT_WIDTH - studio::EDITOR_MIN_WIDTH;
    let right = studio::TOOLS_PANEL_WIDTH - studio::EDITOR_MIN_WIDTH;
    let moved = results_centre(&h) - open;
    assert!(
        (moved - (right - left) / 2.0).abs() < 2.0,
        "the results' centre moved {moved}, not {} (left gave {left}, right {right})",
        (right - left) / 2.0
    );

    // The shortcuts bring both back, to where they were.
    h.key_press_modifiers(Modifiers::COMMAND, Key::OpenBracket);
    h.key_press_modifiers(Modifiers::COMMAND, Key::CloseBracket);
    settle(&mut h);
    assert!(!h.state().editor_collapsed && !h.state().tools_collapsed);
    assert!((results_centre(&h) - open).abs() < 1.0, "the results come back to where they were");
}

/// **At the Studio window's default size nothing in the toolbar draws over anything**: the live desktop
/// drew "Server: …" over the run window's presets at 840×660 and cut Refresh off at the edge. Every
/// toolbar control is inside the width, and no two overlap.
#[test]
fn the_toolbar_fits_the_default_studio_size_without_overlap() {
    let (dir, store) = seeded_store();
    let width = 840.0;
    let st = state(&store, &dir, StudioTab::Sweep, Default::default());
    let mut h = harness_sized(st, egui::vec2(width, 660.0));
    settle(&mut h);
    let run = icons::RUN.accessible_label("Run");
    let refresh = icons::REFRESH.accessible_label("Refresh");
    let names = ["1M", "6M", "1Y", "All", "Check", "Advanced", run.as_str(), refresh.as_str()];
    let mut seen: Vec<(String, egui::Rect)> = Vec::new();
    for n in h.root().children_recursive() {
        let a = n.accesskit_node();
        let words = a.label().or(a.value()).map(|t| t.to_string()).unwrap_or_default();
        let wanted = names.contains(&words.as_str()) || words.starts_with("Server: ");
        if wanted && !seen.iter().any(|(w, _)| *w == words) {
            seen.push((words, n.rect()));
        }
    }
    assert_eq!(seen.len(), names.len() + 1, "every toolbar control is on screen: {seen:?}");
    for (i, (a, ra)) in seen.iter().enumerate() {
        assert!(ra.max.x <= width + 0.5, "{a:?} ends at {} past the {width} width", ra.max.x);
        for (b, rb) in &seen[i + 1..] {
            let apart = !ra.shrink(0.5).intersects(rb.shrink(0.5));
            assert!(apart, "{a:?} {ra:?} overlaps {b:?} {rb:?}");
        }
    }
}

/// The highlighted rail icon hides the settings panel; any icon brings it back on that tool.
#[test]
fn the_active_rail_icon_hides_the_settings_panel() {
    let (dir, store) = seeded_store();
    let mut h = harness_over(state(&store, &dir, StudioTab::Sweep, Default::default()));
    settle(&mut h);
    button(&h, StudioTab::Sweep.label()).click();
    settle(&mut h);
    assert!(h.state().tools_collapsed, "the active tool's icon hides its panel");
    button(&h, StudioTab::Saved.label()).click();
    settle(&mut h);
    assert!(!h.state().tools_collapsed, "another icon shows the panel...");
    assert_eq!(h.state().right_tab, StudioTab::Saved, "...on its tool");
}

/// The editor opens at the width the state holds (the workspace's), and the results start where it
/// ends: the splitter's position is the state's, not the theme's default.
#[test]
fn the_editor_opens_at_the_width_the_workspace_kept() {
    let (dir, store) = seeded_store();
    let mut h = harness_over(state(&store, &dir, StudioTab::Sweep, Default::default()));
    settle(&mut h);
    let default = results_centre(&h);

    // Wider than the default, and still beside the settings panel and the results at 1440.
    let wide = studio::EDITOR_DEFAULT_WIDTH + 48.0;
    let mut st = state(&store, &dir, StudioTab::Sweep, Default::default());
    st.editor_width = wide;
    let mut h = harness_over(st);
    settle(&mut h);
    assert_eq!(h.state().editor_width, wide, "the panel kept the width it opened at");
    let moved = results_centre(&h) - default;
    let want = (wide - studio::EDITOR_DEFAULT_WIDTH) / 2.0;
    assert!((moved - want).abs() < 2.0, "the results moved {moved}, not {want}");
}

/// The results' extent: from the editor's right (its Hide, or its strip's arrow) to the settings
/// panel's left (its Hide), each a control just inside its panel's edge.
fn results_span(h: &egui_kittest::Harness<'static, vike_studio::StudioState>) -> (f32, f32) {
    let hide = icons::COLLAPSE_PANEL.accessible_label("Hide");
    let editor = nodes(h, |n| {
        let a = n.accesskit_node();
        let label = a.label().unwrap_or_default();
        a.role() == egui::accesskit::Role::Button
            && (label == hide || label.starts_with("Show the script editor"))
    });
    assert_eq!(editor.len(), 1, "one way to the editor: its Hide or its strip's arrow");
    let tools = button(h, &icons::EXPAND_PANEL.accessible_label("Hide")).rect().min.x;
    (editor[0].rect().max.x, tools)
}

/// **A narrow Studio keeps its results usable** (the owner's pass of 2026-11-21: at about 1420 pixels
/// the editor and the settings panel kept their widths and left the results about 80): the side panels
/// shrink, then the editor folds to its strip, whose arrow makes room by hiding the settings panel; and
/// the Studio widened again shows both at the widths the user had.
#[test]
fn a_narrow_studio_keeps_its_results_usable() {
    let (dir, store) = seeded_store();
    let mut h = harness_over(state(&store, &dir, StudioTab::Sweep, Default::default()));
    settle(&mut h);
    let open = results_centre(&h);

    // Both shrink: the results keep their width, the user's editor width is kept for later.
    h.set_size(egui::vec2(1250.0, 960.0));
    settle(&mut h);
    let (left, right) = results_span(&h);
    assert!(right - left >= studio::RESULTS_MIN_WIDTH, "the results span {left}..{right}");
    // At its own width its Hide ends a margin inside 460; given way, tens of points further left.
    assert!(left + 24.0 < studio::EDITOR_DEFAULT_WIDTH, "the editor gave way: it ends at {left}");
    assert_eq!(h.state().editor_width, studio::EDITOR_DEFAULT_WIDTH, "the user's width is kept");

    // The owner's Studio (about 1420 pixels at 1.5 per point): the editor folds to its strip.
    h.set_size(egui::vec2(943.0, 700.0));
    settle(&mut h);
    let (left, right) = results_span(&h);
    assert!(right - left >= studio::RESULTS_MIN_WIDTH, "the results span {left}..{right}");
    assert!(!h.state().editor_collapsed, "folded for room, not hidden by the user");

    // Widened again: both panels at their widths, the results where they were.
    h.set_size(egui::vec2(1440.0, 960.0));
    settle(&mut h);
    assert!((results_centre(&h) - open).abs() < 1.0, "the panels come back to their widths");

    // Narrow again, the folded editor's arrow hides the settings panel to show the editor.
    h.set_size(egui::vec2(943.0, 700.0));
    settle(&mut h);
    let tip = "Show the script editor: the settings panel hides to make room (Ctrl+[)";
    button(&h, tip).click();
    settle(&mut h);
    assert!(h.state().tools_collapsed && !h.state().editor_collapsed);
    assert!(has_button(&h, &icons::COLLAPSE_PANEL.accessible_label("Hide")), "the editor shows");
}
