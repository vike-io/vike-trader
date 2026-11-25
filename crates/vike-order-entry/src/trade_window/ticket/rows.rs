//! The ticket's rows: the position's Close and Reverse, the Cancel row, the size with its padlock,
//! unit and quick sizes, the shares, the toggles, the price, Buy and Sell, and the captions every
//! row is labelled with.

use super::button::{TradeWindowTicketButton, TradeWindowTicketButtonLook, number_field};
use super::fit::{TradeWindowTicketRowFill, balance, digits_button, in_rows, pack, push};
use super::{
    NO_ASKS_WHY, NO_BIDS_WHY, NO_BUYING_POWER_WHY, NO_CONVERT_WHY, NO_MARKET_PRICE_WHY,
    NO_ORDERS_WHY, NO_PRICE_WHY, NO_SIDE_WHY, NO_SIZE, ONE_CLICK_OFF_TIP, ONE_CLICK_ON_TIP, best,
    known, market_ref, no_lot_reason, order_as, post_only_block, price_text, reference_price,
    side_words, write_size,
};
use crate::trade_window::describe::why_untradable;
use crate::trade_window::{
    TradeWindowAction, TradeWindowInputs, TradeWindowSizingUnit, TradeWindowState,
    TradeWindowStatusLineKind, TradeWindowTicketKind, keyed, ladder, sizing, text_w,
};
use egui::text::LayoutJob;
use egui::{Align, Color32, FontId, Layout, Rect, RichText, UiBuilder};
use vike_ui_theme::components::ThemeTokens;
use vike_ui_theme::fmt::format_thousands_precision;
use vike_ui_theme::icons;
use vike_ui_theme::side::ThemeSidePair;
use vike_ui_theme::type_scale::ThemeTextRole;
use vike_ui_theme::value::trade;

/// What the account holds: LONG or SHORT in its side's colour and the size at the average price;
/// FLAT when there is none. A fill changes it; a tick of the market never does.
pub(super) fn position_held(t: &ThemeTokens, inputs: &TradeWindowInputs<'_>) -> LayoutJob {
    let mut job = LayoutJob::default();
    let side = t.font_semibold(ThemeTextRole::Caption);
    let line = t.text.px(ThemeTextRole::Body);
    match position_side(t, inputs) {
        None => push(&mut job, "FLAT", side, t.theme.text_tertiary, line),
        Some((word, col, facts)) => {
            push(&mut job, word, side, col, line);
            push(&mut job, &format!(" {facts}"), t.mono(ThemeTextRole::Body), t.theme.text, line);
        }
    }
    job
}

/// The held position as its two runs: the side's word in the side's colour, and the size at the
/// average price (`0.050 @ 65,410.2`); `None` while flat.
pub(super) fn position_side(
    t: &ThemeTokens,
    inputs: &TradeWindowInputs<'_>,
) -> Option<(&'static str, Color32, String)> {
    let p = inputs.position.filter(|p| p.size != 0.0)?;
    let long = p.size > 0.0;
    let (word, col) = (if long { "LONG" } else { "SHORT" }, ThemeSidePair::LongShort.text(long, t));
    let facts = format!(
        "{} @ {}",
        sizing::quantity_text(p.size.abs(), inputs.grid.lot_size),
        ladder::fmt_price(p.average_price, inputs.grid.tick_size)
    );
    Some((word, col, facts))
}

/// The position's unrealized P/L and its colour, or `None` while flat. It moves with every tick of
/// the mark, and gains or loses a digit at ±10, ±100, ±1,000.
pub(super) fn position_pl(
    t: &ThemeTokens,
    inputs: &TradeWindowInputs<'_>,
) -> Option<(String, Color32)> {
    let p = inputs.position.filter(|p| p.size != 0.0)?;
    let col = ThemeSidePair::GainLoss.text(p.unrealized_pnl >= 0.0, t);
    Some((format!("{:+.2} {}", p.unrealized_pnl, inputs.quote), col))
}

fn is_flat(inputs: &TradeWindowInputs<'_>) -> bool {
    inputs.position.is_none_or(|p| p.size == 0.0)
}

