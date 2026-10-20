//! What the app looks like, as one value, and the one function that hands it to egui
//! (design system spec §3, §5).
//!
//! `Appearance` is the five appearance settings. The desktop builds it at start from the five
//! `preferences.*` rows and hands it over with [`install`]; the Settings window's Appearance section
//! changes it live with [`apply`], which painters read back through [`current`].
//!
//! The default — Graphite, Classic, no header gradient, Normal, Standard — reproduces the look
//! `vike-desktop`'s `install_visuals` gave, apart from the slots nobody had chosen (warning, error,
//! hyperlink, code background, text cursor, IME underline). Those are now tokens. Which token fills
//! each slot of egui's `Visuals` is a row of the TOML's `visuals` map ([`maps::visuals`]).
//!
//! egui's text styles follow the type roles ([`text_styles`], set with the bundled fonts in PR 3);
//! control heights change with the components (PR 6), where moving every screen is the point.

use egui::{CornerRadius, Margin, Stroke, Visuals};

use crate::components::Tokens;
use crate::maps::{self, MapRow};
use crate::market::MarketId;
use crate::metrics::{Density, RADIUS, space, stroke};
use crate::theme::ThemeId;
use crate::type_scale::TextSize;

/// The five appearance settings, as one value.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Appearance {
    pub theme: ThemeId,
    pub market: MarketId,
    /// The chart's background gradient in window headers. Off by default.
    pub header_gradient: bool,
    pub density: Density,
    pub text_size: TextSize,
}

/// The egui `Visuals` for an appearance — every colour slot a token (see the tests).
///
/// WHICH colour fills each slot is a row of `ui-theme.toml`'s `visuals` map
/// ([`maps::visuals`], one row per slot, named for the egui field in UPPER_SNAKE): a theme token
/// or a status colour, read here under `a`. What stays in code is what is not a colour role: the
/// corner radius, the strokes' width (`stroke::HAIRLINE`) and the two shadows, which are off.
pub fn visuals(a: &Appearance) -> Visuals {
    let t = Tokens::from_appearance(a);
    let cr = CornerRadius::same(RADIUS);
    let fill = |row: &MapRow| row.colour.resolve(&t);
    let outline = |row: &MapRow| Stroke::new(stroke::HAIRLINE, fill(row));

    let mut v = Visuals::dark();
    v.panel_fill = fill(&maps::visuals::PANEL_FILL);
    // Windows, title bars, the rail and dialogs are the background — the same dark as the plot. Only
    // a 1 px border distinguishes them; the surface colour is for panels, tables and menus only.
    v.window_fill = fill(&maps::visuals::WINDOW_FILL);
    v.extreme_bg_color = fill(&maps::visuals::EXTREME_BG_COLOR); // the egui_plot canvas
    v.faint_bg_color = fill(&maps::visuals::FAINT_BG_COLOR);
    v.code_bg_color = fill(&maps::visuals::CODE_BG_COLOR);
    v.window_stroke = outline(&maps::visuals::WINDOW_STROKE);
    v.window_corner_radius = cr;
    v.menu_corner_radius = cr;
    v.selection.bg_fill = fill(&maps::visuals::SELECTION_BG_FILL);
    v.selection.stroke = outline(&maps::visuals::SELECTION_STROKE);
    // No drop shadow or popup halo: that soft dark halo is what made tiled windows look
    // gutter-padded against the flush 2 px tiling.
    v.window_shadow = egui::epaint::Shadow::NONE;
    v.popup_shadow = egui::epaint::Shadow::NONE;
    // The slots nobody had chosen until the design system: egui's own orange warning and pure-red
    // error, its hyperlink blue, grey code background, pale-blue cursor and IME underline.
    v.hyperlink_color = fill(&maps::visuals::HYPERLINK_COLOR);
    v.warn_fg_color = fill(&maps::visuals::WARN_FG_COLOR);
    v.error_fg_color = fill(&maps::visuals::ERROR_FG_COLOR);
    v.text_cursor.stroke.color = fill(&maps::visuals::TEXT_CURSOR_STROKE);
    v.ime_composition.active_underline_stroke.color =
        fill(&maps::visuals::IME_COMPOSITION_ACTIVE_UNDERLINE_STROKE);
    v.ime_composition.inactive_underline_stroke.color =
        fill(&maps::visuals::IME_COMPOSITION_INACTIVE_UNDERLINE_STROKE);

    // Each widget state, with its four rows in the order `bg_fill`, `weak_bg_fill`, `bg_stroke`,
    // `fg_stroke`.
    for (w, [bg_fill, weak_bg_fill, bg_stroke, fg_stroke]) in [
        (
            &mut v.widgets.noninteractive,
            [
                &maps::visuals::NONINTERACTIVE_BG_FILL,
                &maps::visuals::NONINTERACTIVE_WEAK_BG_FILL,
                &maps::visuals::NONINTERACTIVE_BG_STROKE,
                &maps::visuals::NONINTERACTIVE_FG_STROKE,
            ],
        ),
        (
            &mut v.widgets.inactive,
            [
                &maps::visuals::INACTIVE_BG_FILL,
                &maps::visuals::INACTIVE_WEAK_BG_FILL,
                &maps::visuals::INACTIVE_BG_STROKE,
                &maps::visuals::INACTIVE_FG_STROKE,
            ],
        ),
        (
            &mut v.widgets.hovered,
            [
                &maps::visuals::HOVERED_BG_FILL,
                &maps::visuals::HOVERED_WEAK_BG_FILL,
                &maps::visuals::HOVERED_BG_STROKE,
                &maps::visuals::HOVERED_FG_STROKE,
            ],
        ),
        (
            &mut v.widgets.active,
            [
                &maps::visuals::ACTIVE_BG_FILL,
                &maps::visuals::ACTIVE_WEAK_BG_FILL,
                &maps::visuals::ACTIVE_BG_STROKE,
                &maps::visuals::ACTIVE_FG_STROKE,
            ],
        ),
        (
            &mut v.widgets.open,
            [
                &maps::visuals::OPEN_BG_FILL,
                &maps::visuals::OPEN_WEAK_BG_FILL,
                &maps::visuals::OPEN_BG_STROKE,
                &maps::visuals::OPEN_FG_STROKE,
            ],
        ),
    ] {
        w.corner_radius = cr;
        w.bg_fill = fill(bg_fill);
        w.weak_bg_fill = fill(weak_bg_fill);
        w.bg_stroke = outline(bg_stroke);
        w.fg_stroke = outline(fg_stroke);
    }
    v
}

