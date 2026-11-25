//! How the ticket's rows fit their width: a row of equal choices or of natural widths, packed and
//! balanced, taken in one row where it fits and in two where it does not, never a cut word.

use super::button::TradeWindowTicketButton;
use super::rows::caption_font;
use crate::trade_window::text_w;
use egui::text::{LayoutJob, TextFormat};
use egui::{Color32, FontId};
use vike_ui_theme::components::ThemeTokens;

/// How many of `widths`, a gap apart and in order, each row `width` wide holds, filling a row before
/// starting the next: `[2]` is one row of two, `[1, 1]` two rows of one. Measured BEFORE anything is
/// drawn, so a row of buttons wraps where it must and its height is known in advance. egui's
/// `horizontal_wrapped` cannot wrap a button that is told its own width, so the last one ran past
/// the ticket's edge (MEASURED by the fit sweep: the fifth quick size at Comfortable density), and
/// every row after it inherited the widened width.
pub(super) fn pack(widths: &[f32], gap: f32, width: f32) -> Vec<usize> {
    let mut rows = Vec::new();
    let (mut n, mut used) = (0, 0.0);
    for &w in widths {
        if n > 0 && used + gap + w > width {
            rows.push(n);
            (n, used) = (0, 0.0);
        }
        used = if n == 0 { w } else { used + gap + w };
        n += 1;
    }
    if n > 0 {
        rows.push(n);
    }
    rows
}

/// `rows` evened out: the same rows holding the same items, the last no lonelier than the first (five
/// quick sizes in two rows are three and two, not four and one). No row grows past what the packing
/// found would fit: they are at most `ceil(items / rows)`, and the first of those held more.
pub(super) fn balance(rows: Vec<usize>) -> Vec<usize> {
    let (k, n) = (rows.len(), rows.iter().sum::<usize>());
    if k < 2 {
        return rows;
    }
    (0..k).map(|i| n / k + usize::from(i < n % k)).collect()
}

/// How the items of a packed row share the row's width.
#[derive(Clone, Copy)]
pub(super) enum TradeWindowTicketRowFill {
    /// Every item as wide as the others, the whole row between them: the design's `grow` buttons
    /// (the order types, the quick sizes, the shares, Buy and Sell).
    Equal,
    /// Every item its own width but the row's last, which takes what is left (the toggles: Reduce
    /// only, Post only and a Leverage that grows).
    LastOfRow,
    /// Every item its own width but item `k`, counted over every row, which takes what is left of
    /// ITS row (the cancel row: Cancel all grows, and the padlock stays a padlock if it wraps).
    Item(usize),
}

/// The widths of one row's items, `naturals` being what each needs, `first` the index of the row's
/// first among all of them, `avail` the row's width and `gap` the space between items.
pub(super) fn row_widths(
    fill: TradeWindowTicketRowFill,
    first: usize,
    naturals: &[f32],
    gap: f32,
    avail: f32,
) -> Vec<f32> {
    let n = naturals.len();
    let gaps = gap * n.saturating_sub(1) as f32;
    let grows = match fill {
        TradeWindowTicketRowFill::Equal => {
            return vec![((avail - gaps) / n.max(1) as f32).max(0.0); n];
        }
        TradeWindowTicketRowFill::LastOfRow => n.checked_sub(1),
        TradeWindowTicketRowFill::Item(k) => k.checked_sub(first).filter(|i| *i < n),
    };
    let mut widths = naturals.to_vec();
    if let Some(i) = grows {
        let others: f32 =
            naturals.iter().enumerate().filter(|(j, _)| *j != i).map(|(_, w)| w).sum();
        widths[i] = (avail - gaps - others).max(naturals[i]);
    }
    widths
}

/// Draw items `0..` into the rows [`pack`] gave them, each row a horizontal line, each item given
/// its width by [`row_widths`].
pub(super) fn in_rows(
    ui: &mut egui::Ui,
    t: &ThemeTokens,
    rows: &[usize],
    naturals: &[f32],
    fill: TradeWindowTicketRowFill,
    mut item: impl FnMut(&mut egui::Ui, usize, f32),
) {
    let mut i = 0;
    for &n in rows {
        let widths = row_widths(fill, i, &naturals[i..i + n], t.metrics.gap, ui.available_width());
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = t.metrics.gap;
            for w in widths {
                item(ui, i, w);
                i += 1;
            }
        });
    }
}

/// A number on a button that grows to fill its row (a quick size, a share): mono, in a snug cell.
pub(super) fn digits_button<'a>(t: &ThemeTokens, words: &str) -> TradeWindowTicketButton<'a> {
    TradeWindowTicketButton::new(words).digits(t, words).snug()
}

/// Whether something `needed` wide fits a row `avail` wide, REMEMBERED under `key` from frame to
/// frame so a width that moves by a digit at the boundary does not make the row flicker between
/// its two shapes: it stops fitting the moment it does not, and fits again only with `margin` to
/// spare (`rows::send_rows` argues the same for Buy and Sell). A row never seen fits.
pub(super) fn fits(ui: &egui::Ui, key: egui::Id, needed: f32, avail: f32, margin: f32) -> bool {
    let was = ui.data(|d| d.get_temp::<bool>(key)).unwrap_or(true);
    let now = needed + if was { 0.0 } else { margin } <= avail;
    ui.data_mut(|d| d.insert_temp(key, now));
    now
}

/// Two digits of the mono Caption face: the margin a remembered fit ([`fits`]) keeps.
pub(super) fn two_digits(context: &egui::Context, t: &ThemeTokens) -> f32 {
    text_w(context, "00", &caption_font(t), t)
}

/// A run of the card's facts: `line` high (the design's line height of 1, the text's own size, so a
/// line of facts is as high as the text it holds), the run centred in it.
pub(super) fn push(job: &mut LayoutJob, text: &str, font: FontId, color: Color32, line: f32) {
    let format = TextFormat { font_id: font, color, line_height: Some(line), ..Default::default() };
    job.append(text, 0.0, format);
}
