//! The component kit (design system spec §4): every control the app draws, built ONCE here —
//! buttons, tabs, the segmented control, inputs, toggles, chips and marks, section structure, the
//! data table, the navigation rail, the window header, menus, tooltips, toasts, and the three
//! renderings of a pane with no rows. The screens move onto it crate by crate in step 7;
//! `examples/gallery.rs` draws every component in every theme at every density.
//!
//! A component paints with [`Tokens`]: the installed appearance's theme, market colours, density
//! and text size, read from the egui context on every call (`crate::appearance::current`). Nothing
//! is threaded through arguments, so a theme or density change reaches every component on the next
//! frame — the Appearance screen's `crate::appearance::apply` is all it takes.
//!
//! The spec's component rules (§4.2) are TYPES or TESTED STRUCTURE here, never advice:
//! - a selected item is a `Label`, never a `Button`: the tabs, the segmented control, the rail and
//!   the account chip report it that way to the accessibility tree;
//! - a disabled control says why: `button::ActionButton::disabled_because` is the only way to
//!   disable a kit button;
//! - loading, empty and unreachable are three renderings (`state`);
//! - a count that is not known is not shown as 0: `chip::count` takes an `Option`.

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
pub mod window;

use egui::{Color32, CornerRadius, FontId, Stroke, StrokeKind};

use crate::appearance::{self, Appearance};
use crate::market::MarketColors;
use crate::metrics::{Metrics, RADIUS};
use crate::theme::Theme;
use crate::type_scale::{TextRole, TextSize};

/// The label colour on every FILLED control: primary, buy, sell and danger buttons, the LIVE chip,
/// a checked toggle. Black clears the 4.5:1 text floor on every fill the design has — the four
/// accents, the eight market colours and the status red — where white clears it on none (the PR 6
/// plan's owner decision 5). `on_fill_reads_on_every_fill` holds it there.
pub const ON_FILL: Color32 = Color32::BLACK;

/// Everything a component paints with, resolved from the installed appearance.
#[derive(Clone, Copy, Debug)]
pub struct Tokens {
    pub theme: &'static Theme,
    pub market: MarketColors,
    pub metrics: Metrics,
    pub text: TextSize,
}

impl Tokens {
    /// The tokens of the appearance installed on `ctx`.
    pub fn of(ctx: &egui::Context) -> Tokens {
        Tokens::from_appearance(&appearance::current(ctx))
    }

    /// The header gradient is not a token: the window header hands the whole appearance to
    /// `crate::header::paint_header_background`, which is the one place that reads it.
    pub fn from_appearance(a: &Appearance) -> Tokens {
        Tokens {
            theme: Theme::of(a.theme),
            market: MarketColors::of(a.market),
            metrics: a.density.metrics(),
            text: a.text_size,
        }
    }

    /// `role` in Inter.
    pub fn font(&self, role: TextRole) -> FontId {
        FontId::proportional(self.text.px(role))
    }

    /// `role` in JetBrains Mono.
    pub fn mono(&self, role: TextRole) -> FontId {
        FontId::monospace(self.text.px(role))
    }
}

/// The keyboard-focus ring (spec §2 names the focus ring among the accent's shapes): a 1 px accent
/// outline 2 px outside the widget, so it reads on a filled control — a primary button IS the
/// accent — as well as on a bare one. Every focusable kit widget draws it while it has focus.
pub(crate) fn focus_ring(ui: &egui::Ui, t: &Tokens, resp: &egui::Response) {
    if resp.has_focus() {
        let ring = Stroke::new(1.0, t.theme.accent);
        let round = CornerRadius::same(RADIUS + 2);
        ui.painter().rect_stroke(resp.rect.expand(2.0), round, ring, StrokeKind::Inside);
    }
}

/// A status — the same colour in every theme (spec §3.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Status {
    Ok,
    Warning,
    Error,
    Info,
    Muted,
}

impl Status {
    pub const ALL: [Status; 5] =
        [Status::Ok, Status::Warning, Status::Error, Status::Info, Status::Muted];

