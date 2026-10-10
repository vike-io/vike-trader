//! Buttons (design system spec §4.1): primary, secondary, buy, sell and danger as
//! [`ActionButton`], and the icon-only [`IconButton`]. Every one is the density's control height
//! and rounded by `RADIUS`.

use egui::{Atoms, Color32, CornerRadius, IntoAtoms, Response, Stroke, Ui, Widget};

use super::Tokens;
use crate::icons::{self, Icon};
use crate::maps::{self, MapRow};
use crate::metrics::{RADIUS, alpha, space, stroke};
use crate::roles::ColourRole;
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

    /// The row of `ui-theme.toml`'s `button` map this kind wears: its fill, outline and label, and the
    /// fill under the pointer. Exhaustive: a kind without a row does not compile. The Trade window's
    /// ticket buttons read the same rows (`vike-panels`'s ticket `Look`), so the two cannot disagree.
    pub fn row(self) -> &'static MapRow {
        match self {
            Kind::Primary => &maps::button::PRIMARY,
            Kind::Secondary => &maps::button::SECONDARY,
            Kind::Buy => &maps::button::BUY,
            Kind::Sell => &maps::button::SELL,
            Kind::Danger => &maps::button::DANGER,
        }
    }

    /// `(fill, stroke, label)` under `t`: the row's `fill`, `stroke` (a hairline, or none) and `text`.
    /// The accent is a SHAPE here — the primary fill — never a label colour; every filled kind
    /// carries the on-fill black.
    pub fn colours(self, t: &Tokens) -> (Color32, Stroke, Color32) {
        let row = self.row();
        (row.fill.resolve(t), row.stroke.stroke(stroke::HAIRLINE, t), row.text.resolve(t))
    }

    /// The fill under the pointer and while pressed: the row's `colour` where it names one (the
    /// outlined kind takes the theme's hover fill, as egui's own buttons do); a filled kind, whose row
    /// names none, moves its fill a step toward the text colour — lighter on every dark theme, so the
    /// on-fill black label only gains contrast.
    fn lit(self, t: &Tokens) -> Color32 {
        match self.row().colour {
            ColourRole::None => self.colours(t).0.lerp_to_gamma(t.theme.text, alpha::LIFT),
            hover => hover.resolve(t),
        }
    }
}

/// Button padding: the density's cell padding across, and down 2 px — or less, when the words' own
/// row leaves no room for 2 px each side inside `control_h` — so the control height, not the text,
/// sets how tall a button is. Compact is 20 pt tall and Large's Strong row is 18: at 2 px each side
/// the button was 22 and a Compact row grew by two points on the biggest text size. The padding is
/// rounded DOWN to the hundredth, so the row and its padding never add up to a hair over `control_h`.
fn padding(ui: &Ui, t: &Tokens) -> egui::Vec2 {
    let row = ui.ctx().fonts_mut(|f| f.row_height(&t.font(TextRole::Strong)));
    let y = ((t.metrics.control_h - row) / 2.0 * 100.0).floor() / 100.0;
    egui::vec2(t.metrics.pad, y.clamp(0.0, space::XS))
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
    /// `Some` only for a toggle ([`ActionButton::selected`]).
    selected: Option<bool>,
}

impl<'a> ActionButton<'a> {
    pub fn new(kind: Kind, atoms: impl IntoAtoms<'a>) -> Self {
        Self { kind, atoms: atoms.into_atoms(), disabled: None, selected: None }
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

    /// Makes the button a TOGGLE, shown PRESSED while `on`: the card fill outlined in the accent, as
    /// the segmented control's chosen segment and a pressed [`IconButton`] are. Only a button given
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

impl Widget for ActionButton<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        refuse_a_silent_disable(ui, self.disabled);
        let t = Tokens::of(ui.ctx());
        let (mut fill, mut stroke, ink) = self.kind.colours(&t);
        let mut lit = self.kind.lit(&t);
        if self.selected == Some(true) {
            fill = t.theme.card;
            lit = t.theme.hover;
            stroke = egui::Stroke::new(stroke::HAIRLINE, t.theme.accent);
        }
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
                let pad = padding(ui, &t);
                ui.spacing_mut().button_padding = pad;
                let mut button = egui::Button::new(self.atoms)
                    .corner_radius(CornerRadius::same(RADIUS))
                    .min_size(egui::vec2(0.0, t.metrics.control_h));
                if let Some(on) = self.selected {
                    button = button.selected(on);
                }
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
    /// `Some` only for a toggle ([`IconButton::selected`]). `None` is a plain button, which a screen
    /// reader must not announce as a toggle at all.
    selected: Option<bool>,
    /// `Some` only where a design fixes the button's size instead of the density's square
    /// ([`IconButton::sized`]): the box, and the role the glyph is drawn at.
    sized: Option<(egui::Vec2, TextRole)>,
}

impl<'a> IconButton<'a> {
    pub fn new(icon: Icon, tip: &'a str) -> Self {
        Self { icon, tip, disabled: None, selected: None, sized: None }
    }

