//! Buttons (design system spec §4.1): primary, secondary, buy, sell and danger as
//! [`ActionButton`], and the icon-only [`IconButton`]. Every one is the density's control height
//! and rounded by `RADIUS`.

use egui::{Atoms, Color32, CornerRadius, IntoAtoms, Response, Stroke, Ui, Widget};

use super::{ON_FILL, Status, Tokens};
use crate::icons::{self, Icon};
use crate::metrics::RADIUS;
use crate::type_scale::TextRole;

/// What a button does, which decides how it is painted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    /// The one action a view exists for — Save, Apply, Run: the theme's accent, filled.
    Primary,
    /// Everything else: an outline (the surface fill, the border stroke).
    Secondary,
    /// Buy: the market set's "up", filled.
    Buy,
    /// Sell: the market set's "down", filled.
    Sell,
    /// An action that destroys something: the status red, filled.
    Danger,
}

impl Kind {
    pub const ALL: [Kind; 5] =
        [Kind::Primary, Kind::Secondary, Kind::Buy, Kind::Sell, Kind::Danger];

    /// `(fill, stroke, label)` under `t`. The accent is a SHAPE here — the primary fill — never a
    /// label colour; every filled kind carries [`ON_FILL`].
    pub fn colours(self, t: &Tokens) -> (Color32, Stroke, Color32) {
        match self {
            Kind::Primary => (t.theme.accent, Stroke::NONE, ON_FILL),
            Kind::Secondary => (t.theme.surface, Stroke::new(1.0, t.theme.border), t.theme.text_ui),
            Kind::Buy => (t.market.up, Stroke::NONE, ON_FILL),
            Kind::Sell => (t.market.down, Stroke::NONE, ON_FILL),
            Kind::Danger => (Status::Error.color(), Stroke::NONE, ON_FILL),
        }
    }

    /// The fill under the pointer and while pressed: the outlined kind takes the theme's hover fill,
    /// as egui's own buttons do; a filled kind moves its fill a step toward the text colour —
    /// lighter on every dark theme, so the on-fill black label only gains contrast.
    fn lit(self, t: &Tokens) -> Color32 {
        match self {
            Kind::Secondary => t.theme.hover,
            k => k.colours(t).0.lerp_to_gamma(t.theme.text, 0.12),
        }
    }
}

/// Button padding: the density's cell padding across, 2 px down, so the control height — not the
/// text — sets how tall a button is.
fn padding(t: &Tokens) -> egui::Vec2 {
    egui::vec2(t.metrics.pad, 2.0)
}

/// Refuses, in a debug build, a kit button disabled without a reason: put in a disabled `Ui` (or by
/// `add_enabled(false, …)`) it would be a dead control that cannot say why (spec §4.2). A `Ui` that
/// is only invisible — egui's first, sizing pass of a menu or a popup — is not a refusal.
fn refuse_a_silent_disable(ui: &Ui, reason: Option<&str>) {
    debug_assert!(
        reason.is_some() || ui.is_enabled() || !ui.is_visible(),
        "a kit button is disabled only through `disabled_because`, which says why (spec §4.2)"
    );
}

/// A kit button: `ui.add(ActionButton::primary("Save"))`, or with an icon,
/// `ui.add(ActionButton::secondary((icons::REFRESH, "Refresh")))`. Its words are the Strong role
/// (spec §3.3: "window titles, buttons, tabs"), and an icon among them takes the words' size.
#[must_use = "add it with `ui.add(…)`"]
pub struct ActionButton<'a> {
    kind: Kind,
    atoms: Atoms<'a>,
    disabled: Option<&'a str>,
}

impl<'a> ActionButton<'a> {
    pub fn new(kind: Kind, atoms: impl IntoAtoms<'a>) -> Self {
        Self { kind, atoms: atoms.into_atoms(), disabled: None }
    }
    pub fn primary(atoms: impl IntoAtoms<'a>) -> Self {
        Self::new(Kind::Primary, atoms)
    }
    pub fn secondary(atoms: impl IntoAtoms<'a>) -> Self {
        Self::new(Kind::Secondary, atoms)
    }
    pub fn buy(atoms: impl IntoAtoms<'a>) -> Self {
        Self::new(Kind::Buy, atoms)
    }
    pub fn sell(atoms: impl IntoAtoms<'a>) -> Self {
        Self::new(Kind::Sell, atoms)
    }
    pub fn danger(atoms: impl IntoAtoms<'a>) -> Self {
        Self::new(Kind::Danger, atoms)
    }

    /// Disable it and say why on hover. The ONLY way to disable a kit button: a control with
    /// nothing behind it is disabled and says why — never a live, dead button (spec §4.2).
    pub fn disabled_because(mut self, why: &'a str) -> Self {
        self.disabled = Some(why);
        self
    }
}