    /// Fixed in every theme — which is why this reads `crate::status`, never the theme.
    pub fn color(self) -> Color32 {
        match self {
            Status::Ok => crate::status::OK,
            Status::Warning => crate::status::WARNING,
            Status::Error => crate::status::ERROR,
            Status::Info => crate::status::INFO,
            Status::Muted => crate::status::MUTED,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color_math::contrast_ratio;
    use crate::market::MarketId;
    use crate::metrics::Density;
    use crate::theme::ThemeId;

    /// A component reads the appearance `install` handed the context, not a compile-time default.
    #[test]
    fn tokens_follow_the_installed_appearance() {
        let ctx = egui::Context::default();
        let a = Appearance {
            theme: ThemeId::Carbon,
            market: MarketId::ColourBlind,
            header_gradient: true,
            density: Density::Comfortable,
            text_size: TextSize::Large,
        };
        appearance::install(&ctx, &a);
        let t = Tokens::of(&ctx);
        assert_eq!(t.theme.accent, Theme::of(ThemeId::Carbon).accent);
        assert_eq!(t.market, MarketColors::of(MarketId::ColourBlind));
        assert_eq!(t.metrics, Density::Comfortable.metrics());
        assert_eq!(t.font(TextRole::Body).size, 12.0);
        assert_eq!(t.mono(TextRole::Body).family, egui::FontFamily::Monospace);
    }

    /// The label on a fill clears the 4.5:1 text floor on every fill the design has.
    #[test]
    fn on_fill_reads_on_every_fill() {
        let mut fills = vec![("status error", Status::Error.color())];
        for id in ThemeId::ALL {
            fills.push(("accent", Theme::of(id).accent));
        }
        for m in MarketId::ALL {
            let c = MarketColors::of(m);
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
        for s in Status::ALL {
            for id in ThemeId::ALL {
                assert_ne!(s.color(), Theme::of(id).accent, "{s:?} on {id:?}");
            }
        }
        assert_eq!(Status::Ok.color(), crate::status::OK);
        assert_eq!(Status::Info.color(), crate::status::INFO);
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
        use crate::components::testing::{ctx_with, paint, paint_focused};
        use crate::icons;
        type Draw = fn(&mut egui::Ui);
        let widgets: [(&str, Draw); 12] = [
            ("action button", |ui| {
                ui.add(button::ActionButton::primary("Save"));
            }),
            ("icon button", |ui| {
                ui.add(button::IconButton::new(icons::SETTINGS, "Settings"));
            }),
            ("tab", |ui| {
                let tabs = [
                    tabs::Tab { value: 0u8, label: "Credentials", count: None },
                    tabs::Tab { value: 1u8, label: "Backend", count: None },
                ];
                tabs::underline(ui, &mut 0u8, &tabs);
            }),
            ("segment", |ui| {
                let segs = [
                    segmented::Segment { value: 0u8, label: "1D", why: "One day" },
                    segmented::Segment { value: 1u8, label: "1W", why: "One week" },
                ];
                segmented::segmented(ui, &mut 1u8, &segs);
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
                chip::account(ui, "hedge", chip::Mode::Demo, false);
            }),
            ("rail row", |ui| {
                let items = [
                    rail::RailItem {
                        value: 0u8,
                        group: "Browse",
                        icon: icons::OVERVIEW,
                        label: "Overview",
                        count: None,
                    },
                    rail::RailItem {
                        value: 1u8,
                        group: "Browse",
                        icon: icons::ALL_SERIES,
                        label: "All series",
                        count: None,
                    },
                ];
                rail::nav_rail(ui, &mut 0u8, &items);
            }),
            ("menu row", |ui| {
                overlay::menu_item(ui, None, "Load layout", None);
            }),
            ("section header", |ui| {
                section::section(ui, "s", "Positions", |_| {}, |_| {});
            }),
            ("table row", |ui| {
                let cols =
                    [table::Column { title: "Symbol", weight: 1.0, min_w: 0.0, numeric: false }];
                table::data_table(ui, "t", &cols, 2, Some(0), |r, _| format!("row {r}"));
            }),
        ];
        for id in ThemeId::ALL {
            let accent = Theme::of(id).accent;
            for (name, draw) in widgets {
                let ctx = ctx_with(&Appearance { theme: id, ..Appearance::default() });
                assert!(
                    !has_focus_ring(&paint(&ctx, draw), accent),
                    "{id:?} {name}: ring unfocused"
                );
                assert!(
                    has_focus_ring(&paint_focused(&ctx, draw), accent),
                    "{id:?} {name}: no ring"
                );
            }
        }
    }
}
