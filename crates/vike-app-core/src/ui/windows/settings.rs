//! The **Settings** window's body (design system spec §5). Its sections are **Appearance** — the
//! five appearance settings as choices, the two colour settings as painted previews — and
//! **Display**: the one startup-only row (`preferences.vsync`), the GPU candle layer
//! (`preferences.gpu_candles`) and the time zone (`preferences.display_timezone`). A click hands
//! the choice to `crate::ui::appearance_settings::ThemeAppearanceSession`, which applies it to every
//! window once this frame has drawn (the vsync choice is saved and waits for a restart) and saves it
//! as one row of this computer's settings database. A save the database refused — most often
//! because another writer held it — is offered again with a "Save again" button.
//!
//! It is the first screen painted only from the CURRENT theme's tokens, so it follows every change
//! it makes. It keeps two rules (spec §2, §4.2). The accent is a SHAPE — the ring round the
//! selected preview, the line under the selected choice — never a coloured word. And a selected item
//! is a LABEL, not a button, which is how the accessibility tree reads the selection;
//! `crates/vike-app-core/tests/screens/settings_window.rs` depends on it.

use std::collections::HashMap;

use egui::{
    Align2, CornerRadius, FontId, Rect, RichText, Role, Sense, Stroke, StrokeKind, WidgetInfo,
    pos2, vec2,
};
use vike_ui_theme::components::button::ThemeButtonAction;
use vike_ui_theme::market::ThemeMarketColorsId;
use vike_ui_theme::metrics::{RADIUS, ThemeDensity, space, stroke};
use vike_ui_theme::preview::{PREVIEW_SIZE, paint_market_preview, paint_theme_preview};
use vike_ui_theme::theme::{ThemePalette, ThemePaletteId};
use vike_ui_theme::type_scale::{ThemeTextRole, ThemeTextSize};
use vike_ui_theme::value::settings;

use crate::ui::appearance_settings::{
    GPU_CANDLES_HINT, GPU_CANDLES_LABEL, GPU_UNAVAILABLE, MIGRATION_NOTE, SAVED_NOTE,
    TIME_ZONE_LABEL, ThemeAppearanceSession, ThemeAppearanceSessionSaveLine, VSYNC_LABEL,
    pair_note, vsync_hint, where_saved,
};
use crate::ui::claude_connect::{ClaudeConnectView, ClaudeStatus};

/// The AI section's button.
pub const CONNECT_CLAUDE: &str = "Connect Claude";

/// How far the selection ring reaches past a preview: `expand(space::XS)` plus its `stroke::EDGE`
/// outside stroke.
/// A tile reserves it on every side, so a body that clips to its own rect — a maximized or
/// user-resized window's scroll area — never cuts the ring.
const RING_REACH: f32 = space::XS + stroke::EDGE;

