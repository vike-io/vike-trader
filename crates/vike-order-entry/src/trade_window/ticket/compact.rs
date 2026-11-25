//! The compact ticket under the ladder, for a narrow window: its fixed height, the position strip
//! and the toggles' one row.

use super::rows::{
    TradeWindowTicketEntry, cancel_layout, cancel_row, caption_font, caption_text, close_reverse,
    greyed_button, part, position_held, position_pl, quick_sizes, reason_line, send_buttons,
    send_rows, size_row, toggle_button, value_text,
};
use super::take_profit_stop_loss::{
    TradeWindowTicketTakeProfitStopLossFace, take_profit_stop_loss_button,
    take_profit_stop_loss_lines, take_profit_stop_loss_refused, take_profit_stop_loss_toggle,
};
use super::{
    LEVERAGE_WHY, POST_COMPACT_WHY, TAKE_PROFIT_STOP_LOSS_OFF, TAKE_PROFIT_STOP_LOSS_OFF_TO_SEND,
    post_only_block,
};
use crate::trade_window::describe::why_untradable;
use crate::trade_window::{
    TradeWindowAction, TradeWindowInputs, TradeWindowState, TradeWindowTicketKind, label_h, text_w,
};
use egui::text::LayoutJob;
use egui::{Align, Layout, Rect, RichText, UiBuilder, pos2};
use vike_ui_theme::components::{ThemeStatus, ThemeTokens};
use vike_ui_theme::icons;
use vike_ui_theme::metrics::{ThemeDensityMetrics, stroke};
use vike_ui_theme::type_scale::ThemeTextRole;

/// The height of the compact ticket's position strip: Close and Reverse, a pad above and below
/// them, and the rule under the strip.
fn strip_h(m: &ThemeDensityMetrics) -> f32 {
    m.control_height + 2.0 * m.padding + stroke::HAIRLINE
}

/// How tall the compact ticket (under the ladder) is at density `m`, with every row it can have:
/// the position strip, a pad, the size, the quick sizes, the toggles, Buy and Sell STACKED (a tall
/// row each: the tallest they take), the cancel row and a pad, a gap between the rows. A ticket
/// whose Buy and Sell sit side by side takes a tall row and a gap less, and leaves the difference
/// under the cancel row (the ladder above it never moves with the labels).
///
/// ⚠ `layout::under_ticket_h` is this, once it is: the compact ticket draws its tightest
/// arrangement ([`compact`]) into a region any shorter, which holds every control but drops the
/// quick sizes, so a window sized by the layout's older seven rows still shows a whole ticket.
pub(crate) fn compact_h(m: &ThemeDensityMetrics) -> f32 {
    let tall = m.control_height + m.padding;
    strip_h(m) + m.padding + 4.0 * m.control_height + 2.0 * tall + 5.0 * m.gap + m.padding
}

