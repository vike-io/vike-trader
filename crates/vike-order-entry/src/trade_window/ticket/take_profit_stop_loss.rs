//! The ticket's TP/SL: the toggle, the refusal written under it, the exits' lines and the row of
//! the two legs (spec §3.6, §4.4).

use super::button::TradeWindowTicketButton;
use super::fit::{fits, two_digits};
use super::rows::caption_font;
use super::{
    NO_SIZE, TAKE_PROFIT_STOP_LOSS_ROOM_WHY, entry_price, legs, order_with,
    take_profit_stop_loss_block, take_profit_stop_loss_refusal,
};
use crate::trade_window::describe::why_untradable;
use crate::trade_window::{
    TradeWindowAction, TradeWindowInputs, TradeWindowState, TradeWindowTicketKind,
    TradeWindowTradable, ladder, sizing, text_w,
};
use egui::RichText;
use vike_ui_theme::components::{ThemeTokens, input};
use vike_ui_theme::icons;
use vike_ui_theme::side::ThemeSidePair;
use vike_ui_theme::value::trade;

/// The TP/SL toggle. `Some` with its response where TP/SL can be used. Where it cannot (I-3,
/// ruled): while the trader has it TICKED it stays ticked and enabled, so it can be turned off,
/// and the order is refused until it is ([`super::order`]; the caller writes the reason beside it); once
/// it is off it is disabled and says why on hover. In a window that takes no order at all
/// ([`why_untradable`]) it is an order control like any other: disabled, showing the trader's
/// setting, with the window's reason on hover (W3 review minor 7). Its name to a screen reader is
/// `TP/SL` whatever face it wears ([`TradeWindowTicketTakeProfitStopLossFace`]).
pub(super) fn take_profit_stop_loss_toggle(
    ui: &mut egui::Ui,
    t: &ThemeTokens,
    state: &mut TradeWindowState,
    inputs: &TradeWindowInputs<'_>,
    order_type: TradeWindowTicketKind,
    face: TradeWindowTicketTakeProfitStopLossFace,
) -> Option<egui::Response> {
    let b = |on: bool| {
        take_profit_stop_loss_button(t, on, face, &state.take_profit_text, &state.stop_loss_text)
    };
    if let Some(why) = why_untradable(inputs) {
        let _ = ui.add(b(state.take_profit_stop_loss).disabled_because(&why));
        return None;
    }
    match take_profit_stop_loss_block(state, inputs, order_type) {
        None => {
            let r = ui.add(b(state.take_profit_stop_loss));
            if r.clicked() {
                state.take_profit_stop_loss = !state.take_profit_stop_loss;
            }
            Some(r)
        }
        Some(why) if state.take_profit_stop_loss => {
            let r = ui.add(b(true));
            if r.clicked() {
                state.take_profit_stop_loss = false;
            }
            let _ = r.on_hover_text(take_profit_stop_loss_refusal(why));
            None
        }
        Some(why) => {
            let _ = ui.add(b(false).disabled_because(why));
            None
        }
    }
}

/// How the TP/SL toggle is worded and padded: the full ticket's `TP/SL`, or the compact ticket's
/// `TP +0.5% SL −0.3%` (the distances typed) where its row has the room, snug where it has not.
#[derive(Clone, Copy, Default)]
pub(super) struct TradeWindowTicketTakeProfitStopLossFace {
    pub(super) distances: bool,
    pub(super) snug: bool,
}

/// The TP/SL toggle, in state `on`, with the face `face` and the distances `tp` and `sl` as typed.
pub(super) fn take_profit_stop_loss_button<'a>(
    t: &ThemeTokens,
    on: bool,
    face: TradeWindowTicketTakeProfitStopLossFace,
    take_profit: &str,
    stop_loss: &str,
) -> TradeWindowTicketButton<'a> {
    let b = TradeWindowTicketButton::new("TP/SL");
    let b = if on { b.icon(t, icons::CHECK, Some(t.theme.accent)) } else { b };
    let b = if face.distances {
        b.words(t, "TP")
            .digits(t, &format!("+{}%", leg_words(take_profit)))
            .words(t, "SL")
            .digits(t, &format!("−{}%", leg_words(stop_loss)))
    } else {
        b.words(t, "TP/SL")
    };
    let b = b.toggled(on);
    if face.snug { b.snug() } else { b }
}

/// A TP or SL distance as the compact toggle's face writes it: what the trader typed, at most six
/// characters of it (a face must not outgrow its row; the whole thing is in the hover), a dash
/// where nothing is typed.
fn leg_words(text: &str) -> String {
    let t = text.trim();
    if t.is_empty() { NO_SIZE.to_string() } else { t.chars().take(6).collect() }
}

/// The reason TP/SL, ticked, refuses this ticket's orders, or `None` when it does not. A window
/// that takes no order at all says THAT instead ([`why_untradable`]): turning TP/SL off would send
/// nothing there either.
pub(super) fn take_profit_stop_loss_refused(
    state: &TradeWindowState,
    inputs: &TradeWindowInputs<'_>,
    order_type: TradeWindowTicketKind,
) -> Option<String> {
    let trades = matches!(inputs.tradable, TradeWindowTradable::Yes);
    take_profit_stop_loss_block(state, inputs, order_type)
        .filter(|_| state.take_profit_stop_loss && trades)
        .map(take_profit_stop_loss_refusal)
}