/// Close and Reverse, `reversed` for a right-to-left row (so they still read Close, Reverse); each
/// `each` wide where the row fills its width, else as wide as its word.
pub(super) fn close_reverse(
    ui: &mut egui::Ui,
    t: &ThemeTokens,
    key: egui::Id,
    state: &mut TradeWindowState,
    inputs: &TradeWindowInputs<'_>,
    actions: &mut Vec<TradeWindowAction>,
    reversed: bool,
    each: Option<f32>,
) {
    let mut buttons = [
        ("Close", "No position to close.", TradeWindowAction::ClosePosition, "close"),
        ("Reverse", "No position to reverse.", TradeWindowAction::Reverse, "reverse"),
    ];
    if reversed {
        buttons.reverse();
    }
    // ⚠ A Reverse's second half OPENS a position the size of the one held, at market: with no
    // price on the side it trades and no last price it goes out blind, and the app's notional cap
    // holds no request without a price. It is refused as a market Buy or Sell is (the owner's
    // decision of 2026-10-03, round 2, item 10, applied by the FW1 review). A Close only takes
    // away what is held, so it stays.
    let reverse_side = vike_model::closing_side(inputs.position.map_or(0.0, |p| p.size));
    let reverse_blind = market_ref(reverse_side, inputs).is_none();
    for (words, flat_why, act, role) in buttons {
        let why = match why_untradable(inputs) {
            Some(w) => Some(w),
            None if is_flat(inputs) => Some(flat_why.to_string()),
            None if matches!(act, TradeWindowAction::Reverse) && reverse_blind => {
                Some(NO_MARKET_PRICE_WHY.to_string())
            }
            None => None,
        };
        let b = TradeWindowTicketButton::text(t, words);
        let b = match each {
            Some(w) => b.snug().width(w),
            None => b,
        };
        let b = match &why {
            Some(w) => b.disabled_because(w),
            None => b,
        };
        if keyed(ui, key.with(role), b).clicked() {
            // Held, it names the size it sends and goes if the position changes before the
            // confirm (final review A, minor 3).
            let size = inputs.position.map_or(0.0, |p| p.size);
            state.submit_exit(act, size, actions);
        }
    }
}

/// The cancel row's three buttons' NAMES: `Cancel all N`, or `Cancel all` with `counted` false (on
/// a node that cannot attribute an order, Ruling R9, and where the count does not fit the compact
/// ticket's one row), `Bids`, `Asks`.
fn cancel_words(inputs: &TradeWindowInputs<'_>, counted: bool) -> [String; 3] {
    let all = match cancel_count(inputs, counted) {
        Some(n) => format!("Cancel all {n}"),
        None => "Cancel all".to_string(),
    };
    [all, "Bids".to_string(), "Asks".to_string()]
}

/// The count Cancel all carries in its pill: the working orders, where the row claims one.
fn cancel_count(inputs: &TradeWindowInputs<'_>, counted: bool) -> Option<usize> {
    (inputs.orders_why.is_none() && counted).then_some(inputs.orders.len())
}

/// The cancel row's four buttons as they are drawn — Cancel all, Bids, Asks, then the one-click
/// padlock — before any is given its reason or its width, so the row can measure them first.
fn cancel_btns<'a>(
    t: &ThemeTokens,
    inputs: &TradeWindowInputs<'_>,
    counted: bool,
    one_click: bool,
    snug: bool,
) -> [TradeWindowTicketButton<'a>; 4] {
    let words = cancel_words(inputs, counted);
    cancel_btns_of(t, &words[0], cancel_count(inputs, counted), one_click, snug)
}

/// [`cancel_btns`] from the two things it reads: the name of Cancel all and the count in its pill.
pub(super) fn cancel_btns_of<'a>(
    t: &ThemeTokens,
    name: &str,
    count: Option<usize>,
    one_click: bool,
    snug: bool,
) -> [TradeWindowTicketButton<'a>; 4] {
    let all = TradeWindowTicketButton::new(name).words(t, "Cancel all").snug();
    let all = match count {
        Some(n) => all.badge(n.to_string()),
        None => all,
    };
    // Bids, Asks and the padlock keep their own padding, and give up half of it where the row would
    // otherwise wrap (a loose look, a three-digit count).
    let own = |b: TradeWindowTicketButton<'a>| if snug { b.snug() } else { b };
    let muted = |name: &str| {
        own(TradeWindowTicketButton::new(name).words_in(t, name, t.theme.text_secondary))
    };
    [all, muted("Bids"), muted("Asks"), own(padlock_button(t, one_click))]
}

