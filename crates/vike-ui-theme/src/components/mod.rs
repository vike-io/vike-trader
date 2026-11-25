//! The component kit (design system spec §4): every control the app draws, built ONCE here —
//! buttons, tabs, the segmented control, inputs, toggles, chips and marks, section structure, the
//! data table, the navigation rail, the window header, menus, tooltips, toasts, and the three
//! renderings of a pane with no rows. The screens move onto it crate by crate in step 7;
//! `examples/gallery.rs` draws every component in every theme at every density.
//!
//! A component paints with [`ThemeTokens`]: the installed appearance's theme, market colours, density
//! and text size, read from the egui context on every call (`crate::appearance::current`). Nothing
//! is threaded through arguments, so a theme or density change reaches every component on the next
//! frame — the Appearance screen's `crate::appearance::apply` is all it takes.
//!
//! The spec's component rules (§4.2) are TYPES or TESTED STRUCTURE here, never advice:
//! - a selected item is a `Label`, never a `Button`: the tabs, the segmented control, the rail and
//!   the account chip report it that way to the accessibility tree;
//! - a disabled control says why: `button::ThemeButtonAction::disabled_because` is the only way to
//!   disable a kit button;
//! - loading, empty and unreachable are three renderings (`state`);
//! - a count that is not known is not shown as 0: `chip::count` takes an `Option`.

pub mod account_menu;
pub mod button;
pub mod chip;
pub mod input;
pub mod overlay;
pub mod rail;
pub mod section;
pub mod segmented;
pub mod state;
pub mod table;
pub mod tabs;
#[cfg(test)]
pub(crate) mod testing;
pub mod toggle;
pub mod venue_chip;
pub mod window;

use egui::{Color32, CornerRadius, FontId, Stroke, StrokeKind};

use crate::appearance::{self, ThemeAppearance};
use crate::market::ThemeMarketColors;
use crate::metrics::{RADIUS, ThemeDensityMetrics, space, stroke};
use crate::theme::ThemePalette;
use crate::type_scale::{ThemeTextRole, ThemeTextSize};

/// The label colour on every FILLED control: primary, buy, sell and danger buttons, the LIVE chip,
/// a checked toggle. Black clears the 4.5:1 text floor on every fill the design has — the four
/// accents, the eight market colours and the status red — where white clears it on none (the PR 6
/// plan's owner decision 5). `on_fill_reads_on_every_fill` holds it there.
pub const ON_FILL: Color32 = Color32::BLACK;

/// Everything a component paints with, resolved from the installed appearance.
#[derive(Clone, Copy, Debug)]
pub struct ThemeTokens {
    pub theme: &'static ThemePalette,
    pub market: ThemeMarketColors,
    pub metrics: ThemeDensityMetrics,
    pub text: ThemeTextSize,
}

/// The size, in points, of `role` under the text size installed on `context` — what a label whose size
/// is a role asks for (`RichText::size(role_px(ui.ctx(), ThemeTextRole::Body))`), so that the Text size
/// setting reaches it. A literal size (`.size(11.0)`) is the same on every setting, which is how five
/// windows ignored the setting until they were moved onto this.
pub fn role_px(context: &egui::Context, role: crate::type_scale::ThemeTextRole) -> f32 {
    ThemeTokens::of(context).text.px(role)
}

impl ThemeTokens {
    /// The tokens of the appearance installed on `context`.
    pub fn of(context: &egui::Context) -> ThemeTokens {
        ThemeTokens::from_appearance(&appearance::current(context))
    }

    /// The header gradient is not a token: the window header hands the whole appearance to
    /// `window::background::paint_header_background`, which is the one place that reads it.
    pub fn from_appearance(a: &ThemeAppearance) -> ThemeTokens {
        ThemeTokens {
            theme: ThemePalette::of(a.theme),
            market: ThemeMarketColors::of(a.market),
            metrics: a.density.metrics(),
            text: a.text_size,
        }
    }

    /// `role` in Inter.
    pub fn font(&self, role: ThemeTextRole) -> FontId {
        FontId::proportional(self.text.px(role))
    }

    /// `role` in JetBrains Mono.
    pub fn mono(&self, role: ThemeTextRole) -> FontId {
        FontId::monospace(self.text.px(role))
    }