/// The compact ticket (under the ladder), top to bottom: the position strip — LONG or SHORT, the
/// size at the average price and the P/L on two lines at the left, Close and Reverse at the right —
/// then the size, the quick sizes, the toggles, Buy and Sell at market (a row each where both
/// labels do not fit side by side), and the cancel row. Limit and stop orders come from the
/// ladder (spec §3.6). The TP/SL distances are the full ticket's; the toggle's hover says what they
/// are and where they put the exits. (The design's Join bid and Join ask are not here: the owner
/// ruled them out, 2026-10-03 and again 2026-10-04.)
///
/// Its height is fixed before it is drawn (`layout::under_ticket_h`), and nothing in it wraps, so
/// where a row's content does not fit its width (a narrow window, a loose look), the row gives up
/// what the full ticket also offers — measured, never cut:
/// - the quick sizes show as many as fit;
/// - Post and Leverage, greyed placeholders, are left out where the toggles leave no room (and a
///   ticked TP/SL that refuses the order writes "Turn TP/SL off to send." in their place);
/// - the TP/SL toggle shows `TP/SL` where its distances would outgrow its row;
/// - `Cancel all N` drops its count where the row cannot hold it (the count is then on hover);
/// - "Turn TP/SL off to send." says "Turn TP/SL off." where the row cannot hold it, and a warning
///   mark where it cannot hold that;
/// - in a window that takes no order, the toggles' row says why instead ([`why_untradable`]):
///   there is no hint line under the ladder in this layout, so the reason is written here;
/// - where Buy and Sell stack and the region is not [`compact_h`] tall, the quick sizes go: every
///   other control stays, and so does the height of Buy and Sell.
pub(crate) fn compact(
    ui: &mut egui::Ui,
    t: &ThemeTokens,
    state: &mut TradeWindowState,
    inputs: &TradeWindowInputs<'_>,
    actions: &mut Vec<TradeWindowAction>,
) {
    let (gap, pad, control_h) = (t.metrics.gap, t.metrics.padding, t.metrics.control_height);
    let region = ui.available_rect_before_wrap();
    // Every order and cancel button is keyed under the ticket's own id ([`keyed`]).
    let key = ui.scope_id();
    ui.spacing_mut().item_spacing = egui::vec2(gap, gap);
    ui.spacing_mut().interact_size.y = control_h;
    let strip = Rect::from_min_size(region.min, egui::vec2(region.width(), strip_h(&t.metrics)));
    position_strip(ui, t, key, strip, state, inputs, actions);
    let body = Rect::from_min_max(
        pos2(region.min.x + pad, strip.max.y + pad),
        pos2(region.max.x - pad, region.max.y - pad),
    );
    let mut body_ui = part(ui, body, "trade_compact_body");
    let ui = &mut body_ui;
    ui.spacing_mut().item_spacing = egui::vec2(gap, gap);
    let tall = control_h + pad;
    // Whether Buy and Sell stack, and whether the rows they ask for fit the region: the rows above
    // them are three controls and their gaps, the cancel row follows.
    let entry =
        TradeWindowTicketEntry { order_type: TradeWindowTicketKind::Market, market_word: "MKT" };
    let send = send_rows(ui, t, key, state, inputs, entry, body.width());
    let wanted =
        |rows: usize| 3.0 * control_h + rows as f32 * tall + (rows as f32 + 3.0) * gap + control_h;
    let roomy = wanted(send.len()) <= body.height() + 0.5;
    ui.horizontal(|ui| {
        size_row(ui, t, state, inputs, TradeWindowTicketKind::Market, Some(3.5 * control_h));
        // The value, at the right, where the unit switch leaves room for it. Cut, with its whole
        // text on hover, only where it alone is wider than what is left.
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.add(
                egui::Label::new(caption_text(
                    t,
                    value_text(state, inputs, TradeWindowTicketKind::Market),
                ))
                .truncate(),
            );
        });
    });
    if roomy {
        quick_sizes(ui, t, state, inputs, TradeWindowTicketKind::Market, true);
    }
    ui.horizontal(|ui| match why_untradable(inputs) {
        Some(why) => {
            reason_line(ui, t, &why, t.theme.text_secondary);
        }
        None => compact_toggles(ui, t, state, inputs),
    });
    send_buttons(ui, t, key, state, inputs, actions, entry, &send);
    // One row, all four — none of them is something the full ticket alone offers — so where the
    // count does not fit (three digits of working orders, a narrow window), Cancel all drops it.
    let counted = cancel_layout(ui, t, inputs, state.one_click, true, body.width()).0.len() == 1;
    let (_, snug) = cancel_layout(ui, t, inputs, state.one_click, counted, body.width());
    cancel_row(ui, t, key, state, inputs, actions, (&[4], snug), counted);
}