/// How the cancel row fills rows `width` wide ([`pack`]): its three buttons, then the padlock, and
/// whether its Bids, Asks and padlock are snug. They have their own padding where the row fits one
/// line with it, and half of it where that is what keeps the row one line (a row that wraps either
/// way keeps the padding: snug buttons would only move where it wraps).
pub(super) fn cancel_layout(
    ui: &egui::Ui,
    t: &ThemeTokens,
    inputs: &TradeWindowInputs<'_>,
    one_click: bool,
    counted: bool,
    width: f32,
) -> (Vec<usize>, bool) {
    cancel_layout_of(t, width, |snug| cancel_naturals(ui, t, inputs, one_click, counted, snug))
}

/// [`cancel_layout`] over the four buttons' natural widths (`naturals(snug)`): how a window that has
/// no orders yet counts the row it must have room for ([`super::full::opening_h`]).
pub(super) fn cancel_layout_of(
    t: &ThemeTokens,
    width: f32,
    naturals: impl Fn(bool) -> Vec<f32>,
) -> (Vec<usize>, bool) {
    let rows = |snug| pack(&naturals(snug), t.metrics.gap, width);
    let wide = rows(false);
    let snug = if wide.len() > 1 { rows(true) } else { Vec::new() };
    if snug.len() == 1 { (snug, true) } else { (wide, false) }
}

fn cancel_naturals(
    ui: &egui::Ui,
    t: &ThemeTokens,
    inputs: &TradeWindowInputs<'_>,
    one_click: bool,
    counted: bool,
    snug: bool,
) -> Vec<f32> {
    cancel_btns(t, inputs, counted, one_click, snug)
        .iter()
        .map(|b| b.natural_w(ui.ctx(), t))
        .collect()
}

/// Cancel all, Bids and Asks, then the one-click padlock, in `rows` ([`cancel_layout`]). A cancel is
/// sent at once, never held. On a node that cannot say which account an order belongs to
/// (`TradeWindowInputs::orders_why`, Ruling R9) every cancel is disabled with THAT reason and claims no
/// count (pre-flight Minor 15). Where `counted` is false and the count is known, it is on hover.
pub(super) fn cancel_row(
    ui: &mut egui::Ui,
    t: &ThemeTokens,
    key: egui::Id,
    state: &mut TradeWindowState,
    inputs: &TradeWindowInputs<'_>,
    actions: &mut Vec<TradeWindowAction>,
    (rows, snug): (&[usize], bool),
    counted: bool,
) {
    let bids = inputs.orders.iter().filter(|o| o.side > 0).count();
    let asks = inputs.orders.iter().filter(|o| o.side < 0).count();
    let cancels = [
        (TradeWindowAction::CancelAll, inputs.orders.len(), NO_ORDERS_WHY, "cancel_all"),
        (TradeWindowAction::CancelSide(1), bids, NO_BIDS_WHY, "cancel_bids"),
        (TradeWindowAction::CancelSide(-1), asks, NO_ASKS_WHY, "cancel_asks"),
    ];
    let naturals = cancel_naturals(ui, t, inputs, state.one_click, counted, snug);
    in_rows(ui, t, rows, &naturals, TradeWindowTicketRowFill::Item(0), |ui, i, w| {
        let Some((act, n, none, role)) = cancels.get(i) else {
            let [.., lock] = cancel_btns(t, inputs, counted, state.one_click, snug);
            padlock(ui, key, lock, state, actions);
            return;
        };
        let why = inputs.orders_why.or((*n == 0).then_some(*none));
        let [all, bids, asks, _] = cancel_btns(t, inputs, counted, state.one_click, snug);
        let b = [all, bids, asks].into_iter().nth(i).expect("one of the three");
        let b = if i == 0 { b.width(w) } else { b };
        let b = match why {
            Some(w) => b.disabled_because(w),
            None => b,
        };
        let mut r = keyed(ui, key.with(role), b);
        if i == 0 && !counted && why.is_none() {
            r = r.on_hover_text(format!("Cancel all {} working orders", inputs.orders.len()));
        }
        if r.clicked() {
            actions.push(act.clone());
        }
    });
}