/// The exits TP/SL puts on an order, for a buy and for a sell (spec §3.6, "with the resulting
/// prices shown"): a PREVIEW, asked of [`super::order`]'s own arithmetic with TP/SL ON whatever the toggle
/// says, so the compact toggle's hover shows what turning it on would set and no line can show a
/// price the order would not carry. Where the order itself cannot be built (no size, a price off
/// the tick, no price at all) the line says why instead.
pub(super) fn take_profit_stop_loss_lines(
    state: &TradeWindowState,
    inputs: &TradeWindowInputs<'_>,
    order_type: TradeWindowTicketKind,
) -> [String; 2] {
    [1, -1].map(|side| {
        let verb = if side > 0 { "Buy" } else { "Sell" };
        match order_with(side, order_type, true, state, inputs) {
            Ok(TradeWindowAction::Place { exits: Some(e), .. }) => format!(
                "{verb}: TP {} · SL {}",
                ladder::fmt_price(e.take_profit, inputs.grid.tick_size),
                ladder::fmt_price(e.stop_loss, inputs.grid.tick_size)
            ),
            // `order_with` with TP/SL on answers exits or a refusal; this arm is never taken.
            Ok(_) => format!("{verb}: {TAKE_PROFIT_STOP_LOSS_ROOM_WHY}"),
            Err(why) => format!("{verb}: {why}"),
        }
    })
}

/// The exits a BUY would carry, as the design's caption beside the toggle writes them: `TP 65,759.6`
/// and `SL 65,236.1` — from the entry price and the distances typed, never from a size, so it reads
/// whatever the size field holds; a dash for an exit nothing can be set for.
fn exit_words(
    state: &TradeWindowState,
    inputs: &TradeWindowInputs<'_>,
    order_type: TradeWindowTicketKind,
) -> [String; 2] {
    let tick = inputs.grid.tick_size;
    let exits = entry_price(1, order_type, state, inputs).zip(legs(state)).and_then(
        |(entry, (take_profit, stop_loss))| sizing::exits(1, entry, take_profit, stop_loss, tick),
    );
    let price =
        |p: Option<f64>| p.map_or_else(|| NO_SIZE.to_string(), |p| ladder::fmt_price(p, tick));
    [
        format!("TP {}", price(exits.map(|e| e.take_profit))),
        format!("SL {}", price(exits.map(|e| e.stop_loss))),
    ]
}

/// The TP/SL toggle and, beside it, what a buy's exits would be at the distances typed
/// ([`exit_words`]): grey while TP/SL is off, in the market's colours while it is on and allowed.
/// Where the toggle's row has no room for them they are left out, never cut: the same exits are on
/// the toggle's hover ([`take_profit_stop_loss_lines`]), and written under the row while TP/SL is on.
pub(super) fn take_profit_stop_loss_row(
    ui: &mut egui::Ui,
    t: &ThemeTokens,
    key: egui::Id,
    state: &mut TradeWindowState,
    inputs: &TradeWindowInputs<'_>,
    order_type: TradeWindowTicketKind,
) -> bool {
    let toggle = take_profit_stop_loss_toggle(
        ui,
        t,
        state,
        inputs,
        order_type,
        TradeWindowTicketTakeProfitStopLossFace::default(),
    );
    let allowed = toggle.is_some() && state.take_profit_stop_loss;
    if let Some(r) = &toggle {
        let [buy, sell] = take_profit_stop_loss_lines(state, inputs, order_type);
        let _ = r.clone().on_hover_text(format!("{buy}\n{sell}"));
    }
    let words = exit_words(state, inputs, order_type);
    let font = caption_font(t);
    let gap = t.metrics.gap;
    let needed: f32 = words.iter().map(|w| text_w(ui.ctx(), w, &font, t)).sum::<f32>() + gap;
    if fits(ui, key.with("tpsl_exits_fit"), needed, ui.available_width(), two_digits(ui.ctx(), t)) {
        let (take_profit_ink, stop_loss_ink) = ThemeSidePair::TakeProfitStopLoss.texts(t);
        for (w, ink) in words.into_iter().zip([take_profit_ink, stop_loss_ink]) {
            let ink = if allowed { ink } else { t.theme.text_tertiary };
            ui.label(RichText::new(w).font(font.clone()).color(ink));
        }
    }
    allowed
}

/// The TP and SL distances, in percent, while TP/SL is on and allowed: the two fields the design's
/// fixed distances have no place for.
pub(super) fn take_profit_stop_loss_legs(
    ui: &mut egui::Ui,
    t: &ThemeTokens,
    state: &mut TradeWindowState,
) {
    ui.spacing_mut().item_spacing.x = t.metrics.gap;
    let each = ((ui.available_width() - t.metrics.gap) / 2.0).max(trade::MIN_FIELD_WIDTH);
    ui.spacing_mut().text_edit_width = each;
    input::number(
        ui,
        &mut state.take_profit_text,
        input::ThemeInputField { hint: "TP", unit: Some("% TP"), ..Default::default() },
    );
    input::number(
        ui,
        &mut state.stop_loss_text,
        input::ThemeInputField { hint: "SL", unit: Some("% SL"), ..Default::default() },
    );
}
