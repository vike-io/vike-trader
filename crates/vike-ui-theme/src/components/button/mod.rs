//! Buttons (design system spec §4.1): primary, secondary, buy, sell and danger as
//! [`ThemeButtonAction`], and the icon-only [`ThemeButtonIcon`]. Every one is the density's control height
//! and rounded by `RADIUS`.

use egui::{Atoms, Color32, CornerRadius, IntoAtoms, Response, Stroke, Ui, Widget};

use super::ThemeTokens;
use crate::icons::{self, ThemeIcon};
use crate::maps::{self, ThemeMapRow};
use crate::metrics::{RADIUS, alpha, space, stroke};
use crate::roles::ThemeColourRole;
use crate::type_scale::ThemeTextRole;

/// What a button does, which decides how it is painted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ThemeButtonKind {
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

impl ThemeButtonKind {
    pub const ALL: [ThemeButtonKind; 5] = [
        ThemeButtonKind::Primary,
        ThemeButtonKind::Secondary,
        ThemeButtonKind::Buy,
        ThemeButtonKind::Sell,
        ThemeButtonKind::Danger,
    ];

    /// The row of `ui-theme.toml`'s `button` map this kind wears: its fill, outline and label, and the
    /// fill under the pointer. Exhaustive: a kind without a row does not compile. The Trade window's
    /// ticket buttons read the same rows (`vike-order-entry`'s ticket `Look`), so the two cannot disagree.
    pub fn row(self) -> &'static ThemeMapRow {
        match self {
            ThemeButtonKind::Primary => &maps::button::PRIMARY,
            ThemeButtonKind::Secondary => &maps::button::SECONDARY,
            ThemeButtonKind::Buy => &maps::button::BUY,
            ThemeButtonKind::Sell => &maps::button::SELL,
            ThemeButtonKind::Danger => &maps::button::DANGER,
        }
    }

    /// `(fill, stroke, label)` under `t`: the row's `fill`, `stroke` (a hairline, or none) and `text`.
    /// The accent is a SHAPE here — the primary fill — never a label colour; every filled kind
    /// carries the on-fill black.
    pub fn colours(self, t: &ThemeTokens) -> (Color32, Stroke, Color32) {
        let row = self.row();
        (row.fill.resolve(t), row.stroke.stroke(stroke::HAIRLINE, t), row.text.resolve(t))
    }

    /// The fill under the pointer and while pressed: the row's `colour` where it names one (the
    /// outlined kind takes the theme's hover fill, as egui's own buttons do); a filled kind, whose row
    /// names none, moves its fill a step toward the text colour — lighter on every dark theme, so the
    /// on-fill black label only gains contrast.
    fn lit(self, t: &ThemeTokens) -> Color32 {
        match self.row().colour {
            ThemeColourRole::None => self.colours(t).0.lerp_to_gamma(t.theme.text, alpha::LIFT),
            hover => hover.resolve(t),
        }
    }
}

/// Button padding: the density's cell padding across, and down 2 px — or less, when the words' own
/// row leaves no room for 2 px each side inside `control_height` — so the control height, not the text,
/// sets how tall a button is. Compact is 20 pt tall and Large's Strong row is 18: at 2 px each side
/// the button was 22 and a Compact row grew by two points on the biggest text size. The padding is
/// rounded DOWN to the hundredth, so the row and its padding never add up to a hair over `control_height`.
fn padding(ui: &Ui, t: &ThemeTokens) -> egui::Vec2 {
    let row = ui.ctx().fonts_mut(|f| f.row_height(&t.font(ThemeTextRole::Strong)));
    let y = ((t.metrics.control_height - row) / 2.0 * 100.0).floor() / 100.0;
    egui::vec2(t.metrics.padding, y.clamp(0.0, space::XS))
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

/// A kit button: `ui.add(ThemeButtonAction::primary("Save"))`, or with an icon,
/// `ui.add(ThemeButtonAction::secondary((icons::REFRESH, "Refresh")))`. Its words are the Strong role
/// (spec §3.3: "window titles, buttons, tabs"), and an icon among them takes the words' size.
#[must_use = "add it with `ui.add(…)`"]
pub struct ThemeButtonAction<'a> {
    kind: ThemeButtonKind,
    atoms: Atoms<'a>,
    disabled: Option<&'a str>,
    /// `Some` only for a toggle ([`ThemeButtonAction::selected`]).
    selected: Option<bool>,
}

impl<'a> ThemeButtonAction<'a> {
    pub fn new(kind: ThemeButtonKind, atoms: impl IntoAtoms<'a>) -> Self {
        Self { kind, atoms: atoms.into_atoms(), disabled: None, selected: None }
    }
    pub fn primary(atoms: impl IntoAtoms<'a>) -> Self {
        Self::new(ThemeButtonKind::Primary, atoms)
    }
    pub fn secondary(atoms: impl IntoAtoms<'a>) -> Self {
        Self::new(ThemeButtonKind::Secondary, atoms)
    }
    pub fn buy(atoms: impl IntoAtoms<'a>) -> Self {
        Self::new(ThemeButtonKind::Buy, atoms)
    }
    pub fn sell(atoms: impl IntoAtoms<'a>) -> Self {
        Self::new(ThemeButtonKind::Sell, atoms)
    }
    pub fn danger(atoms: impl IntoAtoms<'a>) -> Self {
        Self::new(ThemeButtonKind::Danger, atoms)
    }

    /// Makes the button a TOGGLE, shown PRESSED while `on`: the card fill outlined in the accent, as
    /// the segmented control's chosen segment and a pressed [`ThemeButtonIcon`] are. Only a button given
    /// this reports a pressed or unpressed state to a screen reader.
    pub fn selected(mut self, on: bool) -> Self {
        self.selected = Some(on);
        self
    }