/// The one-click padlock as a button: PRESSED while one-click trading is on, the state its name
/// states (the approved prototype could not be read here; spec §3.6 and the toggle's own name
/// decide it, pre-flight Minor 17). Named by its tip, as an icon with no word is.
fn padlock_button<'a>(t: &ThemeTokens, one_click: bool) -> TradeWindowTicketButton<'a> {
    let (icon, tip) = if one_click {
        (icons::ONE_CLICK_ON, ONE_CLICK_ON_TIP)
    } else {
        (icons::ONE_CLICK_OFF, ONE_CLICK_OFF_TIP)
    };
    TradeWindowTicketButton::new(tip).icon(t, icon, None).toggled(one_click)
}

/// The one-click padlock. Changing it lets go of a held order, and says so as the strip's Cancel
/// does, "Not sent." (final review A, minor 4).
fn padlock(
    ui: &mut egui::Ui,
    key: egui::Id,
    lock: TradeWindowTicketButton<'_>,
    state: &mut TradeWindowState,
    actions: &mut Vec<TradeWindowAction>,
) {
    let tip = if state.one_click { ONE_CLICK_ON_TIP } else { ONE_CLICK_OFF_TIP };
    if keyed(ui, key.with("one_click"), lock).on_hover_text(tip).clicked() {
        state.one_click = !state.one_click;
        if state.held.take().is_some() {
            let text = crate::trade_window::status_line::NOT_SENT.to_string();
            actions.push(TradeWindowAction::Note { kind: TradeWindowStatusLineKind::Info, text });
        }
    }
}

/// The unit switch: the unit the size is in, and the arrows. It converts the size at
/// [`reference_price`], or, with no size to convert, empties the field: a number never silently
/// changes what it means. With no price to convert at it is disabled.
fn unit_button<'a>(
    t: &ThemeTokens,
    state: &TradeWindowState,
    inputs: &TradeWindowInputs<'_>,
    convertible: bool,
) -> TradeWindowTicketButton<'a> {
    let (to, now) = match state.unit {
        TradeWindowSizingUnit::Base => (inputs.quote, inputs.base),
        TradeWindowSizingUnit::Quote => (inputs.base, inputs.quote),
    };
    let b = TradeWindowTicketButton::new(format!("Enter the size in {to} instead"))
        .words(t, now)
        .icon(t, icons::SWAP_UNITS, Some(t.theme.text_tertiary));
    if convertible { b } else { b.disabled_because(NO_CONVERT_WHY) }
}

fn swap_units(
    ui: &mut egui::Ui,
    t: &ThemeTokens,
    state: &mut TradeWindowState,
    inputs: &TradeWindowInputs<'_>,
    order_type: TradeWindowTicketKind,
) {
    let to = match state.unit {
        TradeWindowSizingUnit::Base => TradeWindowSizingUnit::Quote,
        TradeWindowSizingUnit::Quote => TradeWindowSizingUnit::Base,
    };
    let at = reference_price(state, order_type, inputs);
    let tip = match to {
        TradeWindowSizingUnit::Quote => format!("Enter the size in {} instead", inputs.quote),
        TradeWindowSizingUnit::Base => format!("Enter the size in {} instead", inputs.base),
    };
    if ui.add(unit_button(t, state, inputs, at.is_some())).on_hover_text(tip).clicked() {
        let base =
            sizing::base_quantity(&state.size, state.unit, at, inputs.grid.lot_size, inputs.value);
        state.unit = to;
        match base {
            Some(q) => write_size(state, q, at, inputs),
            None => state.size.clear(),
        }
    }
}

/// The caption of a field's row, at its left (`Price`, `Trigger`, `Size`).
fn row_label(ui: &mut egui::Ui, t: &ThemeTokens, words: &str) {
    ui.label(
        RichText::new(words).font(t.font(ThemeTextRole::Caption)).color(t.theme.text_tertiary),
    );
}

