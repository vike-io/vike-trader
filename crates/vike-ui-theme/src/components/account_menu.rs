//! The rows of the Trade window's venue · account menu: ONE line each — the venue and its product in
//! the Strong role, the account's name or number after it (`Binance Perp  main`,
//! `Dukascopy  3709890`) — with the account's mode chip at the right and a check on the account the
//! window is on. **Both are regular weight, and size and ink tell them apart** (the owner chose this
//! one, "B", from five drawn on an HTML page, 2026-10-05, after calling the semibold venue bold): the
//! venue in the primary ink at the Strong size, the account in the dim ink at the Body size. A
//! NUMBER (`3709890`) is JetBrains Mono and a NAME (`main`) is Inter — the design system's rule,
//! "words are Inter, anything read character by character is mono". A venue
//! with no account shows the venue and a Connect button where the chip would be. The owner's v3
//! design drew two lines (the account and the symbol under the venue) and the owner then asked for
//! one (2026-10-04, "second line remove and put acc name or number after venue"): the symbol the row
//! opens is on hover instead, and a row that cannot be picked is greyed with its reason on hover.
//! The menu fetches no price: the design draws one per row, and the owner ruled it out ("you may not
//! fetch prices for menu").
//!
//! The kit knows no venues and no accounts: a row takes strings and a mark. What a row IS is the
//! Trade window's (`vike_panels::trade::AccountRow`); this file paints it.

use egui::{
    Align, CornerRadius, Layout, Rect, Response, Sense, Ui, UiBuilder, WidgetInfo, WidgetType,
    pos2, vec2,
};

use super::button::ActionButton;
use super::chip::{self, Mode};
use super::{Status, Tokens, focus_ring};
use crate::icons;
use crate::metrics::RADIUS;
use crate::type_scale::TextRole;

/// What an account row says about the account's mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccountMark<'a> {
    /// The mode, as its own chip (LIVE filled in the accent, DEMO and PAPER outlined).
    Mode(Mode),
    /// The backend did not report a mode: `word` in the muted outline, `why` on hover. Never drawn
    /// as PAPER (spec §4.3).
    Unknown { word: &'a str, why: &'a str },
}

/// One account in the menu.
#[derive(Clone, Copy, Debug)]
pub struct AccountMenuRow<'a> {
    /// The venue and its product: `Binance Perp`.
    pub title: &'a str,
    /// The account's name or number, drawn after the title: `main`.
    pub account: &'a str,
    /// The symbol the row opens (the venue's own spelling), empty where the venue does not list the
    /// instrument. On hover, and in the accessible name — never on the line.
    pub opens: &'a str,
    pub mark: AccountMark<'a>,
    /// The account the window is on: a check at the right, a Label rather than a Button (a selected
    /// item is a Label, spec §4.2), the hover fill kept.
    pub current: bool,
    /// Why the row cannot be picked (the server does not run the account, the venue does not list
    /// the instrument): drawn greyed, the reason on hover, and it takes no click.
    pub why_not: Option<&'a str>,
}

/// The width of the check's slot, kept on every row so the chips line up whether or not a row is
/// the current one.
fn check_slot(t: &Tokens) -> f32 {
    t.text.px(TextRole::Body) + t.metrics.gap
}

/// A row's height: its one line and a gap above and below.
pub fn row_height(ui: &Ui, t: &Tokens) -> f32 {
    let line =
        ui.painter().layout_no_wrap("Ag".to_string(), t.font(TextRole::Strong), t.theme.text);
    line.size().y + 2.0 * t.metrics.gap
}