    /// `role` in Inter at the semibold weight: the owner's v3 design draws the symbol this way.
    pub fn font_semibold(&self, role: ThemeTextRole) -> FontId {
        FontId::new(self.text.px(role), egui::FontFamily::Name(crate::fonts::SEMIBOLD.into()))
    }

    /// `role` in JetBrains Mono at the semibold weight: the design's window price.
    pub fn mono_semibold(&self, role: ThemeTextRole) -> FontId {
        FontId::new(self.text.px(role), egui::FontFamily::Name(crate::fonts::MONO_SEMIBOLD.into()))
    }
}

/// The keyboard-focus ring (spec §2 names the focus ring among the accent's shapes): a 1 px accent
/// outline 2 px outside the widget, so it reads on a filled control — a primary button IS the
/// accent — as well as on a bare one. Every focusable kit widget draws it while it has focus.
pub(crate) fn focus_ring(ui: &egui::Ui, t: &ThemeTokens, response: &egui::Response) {
    if response.has_focus() {
        let ring = Stroke::new(stroke::HAIRLINE, t.theme.accent);
        let round = CornerRadius::same(RADIUS + 2);
        ui.painter().rect_stroke(response.rect.expand(space::XS), round, ring, StrokeKind::Inside);
    }
}

/// A status — the same colour in every theme (spec §3.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ThemeStatus {
    Ok,
    Warning,
    Error,
    Info,
    Muted,
}

impl ThemeStatus {
    pub const ALL: [ThemeStatus; 5] = [
        ThemeStatus::Ok,
        ThemeStatus::Warning,
        ThemeStatus::Error,
        ThemeStatus::Info,
        ThemeStatus::Muted,
    ];