/// The size field in its row, `Size`, the field and the unit switch, for a field `field_w` wide
/// where the caller has fixed it, else the width the row leaves. Its answer is whether the field
/// holds something that is not a number.
pub(super) fn size_row(
    ui: &mut egui::Ui,
    t: &ThemeTokens,
    state: &mut TradeWindowState,
    inputs: &TradeWindowInputs<'_>,
    order_type: TradeWindowTicketKind,
    field_w: Option<f32>,
) -> bool {
    let context = ui.ctx().clone();
    let gap = t.metrics.gap;
    let at = reference_price(state, order_type, inputs);
    let unit_w = unit_button(t, state, inputs, at.is_some()).natural_w(&context, t);
    row_label(ui, t, "Size");
    let w =
        field_w.unwrap_or_else(|| ui.available_width() - unit_w - gap).max(trade::MIN_FIELD_WIDTH);
    let (response, bad) = number_field(ui, t, &mut state.size, "0", w);
    if bad {
        let _ = response.on_hover_text("Not a number");
    }
    swap_units(ui, t, state, inputs, order_type);
    bad
}

/// The five quick sizes, lot multiples (spec §3.6), in rows `ui`'s width wide: every one, wrapped
/// onto as many rows as they take, or, with `one_row` (the compact ticket's fixed row), as many as
/// fit on one — the rest are the full ticket's. Each row's buttons share its width, and the one
/// that IS the size field's reads chosen. While the lot is not known each is disabled and says
/// why, rather than reading `0`; in the quote currency with no price to convert at, too.
pub(super) fn quick_sizes(
    ui: &mut egui::Ui,
    t: &ThemeTokens,
    state: &mut TradeWindowState,
    inputs: &TradeWindowInputs<'_>,
    order_type: TradeWindowTicketKind,
    one_row: bool,
) {
    let lot = inputs.grid.lot_size;
    let at = reference_price(state, order_type, inputs);
    let why = if lot.is_none() {
        Some(no_lot_reason(inputs.symbol))
    } else if state.unit == TradeWindowSizingUnit::Quote && at.is_none() {
        Some(NO_CONVERT_WHY.to_string())
    } else {
        None
    };
    let sizes = sizing::quick_sizes(inputs.grid);
    let words =
        sizes.map(|q| if known(q) { sizing::quantity_text(q, lot) } else { NO_SIZE.to_string() });
    let held = sizing::base_quantity(&state.size, state.unit, at, lot, inputs.value);
    let naturals: Vec<f32> =
        words.iter().map(|w| digits_button(t, w).natural_w(ui.ctx(), t)).collect();
    let mut rows = pack(&naturals, t.metrics.gap, ui.available_width());
    if one_row {
        rows.truncate(1);
    } else {
        rows = balance(rows);
    }
    in_rows(ui, t, &rows, &naturals, TradeWindowTicketRowFill::Equal, |ui, i, w| {
        let chosen = known(sizes[i])
            && held.zip(lot).is_some_and(|(q, lot)| (q - sizes[i]).abs() < 0.5 * lot);
        let b = digits_button(t, &words[i]).width(w);
        let b = if chosen { b.look(TradeWindowTicketButtonLook::Chosen) } else { b };
        let b = match &why {
            Some(w) => b.disabled_because(w),
            None => b,
        };
        if ui.add(b).clicked() {
            write_size(state, sizes[i], at, inputs);
        }
    });
}

/// The order's value in the quote currency, at [`reference_price`]: `≈ 1,000.00 USDT`. The
/// contract's notional (`ContractValue::order_notional`), so one ES contract at 5,000 reads 250,000.
pub(super) fn value_text(
    state: &TradeWindowState,
    inputs: &TradeWindowInputs<'_>,
    order_type: TradeWindowTicketKind,
) -> String {
    let (lot, unit) = (inputs.grid.lot_size, inputs.value);
    let at = reference_price(state, order_type, inputs);
    let value = sizing::base_quantity(&state.size, state.unit, at, lot, unit)
        .zip(at)
        .map(|(q, p)| unit.order_notional(q, p));
    let value = value.map_or_else(|| NO_SIZE.to_string(), |v| format_thousands_precision(v, 2));
    format!("≈ {value} {}", inputs.quote)
}