/// Draw an account row across the available width; the response is the click on it (none for a
/// current or greyed row).
pub fn account_row(
    ui: &mut Ui,
    id: impl std::hash::Hash + std::fmt::Debug,
    row: &AccountMenuRow<'_>,
) -> Response {
    let t = Tokens::of(ui.ctx());
    let (rect, _) =
        ui.allocate_exact_size(vec2(ui.available_width(), row_height(ui, &t)), Sense::hover());
    let pickable = !row.current && row.why_not.is_none();
    let sense = if pickable { Sense::click() } else { Sense::hover() };
    let resp = ui.interact(rect, ui.id().with(&id), sense);
    if row.current || (pickable && resp.hovered()) {
        ui.painter().rect_filled(rect, CornerRadius::same(RADIUS), t.theme.hover);
    }
    let ink = if row.why_not.is_some() { t.theme.text3 } else { t.theme.text };
    paint_line(ui, &t, rect, row.title, row.account, ink);
    // The chip, right-aligned before the check's slot.
    let right = rect.right() - t.metrics.pad - check_slot(&t);
    let chip_area = Rect::from_min_max(pos2(rect.left(), rect.top()), pos2(right, rect.bottom()));
    let mut at = ui.new_child(
        UiBuilder::new()
            .id_salt((&id, "mark"))
            .max_rect(chip_area)
            .layout(Layout::right_to_left(Align::Center)),
    );
    match row.mark {
        AccountMark::Mode(m) => {
            chip::mode(&mut at, m);
        }
        AccountMark::Unknown { word, why } => {
            chip::badge(&mut at, word, Status::Muted).on_hover_text(why);
        }
    }
    if row.current {
        let glyph = egui::FontId::new(t.text.px(TextRole::Body), icons::family());
        icons::CHECK.paint(
            ui.painter(),
            pos2(rect.right() - t.metrics.pad, rect.center().y),
            egui::Align2::RIGHT_CENTER,
            glyph,
            t.theme.accent,
        );
    }
    focus_ring(ui, &t, &resp);
    let mode_word = match row.mark {
        AccountMark::Mode(m) => m.label(),
        AccountMark::Unknown { word, .. } => word,
    };
    // The accessible name keeps what the line no longer says: the symbol the row opens, or that the
    // venue does not list it.
    let opens = if row.opens.is_empty() { "not listed here" } else { row.opens };
    let name = format!("{}, {} · {opens}, {mode_word}", row.title, row.account);
    let kind = if pickable { WidgetType::Button } else { WidgetType::Label };
    resp.widget_info(|| WidgetInfo::selected(kind, true, row.current, &name));
    match (row.why_not, row.opens) {
        (Some(why), _) => resp.on_hover_text(why),
        (None, opens) if !opens.is_empty() => resp.on_hover_text(format!("Opens {opens}")),
        (None, _) => resp,
    }
}

/// The venue with no account: its name and a Connect button at the right. The response is the
/// button's. Its accessible name carries the venue, so a screen reader does not hear the same word
/// once per row.
pub fn connect_row(
    ui: &mut Ui,
    id: impl std::hash::Hash + std::fmt::Debug,
    title: &str,
    button_name: &str,
) -> Response {
    let t = Tokens::of(ui.ctx());
    let (rect, _) =
        ui.allocate_exact_size(vec2(ui.available_width(), row_height(ui, &t)), Sense::hover());
    paint_line(ui, &t, rect, title, "", t.theme.text3);
    let area = Rect::from_min_max(rect.min, pos2(rect.right() - t.metrics.pad, rect.bottom()));
    let mut at = ui.new_child(
        UiBuilder::new()
            .id_salt((&id, "connect"))
            .max_rect(area)
            .layout(Layout::right_to_left(Align::Center)),
    );
    let button = at.scope_builder(UiBuilder::new().id_salt((&id, "button")), |ui| {
        ui.add(ActionButton::secondary("Connect"))
    });
    icons::named(button.inner, button_name)
}

/// The share of a row the line may take, left to right: the rest is the mark's (a long account is
/// a cTrader number; clipped, never run under the chip).
const TEXT_SHARE: f32 = 0.7;

