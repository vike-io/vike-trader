//! The app's CAPTION: the frameless bar across the top of the main window — the Vike mark, the
//! File / View / Window / Help menus, the GPU toggle, the command palette, the tool launchers and
//! the window controls.
//!
//! It moved down out of `vike-desktop`'s `draw_chrome` in step 7 of the design system
//! (`docs/superpowers/specs/2026-09-28-gui-design-system-design.md` §9) for the reason
//! `crate::ui::workspace::title_bar` did: every decision it makes is CPU work, and here the CI
//! roster runs its tests. The shell keeps what only it can hold — the launcher textures it loaded —
//! and applies the actions this returns.
//!
//! It paints from the INSTALLED appearance (`vike_ui_theme::appearance::current`), never from
//! `vike_ui_theme::palette`'s compile-time Graphite: the theme's background under the bar, its
//! surface under the palette field and its accent as the field's focus ring (spec §2), its hover
//! colour under a hovered launcher. On Midnight, Dusk and Carbon the bar used to stay Graphite's
//! `#0D1117`, a band above windows that had already changed.
//!
//! Its GEOMETRY is fixed on purpose: the caption is the app window's own title bar, not one of the
//! window headers spec §3.4 sizes by density, and its launchers are PNGs drawn at their 18 px design
//! size.

use egui::{Align, Button, CornerRadius, Layout, Sense, UiBuilder};
use vike_ui_theme::appearance;
use vike_ui_theme::icons::{self, Icon};
use vike_ui_theme::metrics::RADIUS;
use vike_ui_theme::theme::Theme;
use vike_ui_theme::type_scale::TextRole;

use crate::ui::workspace::{MenuResult, WinKind, menu_bar};

/// The caption's height — the Python app's `TITLEBAR_H`.
pub const CAPTION_H: f32 = 32.0;
/// One window control's width.
const CONTROL_W: f32 = 34.0;
/// A launcher's square, and its icon's inset: the PNGs' 18 px design size.
const LAUNCHER_BOX: f32 = 26.0;
const LAUNCHER_INSET: f32 = 4.0;
/// The command palette field's height, fixed chrome in this step: only content spacing follows
/// density here (owner decision 4 of the step-7 plan). The field takes the density's control height
/// when it becomes the component kit's text input.
const PALETTE_H: f32 = 24.0;
/// The command palette's hint, shown while the field is empty.
const PALETTE_HINT: &str = "Type symbol or command…  ( / )";
/// The command palette's widest, and the room it leaves the clusters either side of it.
const PALETTE_MAX_W: f32 = 360.0;
const PALETTE_LEFT_CLEAR: f32 = 190.0;
const PALETTE_RIGHT_CLEAR: f32 = 280.0;
/// The GPU toggle's hover texts. A disabled control says why (spec §4.2) — through
/// `on_disabled_hover_text`: egui shows `on_hover_text` for an ENABLED widget only.
const GPU_ON_HOVER: &str = "GPU rendering (candles)";
const GPU_UNAVAILABLE: &str =
    "GPU unavailable: no wgpu backend, or the candle pipeline failed to build at startup";

/// One tool launcher.
pub struct Launcher<'a> {
    /// Its hover text.
    pub name: &'a str,
    /// The icon the shell loaded; `None` until it has.
    pub texture: Option<&'a egui::TextureHandle>,
    /// The window it opens; `None` for a launcher with no tool behind it yet.
    pub kind: Option<WinKind>,
}

/// What the caption draws from.
pub struct CaptionInputs<'a> {
    pub display_tz: vike_chart::DisplayTz,
    /// The saved layouts, for File → Load layout and Delete layout.
    pub layouts: &'a [String],
    /// Whether a GPU candle path exists at all.
    pub gpu_ok: bool,
    /// Left to right, as the bar shows them.
    pub launchers: &'a [Launcher<'a>],
}

/// What the caption decided this frame. The window controls and a drag act on the viewport
/// themselves, as they did before the move.
#[derive(Default)]
pub struct CaptionActions {
    pub menu: MenuResult,
    pub open_kind: Option<WinKind>,
    /// Enter was pressed in the command palette.
    pub palette_submit: bool,
}