/// The position strip: LONG or SHORT, the size at the average price and, under them, the P/L, at
/// the left, and Close and Reverse at the right, on the surface fill with a rule under it. The
/// facts are cut, with the whole text on hover, where they alone are wider than the strip leaves
/// them: Close and Reverse come first, from the right edge.
fn position_strip(
    ui: &mut egui::Ui,
    t: &ThemeTokens,
    key: egui::Id,
    strip: Rect,
    state: &mut TradeWindowState,
    inputs: &TradeWindowInputs<'_>,
    actions: &mut Vec<TradeWindowAction>,
) {
    let pad = t.metrics.padding;
    ui.painter().rect_filled(strip, 0.0, t.theme.surface);
    let y = strip.max.y - 0.5;
    ui.painter().hline(strip.x_range(), y, egui::Stroke::new(stroke::HAIRLINE, t.theme.border));
    let inner = Rect::from_min_max(
        pos2(strip.min.x + pad, strip.min.y),
        pos2(strip.max.x - pad, strip.max.y - stroke::HAIRLINE),
    );
    let mut s = ui.new_child(
        UiBuilder::new()
            .id_salt("trade_position_strip")
            .max_rect(inner)
            .layout(Layout::right_to_left(Align::Center)),
    );
    close_reverse(&mut s, t, key, state, inputs, actions, true, None);
    s.with_layout(Layout::top_down(Align::Min), |ui| {
        let held = position_held(t, inputs);
        let facts_h = job_h(ui.ctx(), held.clone());
        let pl = position_pl(t, inputs);
        let line = t.metrics.gap / 2.0;
        let block =
            facts_h + pl.as_ref().map_or(0.0, |_| line + label_h(ui.ctx(), &caption_font(t), t));
        ui.add_space(((inner.height() - block) / 2.0).max(0.0));
        ui.spacing_mut().item_spacing.y = line;
        match pl {
            Some((pl, col)) => {
                ui.add(egui::Label::new(held).truncate());
                // The P/L is the line a trader reads, so where `-123.45 USDT` outgrows the room
                // Close and Reverse leave it, the unit goes (the hover still says it) before a digit
                // is cut: Comfortable density at Large text, in a window 260 to 270 pt wide.
                let font = caption_font(t);
                let full = pl.to_string();
                let (shown, unit_dropped) = match full.rsplit_once(' ') {
                    Some((number, _))
                        if text_w(ui.ctx(), &full, &font, t) > ui.available_width() =>
                    {
                        (number.to_string(), true)
                    }
                    _ => (full.clone(), false),
                };
                let line =
                    ui.add(egui::Label::new(RichText::new(shown).font(font).color(col)).truncate());
                if unit_dropped {
                    line.on_hover_text(full);
                }
            }
            None => {
                ui.add(egui::Label::new(held).truncate());
                ui.add(egui::Label::new(caption_text(t, "No position on this account")).truncate());
            }
        }
    });
    ui.allocate_rect(strip, egui::Sense::hover());
}

/// How tall `job` is laid out, on one line.
fn job_h(context: &egui::Context, job: LayoutJob) -> f32 {
    context.fonts_mut(|f| f.layout_job(job)).size().y
}

