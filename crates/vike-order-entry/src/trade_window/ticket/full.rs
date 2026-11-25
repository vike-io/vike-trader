//! The full ticket, beside the ladder or alone: the form of rows, its opening height and where it
//! scrolls, the order types, the toggles and the position card.

use super::button::{TradeWindowTicketButton, tall_h};
use super::fit::{
    TradeWindowTicketRowFill, balance, digits_button, fits, in_rows, pack, two_digits,
};
use super::rows::{
    TradeWindowTicketEntry, cancel_btns_of, cancel_layout, cancel_layout_of, cancel_row, caption,
    caption_font, caption_text, close_reverse, greyed_button, max_text, part, position_held,
    position_pl, position_side, post_only_off, price_row, quick_sizes, send_button_of,
    send_buttons, send_rows, share_words, shares, size_row, toggle_button, value_text,
};
use super::take_profit_stop_loss::{
    take_profit_stop_loss_legs, take_profit_stop_loss_lines, take_profit_stop_loss_refused,
    take_profit_stop_loss_row,
};
use super::{FORM_NAME, LEVERAGE_WHY, POST_TAKE_PROFIT_STOP_LOSS_WHY, stop_warning, typed_price};
use crate::trade_window::describe::why_untradable;
use crate::trade_window::{
    TradeWindowAction, TradeWindowInputs, TradeWindowState, TradeWindowTicketKind, label_h,
    name_group, section, text_w,
};
use egui::{Align, FontId, Layout, Rect, RichText, pos2};
use vike_ui_theme::components::{ThemeStatus, ThemeTokens};
use vike_ui_theme::metrics::{RADIUS, stroke};
use vike_ui_theme::type_scale::ThemeTextRole;

/// How the full ticket's Buy and Sell name a market order.
const MARKET: &str = "market";

/// The full ticket (beside the ladder, or alone): a form that scrolls, and under it, PINNED, Buy,
/// Sell and the cancel row. A pad of the density's all round, as the design's ticket has 8 px.
///
/// ⚠ Buy, Sell and the cancel row are never inside the scroll (W3 review hand-off 1): they are what
/// the ticket is for, and the last rows of a scrolling form are the first a loose look or a short
/// window pushes out of view. Their rows are counted BEFORE the form is drawn, from the labels as
/// this frame found them — so the form knows its height — and Buy and Sell stack, one to a row,
/// where both labels do not fit side by side (a label names the order and is never cut). An edit
/// in the form that changes how they stack is drawn the next frame, which it asks for.
///
/// Where the form is shorter than the room it is given, the pinned rows follow it, a gap under its
/// last row, as the design's ticket flows top to bottom with its spare room under the cancel row;
/// where it is taller, it scrolls and they stay at the bottom.
pub(crate) fn full(
    ui: &mut egui::Ui,
    t: &ThemeTokens,
    state: &mut TradeWindowState,
    inputs: &TradeWindowInputs<'_>,
    actions: &mut Vec<TradeWindowAction>,
) {
    let (gap, pad) = (t.metrics.gap, t.metrics.padding);
    let area = ui.available_rect_before_wrap().shrink2(egui::vec2(pad, pad));
    // Every order and cancel button is keyed under the ticket's own id ([`keyed`]).
    let key = ui.scope_id();
    ui.spacing_mut().item_spacing = egui::vec2(gap, gap);
    ui.spacing_mut().interact_size.y = t.metrics.control_height;
    let rows_now = |ui: &egui::Ui, state: &TradeWindowState| {
        let entry = TradeWindowTicketEntry { order_type: state.order_type, market_word: MARKET };
        let send = send_rows(ui, t, key, state, inputs, entry, area.width());
        let cancel = cancel_layout(ui, t, inputs, state.one_click, true, area.width());
        (send, cancel)
    };
    let (send, cancel) = rows_now(ui, state);
    let top = (area.max.y - pinned_h(t, send.len(), cancel.0.len())).max(area.min.y);
    let room = Rect::from_min_max(area.min, pos2(area.max.x, (top - gap).max(area.min.y)));
    let mut form_ui = part(ui, room, "trade_ticket_form");
    let scroll = egui::ScrollArea::vertical()
        .id_salt("trade_ticket_full")
        .auto_shrink([false, false])
        .show(&mut form_ui, |ui| form_rows(ui, t, key, state, inputs, actions));
    // The form takes the room its rows take, up to the room it has.
    let form = Rect::from_min_size(
        room.min,
        egui::vec2(room.width(), scroll.content_size.y.min(room.height())),
    );
    name_group(&form_ui, form, FORM_NAME);
    // A form that goes on below what it shows SAYS so (ruling B2 of the render check, 2026-10-03):
    // egui's floating scroll bar is invisible until hovered, and Buy and Sell name only side, size
    // and type, so a ticked Reduce only scrolled out of view showed nowhere. A divider across the
    // ticket, in the gap between the form and the rows pinned under it: painted, never laid out,
    // so it moves no widget and no id.
    if hidden_below(
        scroll.content_size.y,
        scroll.state.clamped_offset().y,
        scroll.inner_rect.height(),
    ) {
        let y = ui.painter().round_to_pixel_center(form.max.y + gap / 2.0);
        let across = egui::Rangef::new(area.min.x - pad, area.max.x + pad);
        ui.painter().hline(across, y, egui::Stroke::new(stroke::HAIRLINE, t.theme.border));
    }
    let (send_now, cancel_now) = rows_now(ui, state);
    if send_now != send || cancel_now != cancel {
        ui.ctx().request_repaint();
    }
    let pinned_rect = Rect::from_min_max(pos2(area.min.x, form.max.y + gap), area.max);
    let mut pinned = part(ui, pinned_rect, "trade_pinned");
    let entry = TradeWindowTicketEntry { order_type: state.order_type, market_word: MARKET };
    send_buttons(&mut pinned, t, key, state, inputs, actions, entry, &send);
    cancel_row(&mut pinned, t, key, state, inputs, actions, (&cancel.0, cancel.1), true);
}