/// The caption as the main window's top panel.
pub fn caption_bar(
    ui: &mut egui::Ui,
    inputs: &CaptionInputs<'_>,
    gpu_render: &mut bool,
    palette_text: &mut String,
) -> CaptionActions {
    let mut out = CaptionActions::default();
    egui::Panel::top("caption").frame(egui::Frame::NONE).show_separator_line(false).show(
        ui,
        |ui| {
            let look = appearance::current(ui.ctx());
            let t = Theme::of(look.theme);
            let (cap, cap_resp) = ui.allocate_exact_size(
                egui::vec2(ui.available_width(), CAPTION_H),
                Sense::click_and_drag(),
            );
            ui.painter().rect_filled(cap, 0.0, t.bg);
            if cap_resp.drag_started() {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
            }
            if cap_resp.double_clicked() {
                let m = ui.input(|i| i.viewport().maximized.unwrap_or(false));
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Maximized(!m));
            }
            // LEFT: the mark and the menus.
            ui.scope_builder(
                UiBuilder::new()
                    .max_rect(cap.shrink2(egui::vec2(8.0, 3.0)))
                    .layout(Layout::left_to_right(Align::Center)),
                |ui| {
                    // The Vike mark, orange on every theme (design system spec §6), in the old "V"
                    // tile's footprint, so nothing beside it moves.
                    let (br, _) = ui.allocate_exact_size(egui::vec2(24.0, 22.0), Sense::hover());
                    vike_ui_theme::brand::paint_mark(ui.painter(), br);
                    ui.add_space(10.0);
                    out.menu = menu_bar(ui, &[], inputs.display_tz, inputs.layouts);
                    // The GPU candle layer's global toggle (GPU Phase 2, Task 3). Disabled when there
                    // is no GPU path to flip it to — and then it says why.
                    ui.add_space(10.0);
                    ui.add_enabled_ui(inputs.gpu_ok, |ui| {
                        vike_ui_theme::components::toggle::checkbox(ui, gpu_render, "GPU")
                    })
                    .inner
                    .on_hover_text(GPU_ON_HOVER)
                    .on_disabled_hover_text(GPU_UNAVAILABLE);
                },
            );
            // RIGHT: the window controls, then the launchers.
            ui.scope_builder(
                UiBuilder::new()
                    .max_rect(cap.shrink2(egui::vec2(0.0, 2.0)))
                    .layout(Layout::right_to_left(Align::Center)),
                |ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    ui.visuals_mut().button_frame = false;
                    let icon_px = look.text_size.px(TextRole::Title);
                    let ctl = |ui: &mut egui::Ui, icon: Icon, tip: &str| -> bool {
                        let button = Button::new(icon.rich().size(icon_px));
                        icons::named(ui.add_sized([CONTROL_W, CAPTION_H - 4.0], button), tip)
                            .clicked()
                    };
                    if ctl(ui, icons::CLOSE, "Close") {
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                    let m = ui.input(|i| i.viewport().maximized.unwrap_or(false));
                    if ctl(ui, if m { icons::RESTORE } else { icons::MAXIMIZE }, "Maximize") {
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Maximized(!m));
                    }
                    if ctl(ui, icons::MINIMIZE, "Minimize") {
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Minimized(true));
                    }
                    ui.add_space(10.0);
                    ui.spacing_mut().item_spacing.x = 2.0; // vike topbar QSS spacing:2px
                    for l in inputs.launchers.iter().rev() {
                        if launcher(ui, l).clicked()
                            && let Some(k) = l.kind
                        {
                            out.open_kind = Some(k);
                        }
                    }
                },
            );
            // CENTRE: the command palette, centred in the gap between the two clusters.
            let gap_left = cap.left() + PALETTE_LEFT_CLEAR;
            let gap_right = cap.right() - PALETTE_RIGHT_CLEAR;
            let pal_w = PALETTE_MAX_W.min((gap_right - gap_left - 12.0).max(120.0));
            let pal = egui::Rect::from_center_size(
                egui::pos2((gap_left + gap_right) / 2.0, cap.center().y),
                egui::vec2(pal_w, PALETTE_H),
            );
            ui.scope_builder(
                UiBuilder::new().max_rect(pal).layout(Layout::left_to_right(Align::Center)),
                |ui| {
                    let resp = palette_field(ui, palette_text, pal_w);
                    if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        out.palette_submit = true;
                    }
                },
            );
        },
    );
    out
}