/// The Settings window's body. `session` is the app's one appearance session; `claude` is this
/// window's AI section and `vars` the binary's swept environment, where Claude Desktop is looked for.
pub fn settings_tool_content(
    ui: &mut egui::Ui,
    session: &mut ThemeAppearanceSession,
    claude: &mut ClaudeConnectView,
    vars: &HashMap<String, String>,
) {
    let a = session.appearance();
    let t = ThemePalette::of(a.theme);
    let mut next = a;

    ui.label(RichText::new("Appearance").size(a.text_size.px(ThemeTextRole::Title)).strong());
    let whose = where_saved(session.settings_dir(), session.database_present());
    ui.label(RichText::new(whose).color(t.text_secondary));
    ui.label(RichText::new(MIGRATION_NOTE).color(t.text_tertiary));

    heading(ui, "Theme", a.text_size);
    ui.horizontal_wrapped(|ui| {
        for id in ThemePaletteId::ALL {
            let paint =
                |p: &egui::Painter, r: Rect| paint_theme_preview(p, r, id, a.header_gradient);
            if tile(ui, t, a.text_size, id.label(), id == a.theme, paint) {
                next.theme = id;
            }
        }
    });
    heading(ui, "Market colours", a.text_size);
    ui.horizontal_wrapped(|ui| {
        for m in ThemeMarketColorsId::ALL {
            let paint = |p: &egui::Painter, r: Rect| paint_market_preview(p, r, m, a.theme);
            if tile(ui, t, a.text_size, m.label(), m == a.market, paint) {
                next.market = m;
            }
        }
    });
    if let Some(note) = pair_note(&a) {
        ui.label(RichText::new(note).color(t.text_secondary));
    }
    ui.add_space(space::MD);
    ui.checkbox(&mut next.header_gradient, "Gradient in window headers");

    // Density and Text size side by side: two short rows, so the window's 820 height keeps room for
    // the AI section below.
    ui.horizontal_top(|ui| {
        ui.vertical(|ui| {
            heading(ui, "Density", a.text_size);
            ui.horizontal(|ui| {
                for d in ThemeDensity::ALL {
                    if segment(ui, t, d.label(), d == a.density) {
                        next.density = d;
                    }
                }
            });
        });
        ui.add_space(space::XL3);
        ui.vertical(|ui| {
            heading(ui, "Text size", a.text_size);
            ui.horizontal(|ui| {
                for s in ThemeTextSize::ALL {
                    if segment(ui, t, s.label(), s == a.text_size) {
                        next.text_size = s;
                    }
                }
            });
        });
    });

    // Display. Vertical sync is the one startup-only row: saved like the others and never applied
    // to the running window, so the label and a note say a restart is owed. The GPU candle layer
    // and the time zone are in force from the next frame.
    heading(ui, "Display", a.text_size);
    let mut vsync = session.vsync();
    ui.checkbox(&mut vsync, VSYNC_LABEL);
    ui.label(RichText::new(vsync_hint()).color(t.text_tertiary));
    if let Some(note) = session.vsync_note() {
        ui.label(RichText::new(note).color(t.text_secondary));
    }
    if vsync != session.vsync() {
        session.request_vsync(vsync);
    }
    ui.add_space(space::MD);
    let mut gpu = session.gpu_candles();
    ui.add_enabled_ui(session.gpu_available(), |ui| ui.checkbox(&mut gpu, GPU_CANDLES_LABEL))
        .inner
        .on_disabled_hover_text(GPU_UNAVAILABLE);
    // A disabled box says why in the line that would explain it (spec §4.2).
    if session.gpu_available() {
        ui.label(RichText::new(GPU_CANDLES_HINT).color(t.text_tertiary));
    } else {
        ui.label(RichText::new(GPU_UNAVAILABLE).color(t.text_secondary));
    }
    if gpu != session.gpu_candles() {
        session.request_gpu_candles(gpu);
    }
    ui.add_space(space::MD);
    ui.label(TIME_ZONE_LABEL);
    let zone = session.display_time_zone();
    ui.horizontal_wrapped(|ui| {
        for z in vike_ui_theme::time_zone::ThemeDisplayTimeZone::CURATED {
            if segment(ui, t, z.label(), z == zone) {
                session.request_display_time_zone(z);
            }
        }
    });

    ui.add_space(space::LG);
    match session.last() {
        ThemeAppearanceSessionSaveLine::Idle => {}
        ThemeAppearanceSessionSaveLine::Saved => {
            ui.label(RichText::new(SAVED_NOTE).color(t.text_secondary));
        }
        ThemeAppearanceSessionSaveLine::NotSaved(why) => {
            ui.label(RichText::new(why.as_str()).color(ui.visuals().warn_fg_color));
        }
    }
    // A refused save (most often another writer holding the store) is offered again: requesting
    // the look in force saves whatever of it the database does not hold yet.
    let save_again = session.can_save_again() && ui.button("Save again").clicked();
    if next != a {
        session.request(next);
    } else if save_again {
        session.request(a);
    }
    claude_section(ui, t, a.text_size, claude, vars, session.settings_dir());
}

/// What the AI section is for, under its heading.
const CLAUDE_HINT: &str = "Let Claude Desktop use vike's tools. Claude starts this app on this \
     computer: no port is opened and no web address is made.";

/// The AI section: Claude Desktop's status, the Connect Claude button while it is not connected, and
/// what the last click did (`crate::ui::claude_connect`). `settings_dir` is the folder the desktop
/// booted from, which Claude's entry carries.
fn claude_section(
    ui: &mut egui::Ui,
    t: &ThemePalette,
    size: ThemeTextSize,
    view: &mut ClaudeConnectView,
    vars: &HashMap<String, String>,
    settings_dir: Option<&std::path::Path>,
) {
    heading(ui, "AI", size);
    ui.label(RichText::new(CLAUDE_HINT).color(t.text_tertiary));
    let status = view.status(vars, settings_dir);
    let offered = matches!(status, ClaudeStatus::NotConnected | ClaudeStatus::Elsewhere(_));
    let clicked = ui
        .horizontal_wrapped(|ui| {
            let clicked = offered && ui.add(ThemeButtonAction::primary(CONNECT_CLAUDE)).clicked();
            ui.label(RichText::new(status.to_string()).color(t.text_secondary));
            clicked
        })
        .inner;
    if clicked {
        view.connect_now(vars, settings_dir);
    }
    if let Some(done) = &view.done {
        let (line, color) = match done {
            Ok(next) => (next, t.text_secondary),
            Err(why) => (why, ui.visuals().warn_fg_color),
        };
        ui.label(RichText::new(line.as_str()).color(color));
    }
}