/// How tall the rows pinned under the full ticket's form are: Buy and Sell, a tall row each where
/// they stack, then the cancel row, a row of controls, wrapped as it wraps.
fn pinned_h(t: &ThemeTokens, send_rows: usize, cancel_rows: usize) -> f32 {
    let (gap, control_h) = (t.metrics.gap, t.metrics.control_height);
    let rows = (send_rows + cancel_rows) as f32;
    send_rows as f32 * tall_h(t) + cancel_rows as f32 * control_h + (rows - 1.0).max(0.0) * gap
}

/// Whether a scroll area's content goes on below what it shows: content `content_h` tall, scrolled
/// `offset` down, in a view `visible_h` tall. Half a point of slack, egui's rounding, so a form
/// that fits exactly, or one scrolled to its end, says nothing.
pub(super) fn hidden_below(content_h: f32, offset: f32, visible_h: f32) -> bool {
    content_h - (offset + visible_h) > 0.5
}

/// Post only's words in the full ticket.
const POST_ONLY: &str = "Post only";

/// The quick sizes' words for the finest lot a window is sized for: the five multiples of a lot with
/// five decimals, `0.00001` to `0.00100`, the widest the row can be. [`opening_h`] packs them as
/// [`quick_sizes`] does, so the rows it counts are the rows the form draws in that look: two at the
/// smaller text sizes and three where Large's wider digits leave two to a row. (This was a constant
/// `2.0` until Large existed; at Large it left TP/SL 36 pt under the fold of a window opened to show
/// the whole form.) A coarser lot may need fewer rows, and a window opens before it has an
/// instrument to measure; a finer lot makes the form scroll, and the divider under it says so.
const FINEST_QUICK: [&str; 5] = ["0.00001", "0.00005", "0.00010", "0.00050", "0.00100"];