/// The largest size the free buying power allows at [`reference_price`]: `max 100.000 BTC`. A
/// KNOWN zero says so — `max 0 BTC · no buying power`, `max 0 BTC · under one lot` — and the dash
/// is only for a max nobody can compute (no price to divide by, no lot, a buying power that is
/// not a number): a dash reads "not known", and hid a known zero behind it (the F wave's review,
/// minor 3). `None` while the buying power is not known — never while a price merely moves, so
/// whether the line is there does not change with a tick.
pub(super) fn max_text(
    state: &TradeWindowState,
    inputs: &TradeWindowInputs<'_>,
    order_type: TradeWindowTicketKind,
) -> Option<String> {
    let (lot, base) = (inputs.grid.lot_size, inputs.base);
    let at = reference_price(state, order_type, inputs);
    let buying_power = inputs.buying_power?;
    if buying_power.is_finite() && buying_power <= 0.0 {
        return Some(format!("max 0 {base} · no buying power"));
    }
    let max = match at.filter(|_| lot.is_some() && buying_power.is_finite()) {
        Some(p) => match sizing::share_of_buying_power(100.0, buying_power, p, lot, inputs.value) {
            Some(m) => sizing::quantity_text(m, lot),
            None => return Some(format!("max 0 {base} · under one lot")),
        },
        None => NO_SIZE.to_string(),
    };
    Some(format!("max {max} {base}"))
}

/// The shares of the free buying power the ticket offers, in percent.
const PERCENTS: [f64; 4] = [10.0, 25.0, 50.0, 100.0];

/// The shares' buttons' words: `10%`, `25%`, `50%`, `100%`.
pub(super) fn share_words() -> [String; 4] {
    PERCENTS.map(|pct| format!("{pct:.0}%"))
}

/// 10 %, 25 %, 50 % and 100 % of the free buying power, each disabled with its own reason, in as
/// many rows of `ui`'s width as they take, each row's buttons sharing it.
pub(super) fn shares(
    ui: &mut egui::Ui,
    t: &ThemeTokens,
    state: &mut TradeWindowState,
    inputs: &TradeWindowInputs<'_>,
    order_type: TradeWindowTicketKind,
) {
    let lot = inputs.grid.lot_size;
    let at = reference_price(state, order_type, inputs);
    let words = share_words();
    let naturals: Vec<f32> =
        words.iter().map(|w| digits_button(t, w).natural_w(ui.ctx(), t)).collect();
    let rows = balance(pack(&naturals, t.metrics.gap, ui.available_width()));
    in_rows(ui, t, &rows, &naturals, TradeWindowTicketRowFill::Equal, |ui, i, w| {
        let q = inputs.buying_power.zip(at).and_then(|(buying_power, p)| {
            sizing::share_of_buying_power(PERCENTS[i], buying_power, p, lot, inputs.value)
        });
        let why = if lot.is_none() {
            Some(no_lot_reason(inputs.symbol))
        } else if inputs.buying_power.is_none() {
            Some(NO_BUYING_POWER_WHY.to_string())
        } else if at.is_none() {
            Some(NO_PRICE_WHY.to_string())
        } else if q.is_none() {
            Some(format!("{} of the buying power is less than one lot.", words[i]))
        } else {
            None
        };
        let b = digits_button(t, &words[i]).width(w);
        let b = match &why {
            Some(w) => b.disabled_because(w),
            None => b,
        };
        if ui.add(b).clicked()
            && let Some(q) = q
        {
            write_size(state, q, at, inputs);
        }
    });
}

/// A toggle: PRESSED while `on`, its check drawn in the accent (Reduce only, Post only). Named by
/// its words, whatever it draws; `snug` is the compact ticket's tightest row.
pub(super) fn toggle_button<'a>(
    t: &ThemeTokens,
    on: bool,
    word: &str,
    snug: bool,
) -> TradeWindowTicketButton<'a> {
    let b = TradeWindowTicketButton::new(word);
    let b = if on { b.icon(t, icons::CHECK, Some(t.theme.accent)) } else { b };
    let b = b.words(t, word).toggled(on);
    if snug { b.snug() } else { b }
}

/// Leverage, drawn and disabled with the owner's words (spec §3.7), the last growing to fill its
/// row. It takes no click, so it never produces an intent (spec §7); the compact ticket's `Post` is
/// drawn the same way, since that ticket sends market orders only ([`super::POST_COMPACT_WHY`]).
pub(super) fn greyed_button<'a>(
    t: &ThemeTokens,
    words: &str,
    why: &'a str,
) -> TradeWindowTicketButton<'a> {
    TradeWindowTicketButton::text(t, words).disabled_because(why)
}

