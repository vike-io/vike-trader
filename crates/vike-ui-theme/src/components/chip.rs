//! Chips, dots, badges and marks (spec §4.1, §4.2):
//! - the LIVE / DEMO / PAPER tier chips and an account chip;
//! - the status dot with its label;
//! - the count badge and the outlined badge;
//! - the Connections presence mark, with its one-line legend.
//!
//! A chip draws the account's TIER, `vike_model::AccountTier` — the one tier type. The three-word
//! `Mode` enum this file used to declare beside it is gone (decision 0156): a second enum of the
//! same three words was only a place for the chip and the account row to disagree.

use egui::{
    Color32, CornerRadius, FontFamily, FontId, Response, RichText, Role, Sense, Stroke, StrokeKind,
    Ui, WidgetInfo,
};
use vike_model::AccountTier;

use super::venue_chip::chip_pad;
use super::{Status, Tokens};
use crate::fonts::SEMIBOLD;
use crate::maps::{self, MapRow};
use crate::metrics::{RADIUS, alpha, stroke};
use crate::roles::{ModeRow, modes};
use crate::type_scale::TextRole;

/// The three tiers in the order a legend lists them: LIVE first, the one that spends real money.
/// (`AccountTier::ALL` is the tiers' RANK order, `Paper < Demo < Live`; a legend reads the other way.)
pub const TIERS_LISTED: [AccountTier; 3] =
    [AccountTier::Live, AccountTier::Demo, AccountTier::Paper];

/// The word a chip shows: the tier's one spelling (`AccountTier::as_str`) upper-cased, as decision
/// 0156 item 6 rules (`a_chip_shows_the_tiers_own_word_upper_cased` holds the two together).
pub fn tier_label(tier: AccountTier) -> &'static str {
    match tier {
        AccountTier::Live => "LIVE",
        AccountTier::Demo => "DEMO",
        AccountTier::Paper => "PAPER",
    }
}

/// This tier's `[[mode]]` row of `ui-theme.toml`: the colour ROLE each part of its chip and of
/// the venue chip's mark wears. The match only ties a tier to its generated constant, so a tier
/// with no row does not compile, and `every_mode_has_its_row_and_every_row_its_mode` holds the
/// other direction. Which colour a tier wears is the TOML's to say.
pub fn tier_roles(tier: AccountTier) -> &'static ModeRow {
    match tier {
        AccountTier::Live => &modes::LIVE,
        AccountTier::Demo => &modes::DEMO,
        AccountTier::Paper => &modes::PAPER,
    }
}

/// `(fill, stroke, text)`: this tier's row, resolved against `t`. What the roles are, and the
/// rulings behind them (LIVE the accent shape, DEMO the info blue, PAPER the border grey), is
/// each row's own `doc` in `ui-theme.toml`.
pub fn tier_colours(tier: AccountTier, t: &Tokens) -> (Color32, Stroke, Color32) {
    let row = tier_roles(tier);
    (row.fill.resolve(t), row.stroke.stroke(stroke::HAIRLINE, t), row.text.resolve(t))
}

/// `t`'s background washed toward this tier's FILL: the fill of a region that marks the tier
/// without taking any of its room. The Trade window washes the confirm prompt of an order held
/// on a LIVE account in LIVE's own fill — a low tint of the theme's accent — so the chip and
/// the prompt read one colour and cannot drift apart. Opaque, so what is written over it is
/// measured exactly: on LIVE's wash the theme's text, the warning amber and LIVE's fill itself
/// each clear the 4.5:1 text floor, on every theme
/// (`the_live_wash_keeps_what_is_written_on_it_readable`). A tier that is not filled (DEMO,
/// PAPER) has nothing to wash toward and gives back the background.
pub fn tier_wash(tier: AccountTier, t: &Tokens) -> Color32 {
    let (fill, ..) = tier_colours(tier, t);
    if fill == Color32::TRANSPARENT {
        t.theme.bg
    } else {
        t.theme.bg.lerp_to_gamma(fill, alpha::WASH)
    }
}

/// A rounded box around `text`: the one shape every chip here is.
fn pill(
    ui: &mut Ui,
    t: &Tokens,
    text: &str,
    font: FontId,
    fill: Color32,
    stroke: Stroke,
    ink: Color32,
) -> Response {
    let galley = ui.painter().layout_no_wrap(text.to_string(), font, ink);
    let pad = egui::vec2(chip_pad(t), 2.0);
    let (rect, resp) = ui.allocate_exact_size(galley.size() + pad * 2.0, Sense::hover());
    let p = ui.painter();
    p.rect(rect, CornerRadius::same(RADIUS), fill, stroke, StrokeKind::Inside);
    p.galley(rect.min + pad, galley, ink);
    resp.widget_info(|| WidgetInfo::labeled(Role::Label, true, text));
    resp
}