    /// A box of the design's own size, drawn with its glyph at `glyph`, at every density: for a
    /// control whose size the design fixes and the density must not scale (the Trade window's title
    /// bar, 25 pt tall, holds 24 × 20 toggles at Comfortable too). Without it the button is a
    /// `control_h` square with the Title glyph, which a bar of that height cannot hold.
    pub fn sized(mut self, size: egui::Vec2, glyph: TextRole) -> Self {
        self.sized = Some((size, glyph));
        self
    }

    /// As [`ActionButton::disabled_because`].
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

impl Widget for IconButton<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        refuse_a_silent_disable(ui, self.disabled);
        let t = Tokens::of(ui.ctx());
        let (size, glyph) = self
            .sized
            .unwrap_or((egui::vec2(t.metrics.control_h, t.metrics.control_h), TextRole::Title));
        let enabled = self.disabled.is_none();
        let resp = ui
            .scope(|ui| {
                // 2 px all round, so the square `min_size` — not the glyph plus the cell padding —
                // decides the size: at every density the glyph and 2 px fit inside `control_h`.
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
    use crate::components::{ON_FILL, Status};
    use crate::market::{MarketColors, MarketId};
    use crate::metrics::Density;
    use crate::theme::{Theme, ThemeId};
    use crate::type_scale::TextSize;
    use egui::accesskit::{Role, Toggled};
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

    /// Each kind's fill, outline, label and fill under the pointer are what the kit painted before
    /// they moved to `ui-theme.toml`'s `button` map: primary the accent, secondary the surface
    /// outlined in the border with the UI text and the theme's hover fill under the pointer, buy and
    /// sell the market set's up and down, danger the status red; every filled one carries the
    /// on-fill black and is lifted a step toward the text colour. Changing a row of the table changes
    /// a button, and this test is what says so by name.
    #[test]
    fn a_kind_wears_the_fill_outline_label_and_hover_the_button_map_gives_it() {
        for id in ThemeId::ALL {
            for m in MarketId::ALL {
                let look = Appearance { theme: id, market: m, ..Appearance::default() };
                let t = Tokens::from_appearance(&look);
                let lifted = |fill: Color32| fill.lerp_to_gamma(t.theme.text, alpha::LIFT);
                let hairline = Stroke::new(stroke::HAIRLINE, t.theme.border);
                for (kind, fill, outline, label, lit) in [
                    (Kind::Primary, t.theme.accent, Stroke::NONE, ON_FILL, lifted(t.theme.accent)),
                    (Kind::Secondary, t.theme.surface, hairline, t.theme.text_ui, t.theme.hover),
                    (Kind::Buy, t.market.up, Stroke::NONE, ON_FILL, lifted(t.market.up)),
                    (Kind::Sell, t.market.down, Stroke::NONE, ON_FILL, lifted(t.market.down)),
                    (
                        Kind::Danger,
                        Status::Error.color(),
                        Stroke::NONE,
                        ON_FILL,
                        lifted(Status::Error.color()),
                    ),
                ] {
                    let at = format!("{id:?} {m:?} {kind:?}");
                    assert_eq!(kind.colours(&t), (fill, outline, label), "{at}");
                    assert_eq!(kind.lit(&t), lit, "{at}: under the pointer");
                }
            }
        }
    }

    /// The map is the kit's own: every kind reads the row of its own name, no two share one, and the
    /// one row of the `button` map that no kind reads is the Trade window's chosen look (read by the
    /// ticket's `Look`, which `vike-panels` pins).
    #[test]
    fn every_kind_has_its_own_row_and_the_one_row_no_kind_reads_is_the_chosen_look() {
        for kind in Kind::ALL {
            assert_eq!(
                kind.row().key,
                format!("{kind:?}").to_uppercase(),
                "{kind:?} reads another row"
            );
            assert_eq!(
                Kind::ALL.iter().filter(|o| o.row() == kind.row()).count(),
                1,
                "{kind:?}'s row is shared"
            );
        }
        let mut unread = Vec::new();
        for row in maps::button::ALL {
            if !Kind::ALL.iter().any(|k| k.row() == *row) {
                unread.push(row.key);
            }
        }
        assert_eq!(unread, ["CHOSEN"], "a row of the button map is read by no kind");
    }

    /// A kind is always drawn: it has a fill and a label (never `none`), a filled kind has no
    /// outline and carries the on-fill label, and only the outlined kind names a fill for the
    /// pointer — a filled kind's is derived in code, so a role on its row would be read by nothing.
    #[test]
    fn a_kind_always_has_a_fill_and_a_label_and_only_the_outlined_one_names_a_hover_fill() {
        for kind in Kind::ALL {
            let row = kind.row();
            assert_ne!(row.fill, ColourRole::None, "{kind:?}: a button with no fill");
            assert_ne!(row.text, ColourRole::None, "{kind:?}: a button with no label");
            let outlined = row.stroke != ColourRole::None;
            assert_eq!(outlined, kind == Kind::Secondary, "{kind:?}: the outlined kind");
            assert_eq!(row.colour != ColourRole::None, outlined, "{kind:?}: its own hover fill");
            if !outlined {
                assert_eq!(row.text, ColourRole::OnFill, "{kind:?}: a filled kind's label");
            }
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

    /// A pressed icon button shows the segmented control's chosen look: outlined in the theme's
    /// accent. An unpressed toggle is not, and neither is a plain icon button (the Trade window's
    /// title-bar toggles, spec §3.2).
    #[test]
    fn a_selected_icon_button_is_outlined_in_each_themes_accent() {
        for id in ThemeId::ALL {
            let ctx = ctx_with(&Appearance { theme: id, ..Appearance::default() });
            let button = |on: Option<bool>| {
                let b = IconButton::new(icons::PANEL_BESIDE, "Ticket beside the ladder");
                match on {
                    Some(on) => b.selected(on),
                    None => b,
                }
            };
            let accent = Theme::of(id).accent;
            let outlined = |on: Option<bool>| {
                strokes(&paint(&ctx, |ui| {
                    ui.add(button(on));
                }))
                .contains(&accent)
            };
            assert!(outlined(Some(true)), "{id:?}: the pressed one");
            assert!(!outlined(Some(false)), "{id:?}: an unpressed toggle");
            assert!(!outlined(None), "{id:?}: a plain icon button");
        }
    }

    /// Only a TOGGLE reports a pressed state. egui's `Button::selected` opts a button into toggle
    /// semantics, so calling it on every icon button would have a screen reader announce each close,
    /// minimize and toolbar icon as an unpressed toggle. A plain icon button reports no toggled state
    /// at all; one given `selected` reports exactly the state it was given.
    #[test]
    fn only_an_icon_button_given_a_state_is_announced_as_a_toggle() {
        let h = harness(Appearance::default(), |ui| {
            ui.add(IconButton::new(icons::SETTINGS, "Settings"));
            ui.add(IconButton::new(icons::PANEL_BESIDE, "Ticket beside the ladder").selected(true));
            ui.add(IconButton::new(icons::PANEL_UNDER, "Ticket under the ladder").selected(false));
        });
        let toggled = |name: &str| {
            h.root()
                .children_recursive()
                .find(|n| n.accesskit_node().label().as_deref() == Some(name))
                .unwrap_or_else(|| panic!("{name} is not in the tree"))
                .accesskit_node()
                .toggled()
        };
        assert_eq!(toggled("Settings"), None, "a plain icon button is not a toggle");
        assert_eq!(toggled("Ticket beside the ladder"), Some(Toggled::True));
        assert_eq!(toggled("Ticket under the ladder"), Some(Toggled::False));
    }
}