/// Why the Post only toggle is disabled in the full ticket, or `None` where it takes a click. A
/// window that takes no order says THAT ([`why_untradable`]): the toggle is an order control like
/// any other there, showing the trader's setting and the window's reason; otherwise the reason is
/// [`post_only_block`]'s for the entry type the ticket shows.
pub(super) fn post_only_off(
    inputs: &TradeWindowInputs<'_>,
    order_type: TradeWindowTicketKind,
) -> Option<String> {
    why_untradable(inputs).or_else(|| post_only_block(inputs, order_type))
}

/// How Buy and Sell fill rows `width` wide: side by side, `[2]`, or one to a row, `[1, 1]`. A label
/// NAMES the order, so it is never cut to make it fit.
///
/// ⚠ With a margin, remembered under `key` from frame to frame, so the rows do not flicker at the
/// boundary: they stack the moment both labels do not fit side by side, and go back side by side
/// only once they fit with two digits to spare. A quote-sized order's labels change on every tick
/// of the book (its size is converted at the side's best price), and without the margin a book
/// ticking at the boundary re-packed the rows tick after tick (W4 fix round 1, I-1; [`keyed`] is
/// what makes a re-pack under a held press harmless).
pub(super) fn send_rows(
    ui: &egui::Ui,
    t: &ThemeTokens,
    key: egui::Id,
    state: &TradeWindowState,
    inputs: &TradeWindowInputs<'_>,
    entry: TradeWindowTicketEntry<'_>,
    width: f32,
) -> Vec<usize> {
    let [buy, sell] =
        [1, -1].map(|side| send_button(t, side, entry, state, inputs).natural_w(ui.ctx(), t));
    let memory = key.with("trade_send_stacked");
    let was = ui.data(|d| d.get_temp::<bool>(memory)).unwrap_or(false);
    let digits = text_w(ui.ctx(), "00", &t.mono_semibold(ThemeTextRole::Strong), t);
    let stacked = buy + t.metrics.gap + sell + if was { digits } else { 0.0 } > width;
    ui.data_mut(|d| d.insert_temp(memory, stacked));
    if stacked { vec![1, 1] } else { vec![2] }
}

/// Which Buy and Sell a ticket draws: the entry they send, and the word a market one is named by
/// (`market` in the full ticket, `MKT` in the compact one).
#[derive(Clone, Copy)]
pub(super) struct TradeWindowTicketEntry<'w> {
    pub(super) order_type: TradeWindowTicketKind,
    pub(super) market_word: &'w str,
}

/// A Buy or Sell button as it is drawn: `Buy 0.010 limit`, its number in the mono face.
fn send_button<'a>(
    t: &ThemeTokens,
    side: i32,
    entry: TradeWindowTicketEntry<'_>,
    state: &TradeWindowState,
    inputs: &TradeWindowInputs<'_>,
) -> TradeWindowTicketButton<'a> {
    let (verb, quantity, kind) =
        side_words(side, entry.order_type, state, inputs, entry.market_word);
    send_button_of(t, side, (&verb, &quantity, &kind))
}

/// [`send_button`] for the words `(verb, quantity, kind)`.
pub(super) fn send_button_of<'a>(
    t: &ThemeTokens,
    side: i32,
    (verb, quantity, kind): (&str, &str, &str),
) -> TradeWindowTicketButton<'a> {
    let strong = t.font_semibold(ThemeTextRole::Strong);
    TradeWindowTicketButton::new(format!("{verb} {quantity} {kind}"))
        .part(&format!("{verb} "), strong.clone(), None)
        .part(quantity, t.mono_semibold(ThemeTextRole::Strong), None)
        .part(&format!(" {kind}"), strong, None)
        .look(if side > 0 {
            TradeWindowTicketButtonLook::Buy
        } else {
            TradeWindowTicketButtonLook::Sell
        })
        .inline()
        .snug()
        .tall()
}