/// The compact ticket's toggles: TP/SL (its hover previews the exits), Reduce, then Post and
/// Leverage where they fit — or, while a ticked TP/SL refuses the order, "Turn TP/SL off to send."
/// with the whole reason on hover.
///
/// The row is ONE line, so each part gives up what it can until the row holds: the TP/SL toggle
/// writes its distances (the design's face) where they leave Reduce its room, else just `TP/SL`;
/// the buttons give up half their padding; and the words that say the order is refused are the
/// short ones. A ticked toggle draws a check, which is counted: a row measured without it would
/// outgrow its width, and everything under it with it.
fn compact_toggles(
    ui: &mut egui::Ui,
    t: &ThemeTokens,
    state: &mut TradeWindowState,
    inputs: &TradeWindowInputs<'_>,
) {
    let gap = t.metrics.gap;
    let context = ui.ctx().clone();
    let avail = ui.available_width();
    let refusal = take_profit_stop_loss_refused(state, inputs, TradeWindowTicketKind::Market);
    let font = t.font(ThemeTextRole::Caption);
    // The three ways to say it, widest first: the long words, the short words, a warning mark.
    let said = [
        text_w(&context, TAKE_PROFIT_STOP_LOSS_OFF_TO_SEND, &font, t),
        text_w(&context, TAKE_PROFIT_STOP_LOSS_OFF, &font, t),
        font.size,
    ];
    // What the row asks for in each shape, the widest first; the last is what it falls back to.
    // A refused order's row has no distances: its words need the room.
    let shapes: Vec<(TradeWindowTicketTakeProfitStopLossFace, usize)> = if refusal.is_some() {
        vec![
            (TradeWindowTicketTakeProfitStopLossFace { distances: false, snug: false }, 0),
            (TradeWindowTicketTakeProfitStopLossFace { distances: false, snug: true }, 0),
            (TradeWindowTicketTakeProfitStopLossFace { distances: false, snug: true }, 1),
            (TradeWindowTicketTakeProfitStopLossFace { distances: false, snug: true }, 2),
        ]
    } else {
        vec![
            (TradeWindowTicketTakeProfitStopLossFace { distances: true, snug: false }, 0),
            (TradeWindowTicketTakeProfitStopLossFace { distances: false, snug: false }, 0),
            (TradeWindowTicketTakeProfitStopLossFace { distances: false, snug: true }, 0),
        ]
    };
    let width_of = |(face, words): &(TradeWindowTicketTakeProfitStopLossFace, usize)| {
        let take_profit = take_profit_stop_loss_button(
            t,
            state.take_profit_stop_loss,
            *face,
            &state.take_profit_text,
            &state.stop_loss_text,
        );
        let reduce = toggle_button(t, state.reduce_only, "Reduce", face.snug);
        let own = take_profit.natural_w(&context, t) + gap + reduce.natural_w(&context, t);
        own + if refusal.is_some() { gap + said[*words] } else { 0.0 }
    };
    let (face, words) = *shapes
        .iter()
        .find(|s| width_of(s) <= avail)
        .unwrap_or_else(|| shapes.last().expect("the row has shapes"));
    if let Some(r) =
        take_profit_stop_loss_toggle(ui, t, state, inputs, TradeWindowTicketKind::Market, face)
    {
        let [buy, sell] = take_profit_stop_loss_lines(state, inputs, TradeWindowTicketKind::Market);
        let _ = r.on_hover_text(format!(
            "TP {} % and SL {} % from the entry, set in the full ticket.\n{buy}\n{sell}",
            state.take_profit_text.trim(),
            state.stop_loss_text.trim()
        ));
    }
    if ui.add(toggle_button(t, state.reduce_only, "Reduce", face.snug)).clicked() {
        state.reduce_only = !state.reduce_only;
    }
    match refusal {
        Some(refusal) => {
            // The words that fit the row, never cut (a 260 pt window cut the long ones at
            // Comfortable density and Large text): the whole reason is the hover.
            let colour = ThemeStatus::Warning.color();
            let said = match words {
                2 => icons::WARNING.rich().size(font.size).color(colour),
                n => {
                    RichText::new([TAKE_PROFIT_STOP_LOSS_OFF_TO_SEND, TAKE_PROFIT_STOP_LOSS_OFF][n])
                        .font(font.clone())
                        .color(colour)
                }
            };
            let _ = ui.add(egui::Label::new(said).truncate()).on_hover_text(refusal);
        }
        None => {
            // The greyed placeholders, as many as the row has room for: Post, then a Leverage that
            // takes what is left. This ticket sends market orders only, so its Post is never live:
            // greyed with the venue's or the node's reason where one of them is the cause (asked of a
            // Limit entry, so only they can answer), else with this ticket's own.
            let why = post_only_block(inputs, TradeWindowTicketKind::Limit)
                .unwrap_or_else(|| POST_COMPACT_WHY.to_string());
            let post = greyed_button(t, "Post", &why);
            if post.natural_w(&context, t) <= ui.available_width() {
                ui.add(post);
                let leverage = greyed_button(t, "Leverage", LEVERAGE_WHY);
                if leverage.natural_w(&context, t) <= ui.available_width() {
                    let w = ui.available_width();
                    ui.add(leverage.width(w));
                }
            }
        }
    }
}