/// How tall the full ticket is, `width` wide, with its WHOLE form in view, in the state a window
/// opens in (ruling B1 of the render check, 2026-10-03): the pad, the form, section by section in
/// [`form_rows`]' order and a gap apart, then the gap, the rows pinned under it and the pad, as
/// [`full`] lays them out. The state is a fresh ticket on an account that trades: a Limit (so a
/// price row), nothing ticked, a position held (the TALLER of the shapes its card takes: the facts
/// and the P/L on two lines) and the buying power known (the max line, with the value and the max
/// on two lines of their own, the shape a long number takes). A control row is the kit's
/// `control_height`, a line of text the label its font draws ([`label_h`]), and a row of buttons wraps
/// by [`pack`] over [`TradeWindowTicketButton::natural_w`], as the form wraps it: the form's own sizes, read where it
/// reads them. `layout::window_size` puts the instrument bar, the strip and the window around it.
///
/// That the form is counted at its tallest costs nothing where it is shorter: the pinned rows
/// follow the form, and the spare room is under them.
pub(crate) fn opening_h(context: &egui::Context, t: &ThemeTokens, width: f32) -> f32 {
    let (gap, control_h, pad) = (t.metrics.gap, t.metrics.control_height, t.metrics.padding);
    let inner = width - 2.0 * pad;
    let line = |font: FontId| label_h(context, &font, t);
    let rows = |n: f32| n * control_h + (n - 1.0).max(0.0) * gap;
    let wrapped = |naturals: &[f32]| rows(pack(naturals, gap, inner).len() as f32);
    let shares = share_words();
    let shares = shares.each_ref().map(|w| digits_button(t, w).natural_w(context, t));
    let quick = FINEST_QUICK.map(|w| digits_button(t, w).natural_w(context, t));
    let toggles = [
        toggle_button(t, false, "Reduce only", false).natural_w(context, t),
        toggle_button(t, false, POST_ONLY, false).natural_w(context, t),
        greyed_button(t, "Leverage", LEVERAGE_WHY).natural_w(context, t),
    ];
    // [`position_card`]: a border and a pad round the facts (the side's word and the size at the
    // price) and, under them, the P/L, then Close and Reverse.
    let facts = t.text.px(ThemeTextRole::Body);
    let card = 2.0 + 2.0 * pad + facts + gap + facts + gap + control_h;
    let sections = [
        card,
        control_h,                                       // the order type
        control_h,                                       // the price
        control_h + 2.0 * (gap + line(caption_font(t))), // the size, its value and its max
        wrapped(&quick[..]),
        wrapped(&shares[..]),
        wrapped(&toggles[..]),
        control_h, // TP/SL
    ];
    let form = sections.iter().sum::<f32>() + (sections.len() - 1) as f32 * gap;
    // Buy and Sell side by side, or a row each where the widest label the window is sized for does
    // not fit beside its twin: the finest lot's middle quick size (`0.00010`, as [`FINEST_QUICK`] is
    // sized for), in the longest order type a window opens in.
    let widest =
        |side| send_button_of(t, side, (if side > 0 { "Buy" } else { "Sell" }, "0.00010", "limit"));
    let both = widest(1).natural_w(context, t) + gap + widest(-1).natural_w(context, t);
    let send_rows = if both > inner { 2 } else { 1 };
    // The cancel row at its widest, a three-digit count in Cancel all's pill (the sweep's `123
    // orders`): one row where it fits, two where Large's wider words leave no room. It was counted
    // as one row always, and a window opened for Comfortable density at Large text hid TP/SL 36 pt
    // below the fold.
    let cancel = cancel_layout_of(t, inner, |snug| {
        cancel_btns_of(t, "Cancel all 123", Some(123), true, snug)
            .iter()
            .map(|b| b.natural_w(context, t))
            .collect()
    });
    pad + form + gap + pinned_h(t, send_rows, cancel.0.len()) + pad
}