/// The Buy and Sell buttons for `entry`, in `rows` ([`send_rows`]): each names the order
/// [`order_as`] builds, and is disabled with its reason when it refuses.
pub(super) fn send_buttons(
    ui: &mut egui::Ui,
    t: &ThemeTokens,
    key: egui::Id,
    state: &mut TradeWindowState,
    inputs: &TradeWindowInputs<'_>,
    actions: &mut Vec<TradeWindowAction>,
    entry: TradeWindowTicketEntry<'_>,
    rows: &[usize],
) {
    let naturals: Vec<f32> = [1, -1]
        .map(|side| send_button(t, side, entry, state, inputs).natural_w(ui.ctx(), t))
        .to_vec();
    in_rows(ui, t, rows, &naturals, TradeWindowTicketRowFill::Equal, |ui, i, w| {
        let (side, role) = if i == 0 { (1, "buy") } else { (-1, "sell") };
        let made = order_as(side, entry.order_type, state, inputs);
        let base = send_button(t, side, entry, state, inputs).width(w);
        let button = match &made {
            Ok(_) => base,
            Err(why) => base.disabled_because(why),
        };
        if keyed(ui, key.with(role), button).clicked()
            && let Ok(a) = made
        {
            state.submit(a, actions);
        }
    });
}

/// The price (Limit) or trigger (Stop) field, with Bid and Ask beside it to fill in the best. Bid
/// and Ask only TYPE that price into the field; they send nothing. Its answer is whether the field
/// holds something that is not a number.
pub(super) fn price_row(
    ui: &mut egui::Ui,
    t: &ThemeTokens,
    state: &mut TradeWindowState,
    inputs: &TradeWindowInputs<'_>,
) -> bool {
    let hint = if state.order_type == TradeWindowTicketKind::Stop { "Trigger" } else { "Price" };
    let context = ui.ctx().clone();
    let gap = t.metrics.gap;
    let (bid, ask) = best(inputs);
    let (bid_ink, ask_ink) = ThemeSidePair::BidAsk.texts(t);
    let sides = [("Bid", bid, bid_ink), ("Ask", ask, ask_ink)];
    let buttons =
        sides.map(|(words, _, ink)| TradeWindowTicketButton::new(words).words_in(t, words, ink));
    let trailing: f32 = buttons.iter().map(|b| b.natural_w(&context, t) + gap).sum();
    row_label(ui, t, hint);
    let w = (ui.available_width() - trailing).max(trade::MIN_FIELD_WIDTH);
    let (_, bad) = number_field(ui, t, &mut state.price, hint, w);
    for ((_, price, _), b) in sides.into_iter().zip(buttons) {
        let b = if price.is_some() { b } else { b.disabled_because(NO_SIDE_WHY) };
        if ui.add(b).clicked()
            && let Some(p) = price
        {
            state.price = price_text(p, inputs);
        }
    }
    bad
}

pub(super) fn caption(ui: &mut egui::Ui, t: &ThemeTokens, text: impl Into<String>) {
    ui.label(caption_text(t, text));
}

/// A caption's font: the mono Caption role.
pub(super) fn caption_font(t: &ThemeTokens) -> FontId {
    t.mono(ThemeTextRole::Caption)
}

/// A caption's words, in [`caption_font`] and the third text colour.
pub(super) fn caption_text(t: &ThemeTokens, text: impl Into<String>) -> RichText {
    RichText::new(text).font(caption_font(t)).color(t.theme.text_tertiary)
}

/// A line that says why, in `colour`, cut to the row with the whole text on hover (egui shows an
/// elided label's text on hover): for a fixed row that has room for one line.
pub(super) fn reason_line(ui: &mut egui::Ui, t: &ThemeTokens, words: &str, colour: Color32) {
    let text = RichText::new(words).font(t.font(ThemeTextRole::Body)).color(colour);
    ui.add(egui::Label::new(text).truncate());
}

/// A child of `ui` laid out top to bottom in `rect` and clipped to it, its id `salt` under `ui`'s.
/// It keeps `ui`'s spacing.
pub(super) fn part(ui: &mut egui::Ui, rect: Rect, salt: &str) -> egui::Ui {
    let mut child = ui.new_child(
        UiBuilder::new().id_salt(salt).max_rect(rect).layout(Layout::top_down(Align::Min)),
    );
    child.shrink_clip_rect(rect);
    child
}