    /// Disable it and say why on hover. The ONLY way to disable a kit button: a control with
    /// nothing behind it is disabled and says why — never a live, dead button (spec §4.2).
    pub fn disabled_because(mut self, why: &'a str) -> Self {
        self.disabled = Some(why);
        self
    }
}

impl Widget for ThemeButtonAction<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        refuse_a_silent_disable(ui, self.disabled);
        let t = ThemeTokens::of(ui.ctx());
        let (mut fill, mut stroke, ink) = self.kind.colours(&t);
        let mut lit = self.kind.lit(&t);
        if self.selected == Some(true) {
            fill = t.theme.card;
            lit = t.theme.hover;
            stroke = egui::Stroke::new(stroke::HAIRLINE, t.theme.accent);
        }
        let enabled = self.disabled.is_none();
        let response = ui
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
                ui.style_mut().override_font_id = Some(t.font(ThemeTextRole::Strong));
                let pad = padding(ui, &t);
                ui.spacing_mut().button_padding = pad;
                let mut button = egui::Button::new(self.atoms)
                    .corner_radius(CornerRadius::same(RADIUS))
                    .min_size(egui::vec2(0.0, t.metrics.control_height));
                if let Some(on) = self.selected {
                    button = button.selected(on);
                }
                ui.add_enabled(enabled, button)
            })
            .inner;
        super::focus_ring(ui, &t, &response);
        match self.disabled {
            Some(why) => response.on_disabled_hover_text(why),
            None => response,
        }
    }
}

/// An icon-only button: frameless until hovered, a `control_height` square, and its `tip` both as the
/// hover text and as the accessible name — an icon without a word needs both, and
/// [`icons::named`] gives it both from the one string.
#[must_use = "add it with `ui.add(…)`"]
pub struct ThemeButtonIcon<'a> {
    icon: ThemeIcon,
    tip: &'a str,
    disabled: Option<&'a str>,
    /// `Some` only for a toggle ([`ThemeButtonIcon::selected`]). `None` is a plain button, which a screen
    /// reader must not announce as a toggle at all.
    selected: Option<bool>,
    /// `Some` only where a design fixes the button's size instead of the density's square
    /// ([`ThemeButtonIcon::sized`]): the box, and the role the glyph is drawn at.
    sized: Option<(egui::Vec2, ThemeTextRole)>,
}

impl<'a> ThemeButtonIcon<'a> {
    pub fn new(icon: ThemeIcon, tip: &'a str) -> Self {
        Self { icon, tip, disabled: None, selected: None, sized: None }
    }

    /// A box of the design's own size, drawn with its glyph at `glyph`, at every density: for a
    /// control whose size the design fixes and the density must not scale (the Trade window's title
    /// bar, 25 pt tall, holds 24 × 20 toggles at Comfortable too). Without it the button is a
    /// `control_height` square with the Title glyph, which a bar of that height cannot hold.
    pub fn sized(mut self, size: egui::Vec2, glyph: ThemeTextRole) -> Self {
        self.sized = Some((size, glyph));
        self
    }

    /// As [`ThemeButtonAction::disabled_because`].
    pub fn disabled_because(mut self, why: &'a str) -> Self {
        self.disabled = Some(why);
        self
    }

    /// Makes the button a TOGGLE whose state is the button itself (the Trade window's title bar),
    /// shown PRESSED while `on`: the segmented control's chosen look, the card fill outlined in the
    /// accent. Unpressed it is frameless, like any icon button. Only a button given this reports a
    /// pressed or unpressed state to a screen reader.
    pub fn selected(mut self, on: bool) -> Self {
        self.selected = Some(on);
        self
    }
}

impl Widget for ThemeButtonIcon<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        refuse_a_silent_disable(ui, self.disabled);
        let t = ThemeTokens::of(ui.ctx());
        let (size, glyph) = self.sized.unwrap_or((
            egui::vec2(t.metrics.control_height, t.metrics.control_height),
            ThemeTextRole::Title,
        ));
        let enabled = self.disabled.is_none();
        let response = ui
            .scope(|ui| {
                // 2 px all round, so the square `min_size` — not the glyph plus the cell padding —
                // decides the size: at every density the glyph and 2 px fit inside `control_height`.
                ui.spacing_mut().button_padding = egui::vec2(space::XS, space::XS);
                let pressed = self.selected == Some(true);
                // A glyph never outgrows its button: the 2 px of padding each side leave `size.y - 4`
                // for it, and Large's Title role is 17 in a Compact bar 20 tall.
                let glyph_px = t.text.px(glyph).min(size.y - 4.0);
                let mut button = egui::Button::new(self.icon.rich().size(glyph_px))
                    .frame_when_inactive(pressed)
                    .corner_radius(CornerRadius::same(RADIUS))
                    .min_size(size);
                // `Button::selected` opts the node into toggle semantics, so a plain icon button
                // never calls it: every close, minimize and toolbar icon would otherwise be read out
                // as an unpressed toggle.
                if let Some(on) = self.selected {
                    button = button.selected(on);
                }
                if pressed {
                    button = button
                        .fill(t.theme.card)
                        .stroke(Stroke::new(stroke::HAIRLINE, t.theme.accent));
                }
                ui.add_enabled(enabled, button)
            })
            .inner;
        super::focus_ring(ui, &t, &response);
        let response = icons::named(response, self.tip);
        match self.disabled {
            Some(why) => response.on_disabled_hover_text(why),
            None => response,
        }
    }
}

#[cfg(test)]
mod tests;
