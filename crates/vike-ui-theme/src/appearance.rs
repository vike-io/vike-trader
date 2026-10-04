//! What the app looks like, as one value, and the one function that hands it to egui
//! (design system spec §3, §5).
//!
//! `Appearance` is the five appearance settings. The desktop builds it at start from the five
//! `preferences.*` rows and hands it over with [`install`]; the Settings window's Appearance section
//! changes it live with [`apply`], which painters read back through [`current`].
//!
//! The default — Graphite, Classic, no header gradient, Normal, Standard — reproduces the look
//! `vike-desktop`'s `install_visuals` gave, apart from the slots nobody had chosen (warning, error,
//! hyperlink, code background, text cursor, IME underline). Those are now tokens.
//!
//! egui's text styles follow the type roles ([`text_styles`], set with the bundled fonts in PR 3);
//! control heights change with the components (PR 6), where moving every screen is the point.

use egui::{CornerRadius, Margin, Stroke, Visuals};

use crate::market::MarketId;
use crate::metrics::{Density, RADIUS};
use crate::theme::{Theme, ThemeId};
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
pub fn visuals(a: &Appearance) -> Visuals {
    let t = Theme::of(a.theme);
    let cr = CornerRadius::same(RADIUS);

    let mut v = Visuals::dark();
    v.panel_fill = t.bg;
    // Windows, title bars, the rail and dialogs are the background — the same dark as the plot. Only
    // a 1 px border distinguishes them; the surface colour is for panels, tables and menus only.
    v.window_fill = t.bg;
    v.extreme_bg_color = t.bg; // the egui_plot canvas
    v.faint_bg_color = t.surface;
    v.code_bg_color = t.surface;
    v.window_stroke = Stroke::new(1.0, t.border);
    v.window_corner_radius = cr;
    v.menu_corner_radius = cr;
    v.selection.bg_fill = t.hover;
    v.selection.stroke = Stroke::new(1.0, t.text_ui);
    // No drop shadow or popup halo: that soft dark halo is what made tiled windows look
    // gutter-padded against the flush 2 px tiling.
    v.window_shadow = egui::epaint::Shadow::NONE;
    v.popup_shadow = egui::epaint::Shadow::NONE;
    // The slots nobody had chosen until the design system: egui's own orange warning and pure-red
    // error, its hyperlink blue, grey code background, pale-blue cursor and IME underline.
    v.hyperlink_color = crate::status::INFO;
    v.warn_fg_color = crate::status::WARNING;
    v.error_fg_color = crate::status::ERROR;
    v.text_cursor.stroke.color = t.text_ui;
    v.ime_composition.active_underline_stroke.color = t.text_ui;
    v.ime_composition.inactive_underline_stroke.color = t.text3;

    for w in [
        &mut v.widgets.noninteractive,
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.corner_radius = cr;
        w.bg_stroke = Stroke::new(1.0, t.border);
    }
    for w in [&mut v.widgets.noninteractive, &mut v.widgets.inactive] {
        w.bg_fill = t.surface;
        w.weak_bg_fill = t.surface;
        w.fg_stroke = Stroke::new(1.0, t.text2);
    }
    for w in [&mut v.widgets.hovered, &mut v.widgets.active, &mut v.widgets.open] {
        w.bg_fill = t.hover;
        w.weak_bg_fill = t.hover;
        w.fg_stroke = Stroke::new(1.0, t.text_ui);
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
        s.spacing.button_padding = egui::vec2(m.pad, 4.0);
        s.spacing.item_spacing = egui::vec2(m.gap, 4.0);
        s.spacing.menu_margin = Margin::same(4);
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
pub fn install_type(ctx: &egui::Context, size: TextSize) {
    ctx.set_fonts(crate::fonts::definitions());
    ctx.all_styles_mut(|s| s.text_styles = text_styles(size));
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
mod tests {
    use super::*;
    use crate::color_math::contrast_ratio;
    use crate::palette;
    use egui::Color32;

    /// Every colour the installed `Visuals` carries, by name.
    ///
    /// ⚠ EXHAUSTIVE on purpose: every struct is destructured without `..`, so an egui upgrade that
    /// adds a field — possibly a colour nobody set — fails to COMPILE here instead of passing
    /// unchecked. A new non-colour field is named with `_`; a new colour joins the list below.
    /// `deprecated` is allowed because egui 0.36 still carries `clip_rect_margin` (deprecated,
    /// no effect), and naming it is what keeps the pattern exhaustive.
    #[allow(deprecated)]
    fn colour_slots(v: &Visuals) -> Vec<(String, Color32)> {
        use egui::style::{ImeComposition, Selection, TextCursorStyle, WidgetVisuals, Widgets};
        let Visuals {
            dark_mode: _,
            text_options: _,
            override_text_color,
            weak_text_alpha: _,
            weak_text_color,
            widgets,
            selection,
            ime_composition,
            hyperlink_color,
            faint_bg_color,
            extreme_bg_color,
            text_edit_bg_color,
            code_bg_color,
            warn_fg_color,
            error_fg_color,
            window_corner_radius: _,
            window_shadow,
            window_fill,
            window_stroke,
            window_highlight_topmost: _,
            menu_corner_radius: _,
            panel_fill,
            popup_shadow,
            resize_corner_size: _,
            text_cursor,
            clip_rect_margin: _,
            button_frame: _,
            collapsing_header_frame: _,
            indent_has_left_vline: _,
            striped: _,
            slider_trailing_fill: _,
            handle_shape: _,
            interact_cursor: _,
            image_loading_spinners: _,
            numeric_color_space: _,
            disabled_alpha: _,
        } = v;
        let Selection { bg_fill: selection_bg, stroke: selection_stroke } = selection;
        let ImeComposition {
            active_underline_stroke,
            inactive_underline_stroke,
            legacy_visuals: _,
        } = ime_composition;
        let TextCursorStyle {
            stroke: cursor,
            preview: _,
            blink: _,
            on_duration: _,
            off_duration: _,
        } = text_cursor;
        let stroke = |s: &egui::Stroke| {
            let egui::Stroke { width: _, color } = s;
            *color
        };
        let shadow = |s: &egui::Shadow| {
            let egui::Shadow { offset: _, blur: _, spread: _, color } = s;
            *color
        };
        let mut out: Vec<(String, Color32)> = [
            ("panel_fill", *panel_fill),
            ("window_fill", *window_fill),
            ("extreme_bg_color", *extreme_bg_color),
            ("faint_bg_color", *faint_bg_color),
            ("code_bg_color", *code_bg_color),
            ("hyperlink_color", *hyperlink_color),
            ("warn_fg_color", *warn_fg_color),
            ("error_fg_color", *error_fg_color),
            ("window_stroke", stroke(window_stroke)),
            ("selection.bg_fill", *selection_bg),
            ("selection.stroke", stroke(selection_stroke)),
            ("text_cursor", stroke(cursor)),
            ("ime.active", stroke(active_underline_stroke)),
            ("ime.inactive", stroke(inactive_underline_stroke)),
            ("window_shadow", shadow(window_shadow)),
            ("popup_shadow", shadow(popup_shadow)),
        ]
        .into_iter()
        .map(|(n, c)| (n.to_string(), c))
        .collect();
        let Widgets { noninteractive, inactive, hovered, active, open } = widgets;
        for (state, w) in [
            ("noninteractive", noninteractive),
            ("inactive", inactive),
            ("hovered", hovered),
            ("active", active),
            ("open", open),
        ] {
            let WidgetVisuals {
                bg_fill,
                weak_bg_fill,
                bg_stroke,
                corner_radius: _,
                fg_stroke,
                expansion: _,
            } = w;
            out.push((format!("{state}.bg_fill"), *bg_fill));
            out.push((format!("{state}.weak_bg_fill"), *weak_bg_fill));
            out.push((format!("{state}.bg_stroke"), stroke(bg_stroke)));
            out.push((format!("{state}.fg_stroke"), stroke(fg_stroke)));
        }
        for (name, c) in [
            ("override_text_color", override_text_color),
            ("text_edit_bg_color", text_edit_bg_color),
            ("weak_text_color", weak_text_color),
        ] {
            if let Some(c) = c {
                out.push((name.to_string(), *c));
            }
        }
        out
    }

    /// No colour the app installs is one nobody chose: every slot is a token of the theme, a
    /// status colour (`crate::status`) or transparent. This is what retires egui's own orange and
    /// pure red, and it keeps a future slot from arriving with an egui default.
    #[test]
    fn every_colour_slot_is_a_token() {
        for id in ThemeId::ALL {
            let t = Theme::of(id);
            let allowed = [
                t.bg,
                t.grad_top,
                t.surface,
                t.card,
                t.hover,
                t.border,
                t.text,
                t.text_ui,
                t.text2,
                t.text3,
                t.analysis_line,
                t.accent,
                crate::status::OK,
                crate::status::WARNING,
                crate::status::ERROR,
                crate::status::MUTED,
                crate::status::INFO,
                Color32::TRANSPARENT,
            ];
            let a = Appearance { theme: id, ..Appearance::default() };
            for (slot, c) in colour_slots(&visuals(&a)) {
                assert!(allowed.contains(&c), "{id:?}: `{slot}` is {c:?}, which is not a token");
            }
        }
    }

    /// The default appearance IS today's look: every value `install_visuals` set before this PR.
    #[test]
    fn the_default_visuals_are_todays() {
        let v = visuals(&Appearance::default());
        assert!(v.dark_mode);
        assert_eq!(v.panel_fill, palette::BG);
        assert_eq!(v.window_fill, palette::BG);
        assert_eq!(v.extreme_bg_color, palette::BG);
        assert_eq!(v.faint_bg_color, palette::SURFACE);
        assert_eq!(v.window_stroke, Stroke::new(1.0, palette::BORDER));
        assert_eq!(v.window_corner_radius, CornerRadius::same(4));
        assert_eq!(v.menu_corner_radius, CornerRadius::same(4));
        assert_eq!(v.selection.bg_fill, palette::HOVER);
        assert_eq!(v.selection.stroke, Stroke::new(1.0, palette::TEXT_UI));
        assert_eq!(v.window_shadow, egui::epaint::Shadow::NONE);
        assert_eq!(v.popup_shadow, egui::epaint::Shadow::NONE);
        for w in [&v.widgets.noninteractive, &v.widgets.inactive] {
            assert_eq!((w.bg_fill, w.weak_bg_fill), (palette::SURFACE, palette::SURFACE));
            assert_eq!(w.fg_stroke, Stroke::new(1.0, palette::TEXT2));
        }
        for w in [&v.widgets.hovered, &v.widgets.active, &v.widgets.open] {
            assert_eq!((w.bg_fill, w.weak_bg_fill), (palette::HOVER, palette::HOVER));
            assert_eq!(w.fg_stroke, Stroke::new(1.0, palette::TEXT_UI));
        }
        for w in [
            &v.widgets.noninteractive,
            &v.widgets.inactive,
            &v.widgets.hovered,
            &v.widgets.active,
            &v.widgets.open,
        ] {
            assert_eq!(w.corner_radius, CornerRadius::same(4));
            assert_eq!(w.bg_stroke, Stroke::new(1.0, palette::BORDER));
        }
    }

    #[test]
    fn warn_and_error_are_the_status_colours() {
        let v = visuals(&Appearance::default());
        assert_eq!(v.warn_fg_color, crate::status::WARNING);
        assert_eq!(v.error_fg_color, crate::status::ERROR);
    }

    /// Warning and error colour TEXT, so they need the 4.5:1 text floor on every theme — not only on
    /// the one theme anybody looked at.
    #[test]
    fn warn_and_error_text_read_on_every_theme() {
        for id in ThemeId::ALL {
            let v = visuals(&Appearance { theme: id, ..Appearance::default() });
            let bg = Theme::of(id).bg;
            assert!(contrast_ratio(v.warn_fg_color, bg) >= 4.5, "{id:?} warning text");
            assert!(contrast_ratio(v.error_fg_color, bg) >= 4.5, "{id:?} error text");
        }
    }

    /// If Normal moved these by a pixel, every screen in the app would shift.
    #[test]
    fn normal_density_installs_todays_spacing() {
        let ctx = egui::Context::default();
        install(&ctx, &Appearance::default());
        let s = ctx.global_style();
        assert_eq!(s.spacing.button_padding, egui::vec2(8.0, 4.0));
        assert_eq!(s.spacing.item_spacing, egui::vec2(6.0, 4.0));
        assert_eq!(s.spacing.menu_margin, Margin::same(4));
        assert_eq!(s.spacing.window_margin, Margin::same(0));
    }

    /// The look must not depend on the OS theme. egui keeps one style for dark mode and one for
    /// light, and by default follows the system's setting from the first frame on. A look written
    /// into the dark style alone would vanish on a machine in light mode: egui's own light style,
    /// every default this module exists to replace, takes over. The design system has no light
    /// theme (spec §10), so the app stays dark whatever the OS says.
    #[test]
    fn a_light_mode_system_does_not_bring_egui_defaults_back() {
        let ctx = egui::Context::default();
        install(&ctx, &Appearance::default());
        let input = egui::RawInput { system_theme: Some(egui::Theme::Light), ..Default::default() };
        ctx.run_ui(input, |_| {}).drop_without_applying_deltas();
        let v = &ctx.global_style().visuals;
        assert!(v.dark_mode, "a light-mode OS switched the app to egui's light style");
        assert_eq!(v.panel_fill, palette::BG);
        assert_eq!(v.warn_fg_color, crate::status::WARNING);
    }

    /// The five egui text styles' sizes, in `Small, Body, Monospace, Button, Heading` order.
    fn style_px(ctx: &egui::Context) -> Vec<f32> {
        let s = ctx.global_style();
        [
            egui::TextStyle::Small,
            egui::TextStyle::Body,
            egui::TextStyle::Monospace,
            egui::TextStyle::Button,
            egui::TextStyle::Heading,
        ]
        .iter()
        .map(|t| s.text_styles[t].size)
        .collect()
    }

    /// Standard: Caption 10, Body 11 (words and mono), Strong 12, Heading 18 (spec §3.3).
    #[test]
    fn the_standard_text_styles_are_the_role_sizes() {
        let ctx = egui::Context::default();
        install(&ctx, &Appearance::default());
        assert_eq!(style_px(&ctx), [10.0, 11.0, 11.0, 12.0, 18.0]);
        let mono = &ctx.global_style().text_styles[&egui::TextStyle::Monospace];
        assert_eq!(mono.family, egui::FontFamily::Monospace);
    }

    #[test]
    fn large_text_moves_every_style_to_the_large_scale() {
        let ctx = egui::Context::default();
        install(&ctx, &Appearance { text_size: TextSize::Large, ..Appearance::default() });
        assert_eq!(style_px(&ctx), [11.0, 12.0, 12.0, 13.0, 19.0]);
    }

    /// A harness gets the app's whole type from one call: the bundled faces AND the role sizes.
    #[test]
    fn install_type_sets_the_faces_and_the_role_sizes() {
        let ctx = egui::Context::default();
        install_type(&ctx, TextSize::Standard);
        assert_eq!(style_px(&ctx), [10.0, 11.0, 11.0, 12.0, 18.0]);
        ctx.run_ui(egui::RawInput::default(), |_| {}).drop_without_applying_deltas();
        let family = egui::FontFamily::Name(crate::fonts::MONO_SEMIBOLD.into());
        let w = ctx.fonts_mut(|f| f.glyph_width(&egui::FontId::new(12.0, family), 'W'));
        assert!(w > 0.0);
    }

    /// `install` hands egui the bundled fonts. `mono-semibold` exists ONLY in the bundled set, so
    /// after one frame it draws — and before `install` sets the fonts, epaint panics on it
    /// ("not bound to any fonts"), which is this test's red.
    #[test]
    fn install_sets_the_bundled_fonts() {
        let ctx = egui::Context::default();
        install(&ctx, &Appearance::default());
        ctx.run_ui(egui::RawInput::default(), |_| {}).drop_without_applying_deltas();
        let family = egui::FontFamily::Name(crate::fonts::MONO_SEMIBOLD.into());
        let w = ctx.fonts_mut(|f| f.glyph_width(&egui::FontId::new(12.0, family), 'W'));
        assert!(w > 0.0);
    }

    #[test]
    fn compact_density_tightens_padding_and_gap() {
        let ctx = egui::Context::default();
        install(&ctx, &Appearance { density: Density::Compact, ..Appearance::default() });
        let s = ctx.global_style();
        assert_eq!(s.spacing.button_padding, egui::vec2(6.0, 4.0));
        assert_eq!(s.spacing.item_spacing, egui::vec2(4.0, 4.0));
    }

    #[test]
    fn the_default_appearance_is_the_ruled_one() {
        let a = Appearance::default();
        assert_eq!(a.theme, ThemeId::Graphite);
        assert_eq!(a.market, MarketId::Classic);
        assert!(!a.header_gradient);
        assert_eq!(a.density, Density::Normal);
        assert_eq!(a.text_size, TextSize::Standard);
    }

    /// Every value a change switches, on a context that is already running.
    #[test]
    fn apply_changes_the_look_of_a_running_context() {
        let ctx = egui::Context::default();
        install(&ctx, &Appearance::default());
        let a = Appearance {
            theme: ThemeId::Midnight,
            density: Density::Comfortable,
            text_size: TextSize::Large,
            ..Appearance::default()
        };
        apply(&ctx, &a);
        let s = ctx.global_style();
        assert_eq!(s.visuals.panel_fill, Theme::of(ThemeId::Midnight).bg);
        assert_eq!(s.spacing.button_padding, egui::vec2(10.0, 4.0));
        assert_eq!(s.spacing.item_spacing, egui::vec2(8.0, 4.0));
        assert_eq!(style_px(&ctx), [11.0, 12.0, 12.0, 13.0, 19.0]);
    }

    /// `apply` is the per-change path and never hands egui fonts: `set_fonts` compares the whole
    /// set on every call. A context never given fonts runs egui's own set, which has no
    /// `mono-semibold` family, so after `apply` and a frame that family must still be unbound.
    #[test]
    fn apply_leaves_the_fonts_alone() {
        let ctx = egui::Context::default();
        let a = Appearance {
            theme: ThemeId::Dusk,
            text_size: TextSize::Large,
            ..Appearance::default()
        };
        apply(&ctx, &a);
        ctx.run_ui(egui::RawInput::default(), |_| {}).drop_without_applying_deltas();
        let bundled = egui::FontFamily::Name(crate::fonts::MONO_SEMIBOLD.into());
        assert!(!ctx.fonts(|f| f.families()).contains(&bundled), "apply installed the fonts");
    }

    /// Painters read the appearance back: the market set and the header gradient are not in
    /// egui's style.
    #[test]
    fn apply_keeps_the_appearance_for_painters() {
        let ctx = egui::Context::default();
        assert_eq!(current(&ctx), Appearance::default(), "nothing applied: today's look");
        let a = Appearance {
            theme: ThemeId::Carbon,
            market: MarketId::ColourBlind,
            header_gradient: true,
            density: Density::Compact,
            text_size: TextSize::Large,
        };
        apply(&ctx, &a);
        assert_eq!(current(&ctx), a);
    }

    /// A change usually lands at the end of the frame that took the click; an idle, reactive app
    /// would keep the old look until the next input unless `apply` asks for a repaint. egui's
    /// style and data setters ask for none of their own, so this fails without the request.
    ///
    /// ⚠ A NEW context is not idle: egui schedules its own first frames (`ViewportRepaintInfo`'s
    /// default carries one outstanding repaint), so `has_requested_repaint` answers `true` before
    /// anything asked. The context is run empty until it settles, which is what makes the request
    /// seen afterwards `apply`'s own.
    #[test]
    fn apply_asks_for_a_repaint() {
        let ctx = egui::Context::default();
        for _ in 0..5 {
            if !ctx.has_requested_repaint() {
                break;
            }
            ctx.run_ui(egui::RawInput::default(), |_| {}).drop_without_applying_deltas();
        }
        assert!(!ctx.has_requested_repaint(), "precondition: a settled context asks for nothing");
        apply(&ctx, &Appearance::default());
        assert!(ctx.has_requested_repaint());
    }
}