/// A group heading, in the Strong role.
fn heading(ui: &mut egui::Ui, text: &str, size: ThemeTextSize) {
    ui.add_space(space::XL);
    ui.label(RichText::new(text).size(size.px(ThemeTextRole::Strong)).strong());
    ui.add_space(space::SM);
}

/// One preview with its name under it. The selected one is a LABEL with the accent ring; every
/// other one is a button, ringed in the border colour on hover. `true` when it was clicked.
fn tile(
    ui: &mut egui::Ui,
    t: &ThemePalette,
    size: ThemeTextSize,
    name: &str,
    selected: bool,
    paint: impl FnOnce(&egui::Painter, Rect),
) -> bool {
    let sense = if selected { Sense::hover() } else { Sense::click() };
    let reach = vec2(RING_REACH, RING_REACH);
    let (rect, response) = ui
        .allocate_exact_size(PREVIEW_SIZE + reach * 2.0 + vec2(0.0, settings::NAME_HEIGHT), sense);
    let preview = Rect::from_min_size(rect.min + reach, PREVIEW_SIZE);
    paint(ui.painter(), preview);
    let ring = if selected {
        Some(Stroke::new(stroke::EDGE, t.accent))
    } else if response.hovered() {
        Some(Stroke::new(stroke::HAIRLINE, t.border))
    } else {
        None
    };
    if let Some(stroke) = ring {
        ui.painter().rect_stroke(
            preview.expand(space::XS),
            CornerRadius::same(RADIUS),
            stroke,
            StrokeKind::Outside,
        );
    }
    ui.painter().text(
        pos2(preview.center().x, preview.bottom() + settings::NAME_HEIGHT / 2.0 + space::HAIR),
        Align2::CENTER_CENTER,
        name,
        FontId::proportional(size.px(ThemeTextRole::Caption)),
        if selected { t.text } else { t.text_secondary },
    );
    let typ = if selected { Role::Label } else { Role::Button };
    response.widget_info(|| WidgetInfo::labeled(typ, true, name));
    response.clicked()
}

/// One choice of a Density or Text size row. The selected one is a LABEL with the accent line
/// under it; the others are buttons. `true` when a button was clicked.
fn segment(ui: &mut egui::Ui, t: &ThemePalette, text: &str, selected: bool) -> bool {
    if selected {
        let r = ui.label(RichText::new(text).strong()).rect;
        ui.painter().hline(
            r.x_range(),
            r.bottom() + space::HAIR,
            Stroke::new(stroke::EDGE, t.accent),
        );
        false
    } else {
        ui.button(text).clicked()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vike_ui_theme::appearance::{ThemeAppearance, install};

    /// Final review (re-graded Important): the selected tile's accent ring stays inside the
    /// body it was drawn in. A maximized or user-resized window lays its body out in a scroll
    /// area that clips to exactly that rect, so a ring reaching past it loses its edge — on
    /// Graphite and Classic, the DEFAULT choices, which are first in their rows.
    #[test]
    fn the_selection_ring_stays_inside_the_body() {
        let context = egui::Context::default();
        install(&context, &ThemeAppearance::default());
        let mut session = ThemeAppearanceSession::new(ThemeAppearance::default(), None);
        let mut claude = ClaudeConnectView::showing(ClaudeStatus::Connected);
        let body = Rect::from_min_size(pos2(40.0, 40.0), vec2(700.0, 700.0));
        let out = context.run_ui(egui::RawInput::default(), |ui| {
            ui.scope_builder(egui::UiBuilder::new().max_rect(body), |ui| {
                settings_tool_content(ui, &mut session, &mut claude, &HashMap::new());
            });
        });
        let accent = ThemePalette::of(ThemePaletteId::Graphite).accent;
        let rings: Vec<Rect> = out
            .shapes
            .iter()
            .filter_map(|c| match &c.shape {
                egui::Shape::Rect(r) if r.stroke.color == accent => Some(r.visual_bounding_rect()),
                _ => None,
            })
            .collect();
        out.drop_without_applying_deltas();
        assert_eq!(rings.len(), 2, "one ring per preview row: the selected theme and market set");
        for r in rings {
            assert!(body.contains_rect(r), "the ring {r:?} leaves the body {body:?}");
        }
    }
}