/// The full ticket's form, top to bottom: the position card (with Close and Reverse), why the
/// window takes no order (when it does not), the order type, the price (or what a market order
/// fills at), a wrong-side Stop's warning (when there is one), the size with its value, the quick
/// sizes, the shares of the buying power, the toggles (Reduce only, Post only, Leverage), and TP/SL
/// with the exits it sets or why it cannot. Each in its own [`section`].
fn form_rows(
    ui: &mut egui::Ui,
    t: &ThemeTokens,
    key: egui::Id,
    state: &mut TradeWindowState,
    inputs: &TradeWindowInputs<'_>,
    actions: &mut Vec<TradeWindowAction>,
) {
    let gap = t.metrics.gap;
    ui.spacing_mut().item_spacing = egui::vec2(gap, gap);
    section(ui, key, "form_position", |ui| position_card(ui, t, key, state, inputs, actions));
    if let Some(why) = why_untradable(inputs) {
        section(ui, key, "form_untradable", |ui| {
            ui.label(
                RichText::new(why).font(t.font(ThemeTextRole::Body)).color(t.theme.text_secondary),
            );
        });
    }
    section(ui, key, "form_type", |ui| order_types(ui, t, state));
    let order_type = state.order_type;
    if order_type == TradeWindowTicketKind::Market {
        section(ui, key, "form_price", |ui| {
            // What a market order fills at, where the price field would be: the design's note.
            ui.label(
                RichText::new(MARKET_NOTE)
                    .font(t.font(ThemeTextRole::Caption))
                    .color(t.theme.text_tertiary),
            );
        });
    } else {
        section(ui, key, "form_price", |ui| {
            if ui.horizontal(|ui| price_row(ui, t, state, inputs)).inner {
                not_a_number(ui, t);
            }
        });
    }
    // A Stop whose trigger would fire at once says so before the click, a line for each side it
    // would (both, for a trigger inside the spread), in the warning colour: written, not hovered.
    let warnings: Vec<String> = match (order_type, typed_price(state)) {
        (TradeWindowTicketKind::Stop, Some(trigger)) => {
            [1, -1].into_iter().filter_map(|side| stop_warning(side, trigger, inputs)).collect()
        }
        _ => Vec::new(),
    };
    if !warnings.is_empty() {
        section(ui, key, "form_stop_warning", |ui| {
            for words in warnings {
                let words = RichText::new(words).font(t.font(ThemeTextRole::Body));
                ui.label(words.color(ThemeStatus::Warning.color()));
            }
        });
    }
    section(ui, key, "form_size", |ui| {
        if ui.horizontal(|ui| size_row(ui, t, state, inputs, order_type, None)).inner {
            not_a_number(ui, t);
        }
        sub_lines(ui, t, key, state, inputs, order_type);
    });
    section(ui, key, "form_quick", |ui| quick_sizes(ui, t, state, inputs, order_type, false));
    section(ui, key, "form_shares", |ui| shares(ui, t, state, inputs, order_type));
    section(ui, key, "form_toggles", |ui| {
        toggles(ui, t, state, inputs, order_type, POST_ONLY, "Reduce only");
    });
    let mut allowed = false;
    section(ui, key, "form_tpsl", |ui| {
        allowed = ui
            .horizontal(|ui| take_profit_stop_loss_row(ui, t, key, state, inputs, order_type))
            .inner;
    });
    if allowed {
        section(ui, key, "form_tpsl_legs", |ui| {
            ui.horizontal(|ui| take_profit_stop_loss_legs(ui, t, state));
        });
    }
    if let Some(refusal) = take_profit_stop_loss_refused(state, inputs, order_type) {
        section(ui, key, "form_tpsl_refused", |ui| {
            // Written, not only hovered: the trader must see why Buy and Sell send nothing.
            ui.label(
                RichText::new(refusal)
                    .font(t.font(ThemeTextRole::Body))
                    .color(ThemeStatus::Warning.color()),
            );
        });
    } else if state.take_profit_stop_loss && state.post_only && why_untradable(inputs).is_none() {
        // Post only and TP/SL together: the one reason, written once where the trader looks, in
        // place of exit previews that would each repeat it (the strip and Buy and Sell say it too).
        section(ui, key, "form_post_tpsl", |ui| {
            ui.label(
                RichText::new(POST_TAKE_PROFIT_STOP_LOSS_WHY)
                    .font(t.font(ThemeTextRole::Body))
                    .color(ThemeStatus::Warning.color()),
            );
        });
    } else if state.take_profit_stop_loss && why_untradable(inputs).is_none() {
        section(ui, key, "form_tpsl_exits", |ui| {
            for line in take_profit_stop_loss_lines(state, inputs, order_type) {
                caption(ui, t, line);
            }
        });
    }
}

/// What the ticket writes where a market order has no price to type (the design's words).
const MARKET_NOTE: &str = "Fills now at the best ask for a buy, the best bid for a sell.";

/// Under a number field that holds no number: the words the border's red stands for.
fn not_a_number(ui: &mut egui::Ui, t: &ThemeTokens) {
    ui.label(
        RichText::new("Not a number")
            .font(t.font(ThemeTextRole::Caption))
            .color(ThemeStatus::Error.color()),
    );
}