/// The title in the Strong role at regular weight and the account after it in the Body role, also
/// regular, in the dim ink (the owner: "we have to distinguish name of venue and acc"; the bundled
/// faces have no weight lighter than regular and he found semibold bold, so the difference is SIZE
/// and INK). An account that is a number is set in the mono face, a name in the words face. A greyed
/// row (`ink` is already the dim ink) reads as unavailable all the way across. Centred in the row
/// from its left pad.
fn paint_line(ui: &Ui, t: &Tokens, rect: Rect, title: &str, account: &str, ink: egui::Color32) {
    let clip =
        Rect::from_min_max(rect.min, pos2(rect.left() + rect.width() * TEXT_SHARE, rect.max.y));
    let p = ui.painter().with_clip_rect(clip);
    let centre = |x: f32| pos2(x, rect.center().y);
    let x = rect.left() + t.metrics.pad;
    let title = p.text(centre(x), egui::Align2::LEFT_CENTER, title, t.font(TextRole::Strong), ink);
    if !account.is_empty() {
        let number = account.chars().all(|c| c.is_ascii_digit());
        let face = if number { t.mono(TextRole::Body) } else { t.font(TextRole::Body) };
        p.text(
            centre(title.right() + t.metrics.gap),
            egui::Align2::LEFT_CENTER,
            account,
            face,
            t.theme.text3,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::appearance::Appearance;
    use crate::components::testing::{ctx_with, paint};
    use crate::type_scale::TextSize;
    use egui::{Color32, FontId, Shape};

    /// `(text, font, colour)` of every text one pass painted.
    fn fonts(shapes: &[Shape]) -> Vec<(String, FontId, Color32)> {
        shapes
            .iter()
            .filter_map(|s| match s {
                Shape::Text(t) => {
                    let f = &t.galley.job.sections[0].format;
                    Some((t.galley.text().to_string(), f.font_id.clone(), f.color))
                }
                _ => None,
            })
            .collect()
    }

    fn row<'a>(account: &'a str, why_not: Option<&'a str>) -> AccountMenuRow<'a> {
        AccountMenuRow {
            title: "Binance Spot",
            account,
            opens: "BTCUSDT",
            mark: AccountMark::Mode(Mode::Demo),
            current: false,
            why_not,
        }
    }

    /// THE choice the owner made (variant B): the venue is the Strong role at regular weight in the
    /// primary ink; a name is the Body role in the words face in the dim ink; a number is the Body
    /// role in the mono face in the dim ink. At every text size, because a literal 12 or 11 passes by
    /// coincidence at one of them.
    #[test]
    fn the_venue_is_regular_and_the_account_is_dim_and_smaller() {
        for size in TextSize::ALL {
            let ctx = ctx_with(&Appearance { text_size: size, ..Appearance::default() });
            let t = Tokens::of(&ctx);
            for (account, face) in
                [("main", t.font(TextRole::Body)), ("3709890", t.mono(TextRole::Body))]
            {
                let shapes = paint(&ctx, |ui| {
                    account_row(ui, "r", &row(account, None));
                });
                let painted = fonts(&shapes);
                let get = |text: &str| {
                    painted.iter().find(|(s, _, _)| s == text).unwrap_or_else(|| {
                        panic!("{size:?}: {text:?} was not painted: {painted:?}")
                    })
                };
                let (_, venue_font, venue_ink) = get("Binance Spot");
                assert_eq!(*venue_font, t.font(TextRole::Strong), "{size:?}: the venue");
                assert_ne!(*venue_font, t.font_semibold(TextRole::Strong), "{size:?}: not bold");
                assert_eq!(*venue_ink, t.theme.text, "{size:?}: the venue's ink");
                let (_, account_font, account_ink) = get(account);
                assert_eq!(*account_font, face, "{size:?}: {account:?}'s face");
                assert_eq!(*account_ink, t.theme.text3, "{size:?}: the account's ink");
            }
        }
    }

    /// A row that cannot be picked is dim all the way across: the venue as well as the account.
    #[test]
    fn a_greyed_row_is_dim_across() {
        let ctx = ctx_with(&Appearance::default());
        let t = Tokens::of(&ctx);
        let shapes = paint(&ctx, |ui| {
            account_row(ui, "r", &row("main", Some("not listed here")));
        });
        for (text, _, ink) in fonts(&shapes).into_iter().filter(|(s, _, _)| s != "DEMO") {
            assert_eq!(ink, t.theme.text3, "{text:?} on a greyed row");
        }
    }
}