/// Hand an appearance to egui for the first time: the bundled fonts, then everything [`apply`]
/// sets. The desktop calls it once, at start.
///
/// A CHANGE afterwards is [`apply`]'s job, never this function's: `set_fonts` compares the whole
/// font set — the 1.8 MB this crate bundles plus egui's own faces — on every call, and nothing about
/// the fonts depends on any of the five settings.
pub fn install(ctx: &egui::Context, a: &Appearance) {
    ctx.set_fonts(crate::fonts::definitions());
    apply(ctx, a);
}

/// Hand a changed appearance to a running app — the Settings window's live re-apply (spec §5):
/// the theme's visuals, the type roles at the chosen size, the density's spacing, and the
/// appearance itself, kept in the context for [`current`]. Everything [`install`] sets except the
/// fonts.
///
/// It asks egui for a repaint: a change lands at the end of the frame that took the click, and an
/// idle app would otherwise show the old look until the next input.
///
/// ⚠ It pins egui to its DARK style first. egui keeps a dark and a light style and, left at its
/// default, follows the OS setting from the first frame on; `set_visuals` writes only the style
/// in use at the time of the call, so on a light-mode machine the look would be written into the
/// dark style and egui's own light style would be shown. The design system has no light theme
/// (spec §10), so the app is dark whatever the OS says.
pub fn apply(ctx: &egui::Context, a: &Appearance) {
    ctx.set_theme(egui::Theme::Dark);
    ctx.set_visuals(visuals(a));
    let m = a.density.metrics();
    ctx.all_styles_mut(|s| {
        s.text_styles = text_styles(a.text_size);
        s.spacing.button_padding = egui::vec2(m.pad, space::SM);
        s.spacing.item_spacing = egui::vec2(m.gap, 4.0);
        s.spacing.menu_margin = Margin::same(space::SM as i8);
        // Flush title bars, no window inset. egui's edge resize-grab zone is ~5 px; the killed
        // window shadow, not a margin, is what tightens the grid gutters.
        s.spacing.window_margin = Margin::same(0);
    });
    ctx.data_mut(|d| d.insert_temp(appearance_id(), *a));
    ctx.request_repaint();
}

/// The appearance [`apply`] last handed `ctx` — what a painter reads for a colour egui's style
/// does not carry: the market set, the header gradient, a theme token. A context nothing has
/// applied to answers `Appearance::default()`, which is today's look.
pub fn current(ctx: &egui::Context) -> Appearance {
    ctx.data(|d| d.get_temp(appearance_id())).unwrap_or_default()
}

/// Where [`apply`] keeps the appearance in the context's temporary data.
fn appearance_id() -> egui::Id {
    egui::Id::new("vike_ui_theme::appearance::current")
}

/// The app's type: the bundled faces ([`crate::fonts::definitions`]) and egui's text styles at
/// the role sizes ([`text_styles`]) — the same two things [`install`] sets, for every headless
/// harness that needs the app's type without its colours — faces alone would lay out any text
/// that names no size at egui's defaults (Body 13), not the app's (spec §8). `set_fonts` takes
/// effect on the next frame.
///
/// It also records `size` as the context's text size, where [`current`] and so `Tokens::of` read
/// it: egui's styles and the kit's role sizes are two readers of ONE setting, and a harness that
/// installed one and left the other at the default was right only while the default was the size
/// it installed. (Three goldens' worth of tests found out the day the default moved.)
pub fn install_type(ctx: &egui::Context, size: TextSize) {
    ctx.set_fonts(crate::fonts::definitions());
    ctx.all_styles_mut(|s| s.text_styles = text_styles(size));
    let a = Appearance { text_size: size, ..current(ctx) };
    ctx.data_mut(|d| d.insert_temp(appearance_id(), a));
}

/// egui's own text styles, from the type roles (spec §3.3): every widget that names no size draws
/// at its role's size. `Monospace` is the Body role in JetBrains Mono.
pub fn text_styles(size: TextSize) -> std::collections::BTreeMap<egui::TextStyle, egui::FontId> {
    use crate::type_scale::TextRole;
    use egui::{FontFamily, FontId, TextStyle};
    [
        (TextStyle::Small, FontId::new(size.px(TextRole::Caption), FontFamily::Proportional)),
        (TextStyle::Body, FontId::new(size.px(TextRole::Body), FontFamily::Proportional)),
        (TextStyle::Monospace, FontId::new(size.px(TextRole::Body), FontFamily::Monospace)),
        (TextStyle::Button, FontId::new(size.px(TextRole::Strong), FontFamily::Proportional)),
        (TextStyle::Heading, FontId::new(size.px(TextRole::Heading), FontFamily::Proportional)),
    ]
    .into()
}

#[cfg(test)]
mod tests;