/// The three order types, equal buttons across the ticket, the chosen one outlined in the accent.
fn order_types(ui: &mut egui::Ui, t: &ThemeTokens, state: &mut TradeWindowState) {
    let types = [
        (TradeWindowTicketKind::Limit, "Limit", "A limit order at your price"),
        (TradeWindowTicketKind::Market, "Market", "Fills now at the best price"),
        (TradeWindowTicketKind::Stop, "Stop", "A market order when the trigger trades"),
    ];
    let naturals: Vec<f32> = types
        .iter()
        .map(|(_, words, _)| TradeWindowTicketButton::text(t, words).snug().natural_w(ui.ctx(), t))
        .collect();
    let rows = balance(pack(&naturals, t.metrics.gap, ui.available_width()));
    in_rows(ui, t, &rows, &naturals, TradeWindowTicketRowFill::Equal, |ui, i, w| {
        let (value, words, why) = types[i];
        let on = state.order_type == value;
        if ui
            .add(TradeWindowTicketButton::text(t, words).snug().width(w).toggled(on))
            .on_hover_text(why)
            .clicked()
        {
            state.order_type = value;
        }
    });
}

/// Reduce only, Post only and Leverage, in as many rows as they take, the last of each growing.
///
/// Post only is a real toggle where [`post_only_off`] finds no reason against it, and a disabled
/// one saying that reason on hover where it does: the node too old to carry it, a venue Vike has
/// no post-only order for, an order type that is not Limit, or a window that takes no order. Its
/// name to a screen reader is its words whichever of the two it is, pressed or not.
fn toggles(
    ui: &mut egui::Ui,
    t: &ThemeTokens,
    state: &mut TradeWindowState,
    inputs: &TradeWindowInputs<'_>,
    order_type: TradeWindowTicketKind,
    post_word: &str,
    reduce_word: &str,
) {
    let context = ui.ctx().clone();
    let off = post_only_off(inputs, order_type);
    let naturals = [
        toggle_button(t, state.reduce_only, reduce_word, false).natural_w(&context, t),
        toggle_button(t, state.post_only, post_word, false).natural_w(&context, t),
        greyed_button(t, "Leverage", LEVERAGE_WHY).natural_w(&context, t),
    ];
    let rows = pack(&naturals, t.metrics.gap, ui.available_width());
    in_rows(ui, t, &rows, &naturals, TradeWindowTicketRowFill::LastOfRow, |ui, i, w| match i {
        0 => {
            let b = toggle_button(t, state.reduce_only, reduce_word, false).width(w);
            if ui.add(b).clicked() {
                state.reduce_only = !state.reduce_only;
            }
        }
        1 => {
            let b = toggle_button(t, state.post_only, post_word, false).width(w);
            match &off {
                Some(why) => {
                    ui.add(b.disabled_because(why));
                }
                None => {
                    if ui.add(b).clicked() {
                        state.post_only = !state.post_only;
                    }
                }
            }
        }
        _ => {
            ui.add(greyed_button(t, "Leverage", LEVERAGE_WHY).width(w));
        }
    });
}

/// The order's value and the most the buying power allows, one line at the right of the ticket
/// where both fit it (`≈ 654.32 USDT · max 0.999 BTC`), else a line each. A Market order's value
/// moves with every tick and gains a digit at 1,000, 10,000, …: whether they share a line is
/// REMEMBERED ([`fits`]) with two digits to spare, so a digit crossing the edge cannot move
/// everything under it by a line (the P/L's sibling, W4 fix round 2's N2). Whether the max is there
/// follows the buying power, which a tick never changes.
fn sub_lines(
    ui: &mut egui::Ui,
    t: &ThemeTokens,
    key: egui::Id,
    state: &TradeWindowState,
    inputs: &TradeWindowInputs<'_>,
    order_type: TradeWindowTicketKind,
) {
    let value = value_text(state, inputs, order_type);
    let max = max_text(state, inputs, order_type);
    let font = caption_font(t);
    let w = |s: &str| text_w(ui.ctx(), s, &font, t);
    let gap = t.metrics.gap;
    let joined = w(&value) + max.as_deref().map_or(0.0, |m| gap + w("·") + gap + w(m));
    let one_line =
        fits(ui, key.with("sub_one_line"), joined, ui.available_width(), two_digits(ui.ctx(), t));
    let line = |ui: &mut egui::Ui, text: String| {
        ui.add(egui::Label::new(caption_text(t, text)).truncate());
    };
    if one_line {
        ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
            if let Some(m) = max {
                line(ui, m);
                line(ui, "·".to_string());
            }
            line(ui, value);
        });
    } else {
        ui.with_layout(Layout::right_to_left(Align::Min), |ui| line(ui, value));
        if let Some(m) = max {
            ui.with_layout(Layout::right_to_left(Align::Min), |ui| line(ui, m));
        }
    }
}