/// One launcher: its icon at the PNGs' design size, the theme's hover fill under it while hovered,
/// its name as hover text.
fn launcher(ui: &mut egui::Ui, l: &Launcher<'_>) -> egui::Response {
    let hover = Theme::of(appearance::current(ui.ctx()).theme).hover;
    let (rect, resp) = ui.allocate_exact_size(egui::Vec2::splat(LAUNCHER_BOX), Sense::click());
    if resp.hovered() {
        ui.painter().rect_filled(rect, CornerRadius::same(RADIUS), hover);
    }
    if let Some(tex) = l.texture {
        // egui's own untinted image — the pixels the old `painter.image(…, WHITE)` drew.
        egui::Image::from_texture(egui::load::SizedTexture::from_handle(tex))
            .paint_at(ui, rect.shrink(LAUNCHER_INSET));
    }
    resp.on_hover_text(l.name)
}

/// The command palette's field: the kit's text input (the accent focus ring, the density's height,
/// the Body role), raised on the theme's surface.
fn palette_field(ui: &mut egui::Ui, text: &mut String, width: f32) -> egui::Response {
    let t = vike_ui_theme::components::Tokens::of(ui.ctx());
    // The kit's input draws on egui's `extreme_bg_color`, the theme's background everywhere else.
    ui.visuals_mut().extreme_bg_color = t.theme.surface;
    ui.spacing_mut().text_edit_width = width;
    vike_ui_theme::components::input::text(
        ui,
        text,
        vike_ui_theme::components::input::Field { hint: PALETTE_HINT, ..Default::default() },
    )
}

/// Shape-level test helpers for the chrome modules (`caption`, `crate::ui::status_bar`, and the
/// dialogs step 7b moves down): a context holding an appearance, the shapes one pass paints, and the
/// colours and sizes in them.
#[cfg(test)]
pub(crate) mod test_shapes {
    use egui::{Color32, Shape};
    use vike_ui_theme::appearance::{Appearance, install};