/// A LIVE / DEMO / PAPER chip for `tier`, in the caption role at the semibold weight.
pub fn tier(ui: &mut Ui, tier: AccountTier) -> Response {
    let t = Tokens::of(ui.ctx());
    let (fill, stroke, ink) = tier_colours(tier, &t);
    let font = FontId::new(t.text.px(TextRole::Caption), FontFamily::Name(SEMIBOLD.into()));
    pill(ui, &t, tier_label(tier), font, fill, stroke, ink)
}

/// An account chip: its tier's own chip, then its name. The SELECTED account is a label outlined in
/// the accent — never a button, so "which account" is answered by role (spec §4.2); every other
/// account is a button outlined in the border grey.
///
/// ⚠ The mode is a CHIP — LIVE filled, DEMO outlined, in the colours of its `[[mode]]` row — never a
/// coloured dot. A LIVE dot in the accent beside a DEMO dot would rest on colour alone, and the mode
/// is carried by SHAPE (spec §3.1): the accent fill against an outline. (DEMO was the warning amber,
/// 4° of hue from Carbon's accent, until the owner took the design's blue on 2026-10-05.)
///
/// ⚠ One exception, by the owner's ruling of 2026-10-04, and it is a DIFFERENT component: the symbol
/// picker's venue chip (`crate::components::venue_chip::VenueChip`) marks the mode as a dot whose
/// shape carries it (filled, ring, grey ring) with the mode also spelled in words as its hover
/// text and accessible name. `account` itself still never draws a dot, and
/// `an_account_shows_its_mode_as_a_chip_not_a_dot_on_every_theme` still holds.
pub fn account(ui: &mut Ui, name: &str, tier: AccountTier, selected: bool) -> Response {
    let t = Tokens::of(ui.ctx());
    let (tag_fill, tag_edge, tag_ink) = tier_colours(tier, &t);
    let tag_font = FontId::new(t.text.px(TextRole::Caption), FontFamily::Name(SEMIBOLD.into()));
    let tag = ui.painter().layout_no_wrap(tier_label(tier).to_string(), tag_font, tag_ink);
    let words = ui.painter().layout_no_wrap(name.to_string(), t.font(TextRole::Body), t.theme.text);
    let tag_pad = egui::vec2((t.metrics.pad * 0.5).round(), 1.0);
    let tag_size = tag.size() + tag_pad * 2.0;
    let w = 2.0 * t.metrics.pad + tag_size.x + t.metrics.gap + words.size().x;
    let sense = if selected { Sense::hover() } else { Sense::click() };
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, t.metrics.control_h), sense);
    let edge = if selected { t.theme.accent } else { t.theme.border };
    let fill = if resp.hovered() && !selected { t.theme.hover } else { Color32::TRANSPARENT };
    let p = ui.painter();
    p.rect(
        rect,
        CornerRadius::same(RADIUS),
        fill,
        Stroke::new(stroke::HAIRLINE, edge),
        StrokeKind::Inside,
    );
    let tag_at = egui::pos2(rect.left() + t.metrics.pad, rect.center().y - tag_size.y / 2.0);
    let tag_rect = egui::Rect::from_min_size(tag_at, tag_size);
    p.rect(tag_rect, CornerRadius::same(RADIUS), tag_fill, tag_edge, StrokeKind::Inside);
    p.galley(tag_rect.min + tag_pad, tag, tag_ink);
    let x = tag_rect.right() + t.metrics.gap;
    p.galley(egui::pos2(x, rect.center().y - words.size().y / 2.0), words, t.theme.text);
    super::focus_ring(ui, &t, &resp);
    let typ = if selected { Role::Label } else { Role::Button };
    resp.widget_info(|| WidgetInfo::labeled(typ, true, name));
    resp
}

/// A status dot and its label. The dot is the status colour — fixed in every theme, so
/// "connected" is `Status::Ok`, never the accent (spec §2) — and the label is secondary text.
pub fn status_dot(ui: &mut Ui, status: Status, label: &str) -> Response {
    let t = Tokens::of(ui.ctx());
    let galley =
        ui.painter().layout_no_wrap(label.to_string(), t.font(TextRole::Body), t.theme.text2);
    let d = (t.text.px(TextRole::Caption) * 0.7).round();
    let size = egui::vec2(d + t.metrics.gap + galley.size().x, galley.size().y.max(d));
    let (rect, resp) = ui.allocate_exact_size(size, Sense::hover());
    let p = ui.painter();
    p.circle_filled(egui::pos2(rect.left() + d / 2.0, rect.center().y), d / 2.0, status.color());
    let x = rect.left() + d + t.metrics.gap;
    p.galley(egui::pos2(x, rect.center().y - galley.size().y / 2.0), galley, t.theme.text2);
    resp.widget_info(|| WidgetInfo::labeled(Role::Label, true, label));
    resp
}