impl Widget for ActionButton<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        refuse_a_silent_disable(ui, self.disabled);
        let t = Tokens::of(ui.ctx());
        let (fill, stroke, ink) = self.kind.colours(&t);
        let lit = self.kind.lit(&t);
        let enabled = self.disabled.is_none();
        let resp = ui
            .scope(|ui| {
                let v = ui.visuals_mut();
                // `override_text_color` is what an uncoloured atom's text takes: the label AND an icon.
                v.override_text_color = Some(ink);
                // Fill and stroke PER STATE, never `Button::fill`: a fill set on the button paints
                // every state alike, and the pointer would get no answer.
                for (w, f) in [
                    (&mut v.widgets.noninteractive, fill),
                    (&mut v.widgets.inactive, fill),
                    (&mut v.widgets.hovered, lit),
                    (&mut v.widgets.active, lit),
                ] {
                    w.weak_bg_fill = f;
                    w.bg_stroke = stroke;
                }
                // egui 0.36 sizes a button's words by `override_font_id`, falling back to Body: the
                // kit names the role instead.
                ui.style_mut().override_font_id = Some(t.font(TextRole::Strong));
                ui.spacing_mut().button_padding = padding(&t);
                let button = egui::Button::new(self.atoms)
                    .corner_radius(CornerRadius::same(RADIUS))
                    .min_size(egui::vec2(0.0, t.metrics.control_h));
                ui.add_enabled(enabled, button)
            })
            .inner;
        super::focus_ring(ui, &t, &resp);
        match self.disabled {
            Some(why) => resp.on_disabled_hover_text(why),
            None => resp,
        }
    }
}

/// An icon-only button: frameless until hovered, a `control_h` square, and its `tip` both as the
/// hover text and as the accessible name — an icon without a word needs both, and
/// [`icons::named`] gives it both from the one string.
#[must_use = "add it with `ui.add(…)`"]
pub struct IconButton<'a> {
    icon: Icon,
    tip: &'a str,
    disabled: Option<&'a str>,
}

impl<'a> IconButton<'a> {
    pub fn new(icon: Icon, tip: &'a str) -> Self {
        Self { icon, tip, disabled: None }
    }

    /// As [`ActionButton::disabled_because`].
    pub fn disabled_because(mut self, why: &'a str) -> Self {
        self.disabled = Some(why);
        self
    }
}