/// The position card: a bordered, rounded box holding what the account holds — the side, the size
/// at the average price and the P/L — and Close and Reverse across its width. The facts share one
/// line with the P/L at the right where they fit with room for a P/L of four digits (REMEMBERED,
/// [`fits`]: a tick that adds a digit never moves the form under the pointer), else the P/L is a
/// line of its own, one row high whatever it says, cut only where it alone is wider than the form
/// (egui shows a cut label whole on hover). The facts WRAP where the form has the height for it
/// (W4 fix round 1: cut to one line, an eight-digit average price lost the P/L at every width of a
/// 260 pt ticket). Only a fill changes what the card says.
fn position_card(
    ui: &mut egui::Ui,
    t: &ThemeTokens,
    key: egui::Id,
    state: &mut TradeWindowState,
    inputs: &TradeWindowInputs<'_>,
    actions: &mut Vec<TradeWindowAction>,
) {
    let (gap, pad) = (t.metrics.gap, t.metrics.padding);
    egui::Frame::new()
        .fill(t.theme.surface)
        .stroke(egui::Stroke::new(stroke::HAIRLINE, t.theme.border))
        .corner_radius(egui::CornerRadius::same(RADIUS))
        .inner_margin(egui::Margin::same(pad as i8))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = gap;
            position_lines(ui, t, key, inputs);
            let each = (ui.available_width() - gap) / 2.0;
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = gap;
                close_reverse(ui, t, key, state, inputs, actions, false, Some(each));
            });
        });
}

/// One line of text, as high as its text and no higher: egui's `horizontal` is a row at least an
/// interact size high, which is a control's height, and the card's facts are not a control.
fn text_row<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    ui.scope(|ui| {
        ui.spacing_mut().interact_size.y = 0.0;
        ui.horizontal(add).inner
    })
    .inner
}

/// The card's facts ([`position_card`]).
fn position_lines(
    ui: &mut egui::Ui,
    t: &ThemeTokens,
    key: egui::Id,
    inputs: &TradeWindowInputs<'_>,
) {
    let gap = t.metrics.gap;
    let held = position_held(t, inputs);
    let Some((pl, col)) = position_pl(t, inputs) else {
        text_row(ui, |ui| {
            ui.label(held);
            ui.label(
                RichText::new("No position on this account")
                    .font(t.font(ThemeTextRole::Caption))
                    .color(t.theme.text_tertiary),
            );
        });
        return;
    };
    let font = t.mono(ThemeTextRole::Body);
    let pl = RichText::new(pl)
        .font(font.clone())
        .color(col)
        .line_height(Some(t.text.px(ThemeTextRole::Body)));
    // The P/L's room is a four-digit one at least (`+9999.99`), so a tick that adds a digit to it
    // never moves the card: it has the room it will need, or it is two lines whatever it says.
    let reserve = text_w(ui.ctx(), &format!("{:+.2} {}", 9999.99, inputs.quote), &font, t);
    let (word, word_col, facts) = position_side(t, inputs).expect("a P/L is a position's");
    let side = t.font_semibold(ThemeTextRole::Caption);
    let held_w = text_w(ui.ctx(), word, &side, t) + gap + text_w(ui.ctx(), &facts, &font, t);
    let pl_w = text_w(ui.ctx(), pl.text(), &font, t).max(reserve);
    let needed = held_w + pl_w;
    let one_line =
        fits(ui, key.with("card_one_line"), needed, ui.available_width(), two_digits(ui.ctx(), t));
    if one_line {
        // Two labels the row centres on each other, as the design's flex row does: one job would
        // sit the smaller word on the larger number's bottom edge, not its middle.
        let line = Some(t.text.px(ThemeTextRole::Body));
        text_row(ui, |ui| {
            let side = RichText::new(word).font(side).color(word_col);
            ui.add(egui::Label::new(side.line_height(line)).extend());
            let facts = RichText::new(facts).font(font.clone()).color(t.theme.text);
            ui.add(egui::Label::new(facts.line_height(line)).extend());
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add(egui::Label::new(pl).extend());
            });
        });
    } else {
        ui.add(egui::Label::new(held).wrap());
        ui.add(egui::Label::new(pl).truncate());
    }
}