    /// Fixed in every theme — which is why this reads `crate::status`, never the theme.
    pub fn color(self) -> Color32 {
        match self {
            ThemeStatus::Ok => crate::status::OK,
            ThemeStatus::Warning => crate::status::WARNING,
            ThemeStatus::Error => crate::status::ERROR,
            ThemeStatus::Info => crate::status::INFO,
            ThemeStatus::Muted => crate::status::MUTED,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color_math::contrast_ratio;
    use crate::market::ThemeMarketColorsId;
    use crate::metrics::ThemeDensity;
    use crate::theme::ThemePaletteId;

    /// A component reads the appearance `install` handed the context, not a compile-time default.
    #[test]
    fn tokens_follow_the_installed_appearance() {
        let context = egui::Context::default();
        let a = ThemeAppearance {
            theme: ThemePaletteId::Carbon,
            market: ThemeMarketColorsId::ColourBlind,
            header_gradient: true,
            density: ThemeDensity::Comfortable,
            text_size: ThemeTextSize::Large,
        };
        appearance::install(&context, &a);
        let t = ThemeTokens::of(&context);
        assert_eq!(t.theme.accent, ThemePalette::of(ThemePaletteId::Carbon).accent);
        assert_eq!(t.market, ThemeMarketColors::of(ThemeMarketColorsId::ColourBlind));
        assert_eq!(t.metrics, ThemeDensity::Comfortable.metrics());
        assert_eq!(t.font(ThemeTextRole::Body).size, 14.0);
        assert_eq!(t.mono(ThemeTextRole::Body).family, egui::FontFamily::Monospace);
    }

    /// The label on a fill clears the 4.5:1 text floor on every fill the design has.
    #[test]
    fn on_fill_reads_on_every_fill() {
        let mut fills = vec![("status error", ThemeStatus::Error.color())];
        for id in ThemePaletteId::ALL {
            fills.push(("accent", ThemePalette::of(id).accent));
        }
        for m in ThemeMarketColorsId::ALL {
            let c = ThemeMarketColors::of(m);
            fills.push(("up", c.up));
            fills.push(("down", c.down));
        }
        for (what, fill) in fills {
            let r = contrast_ratio(ON_FILL, fill);
            assert!(r >= 4.5, "{what} {fill:?}: {r:.2}");
        }
    }

    /// Status colours are the same in every theme and never the theme's accent (spec §2).
    #[test]
    fn status_colours_are_fixed_and_never_the_accent() {
        for s in ThemeStatus::ALL {
            for id in ThemePaletteId::ALL {
                assert_ne!(s.color(), ThemePalette::of(id).accent, "{s:?} on {id:?}");
            }
        }
        assert_eq!(ThemeStatus::Ok.color(), crate::status::OK);
        assert_eq!(ThemeStatus::Info.color(), crate::status::INFO);
    }

    /// Is there a focus ring in `accent` — an accent outline with nothing inside it?
    fn has_focus_ring(shapes: &[egui::Shape], accent: Color32) -> bool {
        shapes.iter().any(|s| {
            matches!(s, egui::Shape::Rect(r)
                if r.stroke.color == accent && r.fill == Color32::TRANSPARENT && r.stroke.width > 0.0)
        })
    }

    /// Every focusable kit widget draws the accent focus ring while it has keyboard focus — spec §2
    /// lists the focus ring among the accent's shapes — on every theme, and none draws one without
    /// focus. Tab moves focus onto the first focusable widget each case draws.
    #[test]
    fn every_focusable_kit_widget_draws_the_accent_focus_ring() {
        use crate::components::testing::{paint, paint_focused};
        use crate::icons;
        use crate::painted::context_with;
        type Draw = fn(&mut egui::Ui);
        let widgets: [(&str, Draw); 12] = [
            ("action button", |ui| {
                ui.add(button::ThemeButtonAction::primary("Save"));
            }),
            ("icon button", |ui| {
                ui.add(button::ThemeButtonIcon::new(icons::SETTINGS, "Settings"));
            }),
            ("tab", |ui| {
                let tabs = [
                    tabs::ThemeTab { value: 0u8, label: "Credentials", count: None },
                    tabs::ThemeTab { value: 1u8, label: "Backend", count: None },
                ];
                tabs::underline(ui, &mut 0u8, &tabs);
            }),
            ("segment", |ui| {
                let segments = [
                    segmented::ThemeSegment { value: 0u8, label: "1D", why: "One day" },
                    segmented::ThemeSegment { value: 1u8, label: "1W", why: "One week" },
                ];
                segmented::segmented(ui, &mut 1u8, &segments);
            }),
            ("switch", |ui| {
                toggle::switch(ui, &mut false, "Live orders");
            }),
            ("checkbox", |ui| {
                toggle::checkbox(ui, &mut false, "Show volume");
            }),
            ("radio", |ui| {
                toggle::radio(ui, &mut 0u8, 1, "Local");
            }),
            ("account", |ui| {
                chip::account(ui, "hedge", vike_model::AccountTier::Demo, false);
            }),
            ("rail row", |ui| {
                let items = [
                    rail::ThemeRailItem {
                        value: 0u8,
                        group: "Browse",
                        icon: icons::OVERVIEW,
                        label: "Overview",
                        count: None,
                    },
                    rail::ThemeRailItem {
                        value: 1u8,
                        group: "Browse",
                        icon: icons::ALL_SERIES,
                        label: "All series",
                        count: None,
                    },
                ];
                rail::navigation_rail(ui, &mut 0u8, &items);
            }),
            ("menu row", |ui| {
                overlay::menu_item(ui, None, "Load layout", None);
            }),
            ("section header", |ui| {
                section::section(ui, "s", "Positions", |_| {}, |_| {});
            }),
            ("table row", |ui| {
                let cols = [table::ThemeTableColumn {
                    title: "Symbol",
                    weight: 1.0,
                    min_w: 0.0,
                    numeric: false,
                }];
                table::data_table(ui, "t", &cols, 2, Some(0), |r, _| format!("row {r}"));
            }),
        ];
        for id in ThemePaletteId::ALL {
            let accent = ThemePalette::of(id).accent;
            for (name, draw) in widgets {
                let context =
                    context_with(&ThemeAppearance { theme: id, ..ThemeAppearance::default() });
                assert!(
                    !has_focus_ring(&paint(&context, draw), accent),
                    "{id:?} {name}: ring unfocused"
                );
                assert!(
                    has_focus_ring(&paint_focused(&context, draw), accent),
                    "{id:?} {name}: no ring"
                );
            }
        }
    }
}