impl Widget for IconButton<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        refuse_a_silent_disable(ui, self.disabled);
        let t = Tokens::of(ui.ctx());
        let side = t.metrics.control_h;
        let enabled = self.disabled.is_none();
        let resp = ui
            .scope(|ui| {
                // 2 px all round, so the square `min_size` — not the glyph plus the cell padding —
                // decides the size: at every density the glyph and 2 px fit inside `control_h`.
                ui.spacing_mut().button_padding = egui::vec2(2.0, 2.0);
                let button = egui::Button::new(self.icon.rich().size(t.text.px(TextRole::Title)))
                    .frame_when_inactive(false)
                    .corner_radius(CornerRadius::same(RADIUS))
                    .min_size(egui::vec2(side, side));
                ui.add_enabled(enabled, button)
            })
            .inner;
        super::focus_ring(ui, &t, &resp);
        let resp = icons::named(resp, self.tip);
        match self.disabled {
            Some(why) => resp.on_disabled_hover_text(why),
            None => resp,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::appearance::Appearance;
    use crate::components::testing::{
        ctx_with, fills, harness, named, paint, paint_at, strokes, texts,
    };
    use crate::market::{MarketColors, MarketId};
    use crate::metrics::Density;
    use crate::theme::{Theme, ThemeId};
    use crate::type_scale::TextSize;
    use egui::accesskit::Role;
    use egui_kittest::kittest::NodeT;

    /// The primary fill IS the installed theme's accent on every theme — three of four would fail
    /// on a compile-time `palette::ACCENT` — and its label is the on-fill black.
    #[test]
    fn primary_is_filled_with_each_themes_accent() {
        for id in ThemeId::ALL {
            let ctx = ctx_with(&Appearance { theme: id, ..Appearance::default() });
            let shapes = paint(&ctx, |ui| {
                ui.add(ActionButton::primary("Save"));
            });
            assert!(fills(&shapes).contains(&Theme::of(id).accent), "{id:?}");
            assert!(texts(&shapes).contains(&("Save".to_string(), ON_FILL)), "{id:?}");
        }
    }

    #[test]
    fn buy_and_sell_follow_the_market_set() {
        for m in MarketId::ALL {
            let ctx = ctx_with(&Appearance { market: m, ..Appearance::default() });
            let shapes = paint(&ctx, |ui| {
                ui.add(ActionButton::buy("Buy"));
                ui.add(ActionButton::sell("Sell"));
            });
            let (f, c) = (fills(&shapes), MarketColors::of(m));
            assert!(f.contains(&c.up) && f.contains(&c.down), "{m:?}: {f:?}");
        }
    }

    #[test]
    fn secondary_is_an_outline_and_danger_the_status_red() {
        let ctx = ctx_with(&Appearance::default());
        let t = Tokens::of(&ctx);
        let shapes = paint(&ctx, |ui| {
            ui.add(ActionButton::secondary("Refresh"));
            ui.add(ActionButton::danger("Delete"));
        });
        assert!(fills(&shapes).contains(&t.theme.surface));
        assert!(strokes(&shapes).contains(&t.theme.border));
        assert!(fills(&shapes).contains(&Status::Error.color()));
    }

    /// Every button — words or icon, at either text size — is the density's control height.
    #[test]
    fn a_button_is_the_densitys_control_height() {
        for d in Density::ALL {
            for size in TextSize::ALL {
                let ctx =
                    ctx_with(&Appearance { density: d, text_size: size, ..Appearance::default() });
                let (mut words, mut icon) = (0.0, 0.0);
                paint(&ctx, |ui| {
                    words = ui.add(ActionButton::secondary("Refresh")).rect.height();
                    icon = ui.add(IconButton::new(icons::SETTINGS, "Settings")).rect.height();
                });
                assert_eq!(words, d.metrics().control_h, "{d:?} {size:?}");
                assert_eq!(icon, d.metrics().control_h, "icon {d:?} {size:?}");
            }
        }
    }

    /// A disabled kit button is disabled in the tree; the only way to make one is to give a reason.
    #[test]
    fn a_disabled_button_is_disabled_and_an_enabled_one_is_not() {
        let h = harness(Appearance::default(), |ui| {
            ui.add(ActionButton::primary("Apply").disabled_because("Nothing changed"));
            ui.add(ActionButton::secondary("Refresh"));
        });
        let node = |name: &str| {
            h.root()
                .children_recursive()
                .find(|n| n.accesskit_node().label().as_deref() == Some(name))
                .unwrap_or_else(|| panic!("{name} is not in the tree"))
        };
        assert!(node("Apply").accesskit_node().is_disabled());
        assert!(!node("Refresh").accesskit_node().is_disabled());
    }

    /// An icon without a word is named by its tip, not by a private-use character.
    #[test]
    fn an_icon_button_is_named_by_its_tip() {
        let h = harness(Appearance::default(), |ui| {
            ui.add(IconButton::new(icons::SETTINGS, "Chart settings"));
        });
        let buttons = named(&h, Role::Button);
        assert!(buttons.contains(&"Chart settings".to_string()), "{buttons:?}");
    }

    /// A kit button answers the pointer: hovered, every kind paints another fill than at rest. A
    /// fill set on the button itself would paint every state alike.
    #[test]
    fn a_kit_button_answers_the_pointer() {
        for kind in Kind::ALL {
            let ctx = ctx_with(&Appearance::default());
            let (at_rest, _, _) = kind.colours(&Tokens::of(&ctx));
            let mut rect = egui::Rect::NOTHING;
            let idle = paint(&ctx, |ui| rect = ui.add(ActionButton::new(kind, "Act")).rect);
            assert!(fills(&idle).contains(&at_rest), "{kind:?} at rest");
            // egui styles a button from the state its previous pass ended in: hover twice.
            paint_at(&ctx, rect.center(), |ui| {
                ui.add(ActionButton::new(kind, "Act"));
            });
            let hovered = paint_at(&ctx, rect.center(), |ui| {
                ui.add(ActionButton::new(kind, "Act"));
            });
            assert!(!fills(&hovered).contains(&at_rest), "{kind:?}: hovering changed nothing");
        }
    }

    /// `disabled_because` is the ONLY way to disable a kit button: one put in a disabled `Ui`
    /// without a reason would be a dead control that cannot say why, so debug builds refuse it
    /// (spec §4.2).
    #[test]
    #[should_panic(expected = "disabled_because")]
    fn a_kit_button_disabled_without_a_reason_is_refused() {
        let ctx = ctx_with(&Appearance::default());
        paint(&ctx, |ui| {
            ui.add_enabled(false, ActionButton::primary("Apply"));
        });
    }

    #[test]
    #[should_panic(expected = "disabled_because")]
    fn an_icon_button_disabled_without_a_reason_is_refused() {
        let ctx = ctx_with(&Appearance::default());
        paint(&ctx, |ui| {
            ui.add_enabled(false, IconButton::new(icons::SETTINGS, "Settings"));
        });
    }

    /// ...and the reason is what hovering the disabled button shows.
    #[test]
    fn a_disabled_button_says_why_on_hover() {
        let mut h = harness(Appearance::default(), |ui| {
            ui.ctx().all_styles_mut(|s| s.interaction.tooltip_delay = 0.0);
            ui.add(ActionButton::primary("Apply").disabled_because("Nothing changed"));
        });
        h.root()
            .children_recursive()
            .find(|n| n.accesskit_node().label().as_deref() == Some("Apply"))
            .expect("the disabled button is in the tree")
            .hover();
        h.run();
        let labels = named(&h, Role::Label);
        assert!(labels.contains(&"Nothing changed".to_string()), "{labels:?}");
    }
}