    /// A 1200 × 400 point screen: room for the caption's two clusters and its palette.
    pub(crate) fn raw() -> egui::RawInput {
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1200.0, 400.0),
            )),
            ..Default::default()
        }
    }

    /// A context holding `a`. `install` hands egui the bundled fonts; they land at the start of the
    /// next pass, which is the first one a test runs.
    pub(crate) fn ctx_with(a: &Appearance) -> egui::Context {
        let ctx = egui::Context::default();
        install(&ctx, a);
        ctx
    }

    /// Every shape one pass of `add` paints on `input`, `Shape::Vec`s flattened.
    pub(crate) fn pass(
        ctx: &egui::Context,
        input: egui::RawInput,
        mut add: impl FnMut(&mut egui::Ui),
    ) -> Vec<Shape> {
        let mut out = ctx.run_ui(input, |ui| add(ui));
        let mut stack: Vec<Shape> =
            std::mem::take(&mut out.shapes).into_iter().map(|c| c.shape).collect();
        out.drop_without_applying_deltas();
        let mut flat = Vec::new();
        while let Some(s) = stack.pop() {
            match s {
                Shape::Vec(v) => stack.extend(v),
                s => flat.push(s),
            }
        }
        flat
    }

    /// The shapes of the SECOND of two passes: a popup, a tooltip or a window lays itself out on a
    /// sizing pass that paints nothing.
    pub(crate) fn second_pass(
        ctx: &egui::Context,
        mut add: impl FnMut(&mut egui::Ui),
    ) -> Vec<Shape> {
        pass(ctx, raw(), &mut add);
        pass(ctx, raw(), &mut add)
    }

    /// `(text, size, colour)` of every text painted, a placeholder colour resolved as the painter
    /// resolves it.
    pub(crate) fn texts(shapes: &[Shape]) -> Vec<(String, f32, Color32)> {
        shapes
            .iter()
            .filter_map(|s| match s {
                Shape::Text(t) => {
                    let f = &t.galley.job.sections.first()?.format;
                    let c =
                        if f.color == Color32::PLACEHOLDER { t.fallback_color } else { f.color };
                    Some((
                        t.galley.text().to_string(),
                        f.font_id.size,
                        t.override_text_color.unwrap_or(c),
                    ))
                }
                _ => None,
            })
            .collect()
    }

    /// The fill colour of every rectangle painted.
    pub(crate) fn fills(shapes: &[Shape]) -> Vec<Color32> {
        shapes
            .iter()
            .filter_map(|s| match s {
                Shape::Rect(r) => Some(r.fill),
                _ => None,
            })
            .collect()
    }

    /// The stroke colour of every line segment painted.
    pub(crate) fn lines(shapes: &[Shape]) -> Vec<Color32> {
        shapes
            .iter()
            .filter_map(|s| match s {
                Shape::LineSegment { stroke, .. } => Some(stroke.color),
                _ => None,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::test_shapes::{ctx_with, pass, raw, second_pass, texts};
    use super::*;
    use egui::Shape;
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;
    use std::sync::{Arc, Mutex};
    use vike_ui_theme::appearance::{Appearance, apply};
    use vike_ui_theme::metrics::{Density, RADIUS};
    use vike_ui_theme::theme::{Theme, ThemeId};
    use vike_ui_theme::type_scale::{TextRole, TextSize};

    const LAUNCHERS: [Launcher<'static>; 1] =
        [Launcher { name: "Chart", texture: None, kind: None }];

    fn inputs(gpu_ok: bool) -> CaptionInputs<'static> {
        CaptionInputs {
            display_tz: vike_chart::DisplayTz::Local,
            layouts: &[],
            gpu_ok,
            launchers: &LAUNCHERS,
        }
    }

    fn draw(ui: &mut egui::Ui, gpu_ok: bool) -> CaptionActions {
        caption_bar(ui, &inputs(gpu_ok), &mut false, &mut String::new())
    }

    /// The fill of the caption's own rectangle.
    fn caption_fill(shapes: &[Shape]) -> Option<egui::Color32> {
        shapes.iter().find_map(|s| match s {
            Shape::Rect(r) if r.rect.top() == 0.0 && r.rect.height() == CAPTION_H => Some(r.fill),
            _ => None,
        })
    }

    /// THE MEASURED DEFECT. The caption was `palette::BG` — Graphite's background — on every theme:
    /// a band of `#0D1117` above windows that had already turned Midnight, Dusk or Carbon.
    #[test]
    fn the_caption_is_painted_in_each_themes_background() {
        for id in ThemeId::ALL {
            let ctx = ctx_with(&Appearance { theme: id, ..Appearance::default() });
            let shapes = pass(&ctx, raw(), |ui| {
                draw(ui, true);
            });
            assert_eq!(caption_fill(&shapes), Some(Theme::of(id).bg), "{id:?}");
        }
    }

    /// A choice in the Settings window reaches the caption on the next frame: the painter reads
    /// the context every frame, and nothing is cached at start.
    #[test]
    fn a_live_theme_change_repaints_the_caption_on_the_next_frame() {
        let ctx = ctx_with(&Appearance::default());
        let fill = |ctx: &egui::Context| {
            caption_fill(&pass(ctx, raw(), |ui| {
                draw(ui, true);
            }))
        };
        assert_eq!(fill(&ctx), Some(Theme::of(ThemeId::Graphite).bg));
        apply(&ctx, &Appearance { theme: ThemeId::Midnight, ..Appearance::default() });
        assert_eq!(fill(&ctx), Some(Theme::of(ThemeId::Midnight).bg));
    }

    /// The palette field sits on the theme's surface and, focused, is ringed in the theme's accent
    /// (spec §2) — through the kit's input now, focused as a person focuses it.
    #[test]
    fn the_palette_field_is_raised_on_the_surface_and_ringed_in_the_accent() {
        for id in ThemeId::ALL {
            let a = Appearance { theme: id, ..Appearance::default() };
            let installed = egui::Id::new("caption-test-installed");
            let mut h =
                Harness::builder().with_size(egui::vec2(1200.0, 200.0)).build_ui(move |ui| {
                    // The first frame installs the theme and draws nothing: the bundled faces land on
                    // the next frame (`vike_ui_theme::harness`'s module doc).
                    if !ui.ctx().data(|d| d.get_temp::<bool>(installed)).unwrap_or(false) {
                        vike_ui_theme::appearance::install(ui.ctx(), &a);
                        ui.ctx().data_mut(|d| d.insert_temp(installed, true));
                        ui.ctx().request_repaint();
                        return;
                    }
                    draw(ui, true);
                });
            h.run();
            h.get_by_role(egui::accesskit::Role::TextInput).focus();
            h.run();
            let t = Theme::of(id);
            let frame = h.output().shapes.iter().find_map(|c| match &c.shape {
                Shape::Rect(r) if r.fill == t.surface && r.rect.width() > 100.0 => {
                    Some(r.stroke.color)
                }
                _ => None,
            });
            assert_eq!(frame, Some(t.accent), "{id:?}: the focused field's frame");
        }
    }

    /// The field is the density's control height (spec §3.4); the kit's input sets it.
    #[test]
    fn the_palette_field_is_the_densitys_control_height() {
        for d in Density::ALL {
            let ctx = ctx_with(&Appearance { density: d, ..Appearance::default() });
            let shapes = pass(&ctx, raw(), |ui| {
                draw(ui, true);
            });
            let surface = Theme::of(ThemeId::Graphite).surface;
            let height = shapes.iter().find_map(|s| match s {
                Shape::Rect(r) if r.fill == surface && r.rect.width() > 100.0 => {
                    Some(r.rect.height())
                }
                _ => None,
            });
            assert_eq!(height, Some(d.metrics().control_h), "{d:?}");
        }
    }

    /// A hovered launcher takes the theme's hover fill at the 4 px radius. It was a fixed `#1E242C`
    /// at 5 px on every theme.
    #[test]
    fn a_hovered_launcher_takes_the_themes_hover_fill() {
        for id in ThemeId::ALL {
            let ctx = ctx_with(&Appearance { theme: id, ..Appearance::default() });
            let mut at = egui::Rect::NOTHING;
            pass(&ctx, raw(), |ui| at = launcher(ui, &LAUNCHERS[0]).rect);
            let hover =
                egui::RawInput { events: vec![egui::Event::PointerMoved(at.center())], ..raw() };
            let shapes = pass(&ctx, hover, |ui| {
                launcher(ui, &LAUNCHERS[0]);
            });
            let hovered = shapes.iter().find_map(|s| match s {
                Shape::Rect(r) if r.fill == Theme::of(id).hover => Some(r.corner_radius),
                _ => None,
            });
            assert_eq!(hovered, Some(egui::CornerRadius::same(RADIUS)), "{id:?}");
        }
    }

    /// A disabled control says why on hover (spec §4.2). The GPU toggle carried its reason on
    /// `on_hover_text`, which egui shows for an ENABLED widget only — so the reason never appeared on
    /// the one machine that needed it. `everything_is_visible` is egui's own "every tooltip at once"
    /// switch, so no pointer timing is involved.
    #[test]
    fn the_disabled_gpu_toggle_says_why() {
        let words = |gpu_ok: bool| -> Vec<String> {
            let ctx = ctx_with(&Appearance::default());
            ctx.memory_mut(|m| m.set_everything_is_visible(true));
            let shapes = second_pass(&ctx, |ui| {
                draw(ui, gpu_ok);
            });
            texts(&shapes).into_iter().map(|(s, _, _)| s).collect()
        };
        assert!(words(false).iter().any(|w| w == GPU_UNAVAILABLE), "the reason, while disabled");
        assert!(!words(true).iter().any(|w| w == GPU_UNAVAILABLE), "…and only while disabled");
    }

    /// Every caption text is ITS OWN role's size, at both text sizes: the menu's words the Strong
    /// role, what is typed into the palette, its hint and the GPU toggle's label the Body role, the
    /// window controls the Title role. The palette is drawn both empty (its hint shows) and holding
    /// a symbol, because only the typed text is laid out in the field's own font — egui draws the
    /// hint in its Body style whatever that font is.
    ///
    /// Checked per text, never against a SET of roles: a literal 12 is the Strong role at Standard
    /// and the Body role at Large, so "is it some role's size" admits it at both text sizes. Every
    /// text must be classified here, so a new one cannot slip past unchecked.
    #[test]
    fn every_caption_text_is_its_own_roles_size() {
        const TYPED: &str = "ETHUSDT";
        let controls: Vec<String> = [icons::CLOSE, icons::MAXIMIZE, icons::MINIMIZE]
            .iter()
            .map(|i| i.rich().text().to_string())
            .collect();
        let role_of = |text: &str| match text {
            "File" | "View" | "Window" | "Help" => Some(TextRole::Strong),
            TYPED | PALETTE_HINT | "GPU" => Some(TextRole::Body),
            t if controls.iter().any(|c| c == t) => Some(TextRole::Title),
            _ => None,
        };
        for size in TextSize::ALL {
            let ctx = ctx_with(&Appearance { text_size: size, ..Appearance::default() });
            for (typed, field_text) in [("", PALETTE_HINT), (TYPED, TYPED)] {
                let shapes = pass(&ctx, raw(), |ui| {
                    caption_bar(ui, &inputs(true), &mut false, &mut typed.to_string());
                });
                let drawn = texts(&shapes);
                for want in ["File", "View", "Window", "Help", field_text] {
                    assert!(drawn.iter().any(|(t, _, _)| t == want), "{size:?}: {want:?} is drawn");
                }
                // An empty field lays out its (empty) text too; a galley with no text paints nothing.
                for (text, px, _) in drawn.into_iter().filter(|(t, _, _)| !t.is_empty()) {
                    let role = role_of(&text)
                        .unwrap_or_else(|| panic!("{size:?}: {text:?} has no role in this test"));
                    assert_eq!(px, size.px(role), "{size:?}: {text:?} is {role:?}");
                }
            }
        }
    }

    /// The move keeps what the caption does: its three window controls are named for a screen
    /// reader, and Enter in the palette submits what was typed.
    #[test]
    fn the_move_keeps_the_window_controls_and_the_palette_submit() {
        let typed = Arc::new(Mutex::new(String::new()));
        let submitted = Arc::new(Mutex::new(false));
        let (text, sink) = (Arc::clone(&typed), Arc::clone(&submitted));
        let mut h = Harness::builder().with_size(egui::vec2(1200.0, 200.0)).build_ui(move |ui| {
            if !vike_ui_theme::harness::type_ready(ui.ctx()) {
                return;
            }
            let out = caption_bar(ui, &inputs(true), &mut false, &mut text.lock().unwrap());
            if out.palette_submit {
                *sink.lock().unwrap() = true;
            }
        });
        h.run();
        for name in ["Close", "Maximize", "Minimize"] {
            h.get_by_label(name);
        }
        h.get_by_role(egui::accesskit::Role::TextInput).focus();
        h.run();
        h.get_by_role(egui::accesskit::Role::TextInput).type_text("ETHUSDT");
        h.run();
        h.key_press(egui::Key::Enter);
        h.run();
        assert_eq!(*typed.lock().unwrap(), "ETHUSDT");
        assert!(*submitted.lock().unwrap(), "Enter submits");
    }
}