/// A count badge. `None` — a count that is not known — draws NOTHING and answers `None`: an
/// unknown count is never shown as 0 (spec §4.2).
pub fn count(ui: &mut Ui, n: Option<u64>) -> Option<Response> {
    let n = n?;
    let t = Tokens::of(ui.ctx());
    let text = n.to_string();
    let galley =
        ui.painter().layout_no_wrap(text.clone(), t.mono(TextRole::Caption), t.theme.text2);
    let h = galley.size().y + 2.0;
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(galley.size().x + h, h), Sense::hover());
    let p = ui.painter();
    // Pills and dots stay round (spec §3.4).
    p.rect_filled(rect, CornerRadius::same((h / 2.0) as u8), t.theme.card);
    p.galley(rect.center() - galley.size() / 2.0, galley, t.theme.text2);
    resp.widget_info(|| WidgetInfo::labeled(Role::Label, true, &text));
    Some(resp)
}

/// A badge's text colour: the status colour, except `Muted`, whose grey is below the 4.5:1 text
/// floor — its text is the caption grey instead (`every_badge_reads_on_every_theme`).
pub fn badge_ink(t: &Tokens, status: Status) -> Color32 {
    match status {
        Status::Muted => t.theme.text3,
        s => s.color(),
    }
}

/// An outlined badge (spec §4.2, from the Connections redesign): short mono text inside an outline
/// in a status colour — `READ-ONLY` in the warning colour, say.
pub fn badge(ui: &mut Ui, text: &str, status: Status) -> Response {
    let t = Tokens::of(ui.ctx());
    badge_coloured(ui, &t, text, status.color(), badge_ink(&t, status))
}

/// [`badge`] in two colours the caller names: `edge` the outline, `ink` the words. What a map row says
/// (`ui-theme.toml`'s `feed_badge`: its `stroke` and its `text`) in place of one [`Status`], so a badge
/// whose table decides its colours need not be a status's own. [`badge`] is this with a status's colour
/// and [`badge_ink`].
pub fn badge_coloured(
    ui: &mut Ui,
    t: &Tokens,
    text: &str,
    edge: Color32,
    ink: Color32,
) -> Response {
    let edge = Stroke::new(stroke::HAIRLINE, edge);
    pill(ui, t, text, t.mono(TextRole::Caption), Color32::TRANSPARENT, edge, ink)
}

/// The four presence marks of the Connections credential rail (spec §4.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Presence {
    Configured,
    NotSet,
    NoSuchTier,
    Unknown,
}

impl Presence {
    pub const ALL: [Presence; 4] =
        [Presence::Configured, Presence::NotSet, Presence::NoSuchTier, Presence::Unknown];

    /// This mark's row of `ui-theme.toml`'s `presence` map: `colour` the mark's colour, `word` the word the
    /// legend gives it. Exhaustive: a mark without a row does not compile, and
    /// `every_mark_has_its_own_row_and_every_row_its_mark` holds the other direction.
    fn row(self) -> &'static MapRow {
        match self {
            Presence::Configured => &maps::presence::CONFIGURED,
            Presence::NotSet => &maps::presence::NOT_SET,
            Presence::NoSuchTier => &maps::presence::NO_SUCH_TIER,
            Presence::Unknown => &maps::presence::UNKNOWN,
        }
    }

    /// The mark — text, not an icon: `crates/vike-gateway-panel/src/view.rs`'s rail prints it. (A row has no
    /// field for a text glyph, so the four stay here.)
    pub fn glyph(self) -> &'static str {
        match self {
            Presence::Configured => "\u{25CF}",
            Presence::NotSet => "\u{25CB}",
            Presence::NoSuchTier => "\u{00B7}",
            Presence::Unknown => "?",
        }
    }

    /// The word the legend gives it: its row's `word`.
    pub fn word(self) -> &'static str {
        self.row().word.unwrap_or("")
    }

    /// The mark's colour: its row's `colour`.
    pub fn color(self, t: &Tokens) -> Color32 {
        self.row().colour.resolve(t)
    }
}

/// One presence mark; hovering it names the key it is about.
pub fn presence(ui: &mut Ui, mark: Presence, key: &str) -> Response {
    let t = Tokens::of(ui.ctx());
    let text = RichText::new(mark.glyph()).font(t.mono(TextRole::Body)).color(mark.color(&t));
    ui.add(egui::Label::new(text).selectable(false)).on_hover_text(key)
}

/// The marks' one-line legend: each mark, then its word.
pub fn presence_legend(ui: &mut Ui) {
    let t = Tokens::of(ui.ctx());
    ui.horizontal(|ui| {
        for m in Presence::ALL {
            ui.label(RichText::new(m.glyph()).font(t.mono(TextRole::Caption)).color(m.color(&t)));
            ui.label(RichText::new(m.word()).font(t.font(TextRole::Caption)).color(t.theme.text3));
        }
    });
}

#[cfg(test)]
mod tests;
