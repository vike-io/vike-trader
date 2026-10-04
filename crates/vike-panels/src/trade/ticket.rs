//! The tickets (spec §3.6): the full one beside the ladder (or alone), and the compact one under
//! it, for a narrow window.
//!
//! # ⚠ The money path
//!
//! [`order`] is the one place the ticket becomes a [`TradeAction::Place`]. The Buy and Sell buttons
//! send what it answers, name the size it computes, and are disabled with the reason it gives when
//! it refuses, so a button can never name one order and send another.
//!
//! - **A bracket is both legs or no order** (spec §4.4, "no lone exits"). With TP/SL on where it is
//!   allowed, a leg that is blank, not a number or not above zero refuses the order, and so does a
//!   grid with no room for an exit ([`sizing::exits`]). The entry is never sent without them.
//! - **TP/SL ticked where it is refused refuses the order** ([`tpsl_block`]: the node, the account,
//!   the entry type, reduce-only; fix round 1, I-3, ruled). The toggle stays ticked, the reason is
//!   written under it ([`tpsl_refusal`]), and it is enabled only so it can be turned off: the
//!   ticket never sends an order without the exits the trader set. A ladder click carries
//!   no exits by design (spec §3.6).
//! - **A typed price sits on the tick or is refused** (fix round 1, I-2, ruled): the core would
//!   round it to the nearest tick, past the trader's limit, with its exits measured from a price
//!   the order does not sit at.
//! - **A market order needs a price** (the owner's decision of 2026-10-03): with no side of the
//!   book to fill against and no last price, it is refused ([`NO_MARKET_PRICE_WHY`]). So is a
//!   Reverse, whose second half opens a position at market; a Close, which only reduces, is not.
//!   The same rule holds at the confirm: a market order or a Reverse held for one is dropped, with
//!   a note, when its side loses its price while it waits (`TradeState::sync`).
//! - **A Stop whose trigger would fire at once is WARNED, never refused** (`stop_warning`): a
//!   line under the trigger before the click, and the same words in the confirm prompt.
//! - **A size converts at the order's own price**: a Limit's price or a Stop's trigger, else the
//!   side's best (the ask for a buy, the bid for a sell), else the last price, as a ladder click
//!   converts at its row's price.
//! - **No size is invented** (pre-flight I18). A lot the catalog has not given refuses every size,
//!   offers no quick size and never prints `(0)`; a quote value with no price to convert at is never
//!   written into the size field.

use egui::text::{LayoutJob, TextFormat};
use egui::{Align, Color32, FontId, Layout, Rect, RichText, UiBuilder, pos2};
use vike_ui_theme::components::button::{ActionButton, IconButton};
use vike_ui_theme::components::{Status, Tokens, input, segmented, toggle};
use vike_ui_theme::fmt::fmt_thousands_prec;
use vike_ui_theme::icons;
use vike_ui_theme::type_scale::TextRole;

use super::{
    Exits, OrderType, Origin, SizeUnit, StatusKind, Tradable, TradeAction, TradeInputs, TradeState,
    button_w, keyed, label_h, ladder, name_group, section, sizing, why_untradable,
};

/// The owner's words for the greyed-out Post only (spec §3.7).
pub const POST_WHY: &str = "Not available yet: Vike cannot send post-only orders to the exchange.";
/// The owner's words for the greyed-out Leverage (spec §3.7).
pub const LEVERAGE_WHY: &str = "Not available yet: Vike cannot set leverage on the exchange.";
/// Why TP/SL is off while the node cannot carry a bracket ([`TradeInputs::bracket_wire`], Ruling
/// R8). The owner's "Not available yet" wording, as for Post only and Leverage. It outranks every
/// other reason: no account and no entry type changes it.
pub const TPSL_WIRE_WHY: &str =
    "Not available yet: Vike cannot send TP/SL orders through the node.";
/// Why TP/SL is off on any account but a venue's single default one (Ruling R3 as widened for the
/// node's bracket command). The GLUE states it, in [`TradeInputs::bracket_why`]; the ticket says
/// whatever cause it is handed and never picks this one itself.
pub const TPSL_ACCOUNT_WHY: &str = "TP/SL is not available on this account yet: a bracket reaches \
                                    only a venue's single default account.";
/// Why TP/SL is off for a Stop entry: a bracket has no stop entry (spec §3.6).
pub const TPSL_STOP_WHY: &str = "TP/SL needs a Market or Limit entry.";
/// Why TP/SL is off with Reduce only: a bracket is for an order that opens or adds.
pub const TPSL_REDUCE_WHY: &str = "TP/SL needs an order that opens or adds; Reduce only is on.";
/// Why an order with TP/SL on is refused while a leg is blank, not a number or not above zero: a
/// bracket is both exits or no order, never its entry alone.
pub const TPSL_LEGS_WHY: &str =
    "TP/SL is on: enter both TP and SL as percentages above zero, or turn TP/SL off.";
/// Why an order with TP/SL on is refused where its exits cannot be placed ([`sizing::exits`]
/// answers `None`): no tick grid, or an exit that would fall to zero or below.
pub const TPSL_ROOM_WHY: &str = "TP/SL cannot be set here: each exit needs a price on the tick \
                                 grid, above zero and at least one tick from the entry.";
/// Why an order with TP/SL on is refused while there is no price to measure its exits from.
pub const TPSL_ENTRY_WHY: &str = "TP/SL is on, but there is no price to set its exits from.";
/// Why a market Buy or Sell is refused where there is no price for it at all: no side of the book
/// to fill against and no last price (the owner's decision of 2026-10-03, round 2, item 10).
pub const NO_MARKET_PRICE_WHY: &str = "No price yet: a market order needs a book or a last price.";
/// Why the cancel buttons are disabled with nothing to cancel.
pub const NO_ORDERS_WHY: &str = "No working orders on this symbol.";
/// The one-click padlock's name and tip while one-click trading is on.
pub const ONE_CLICK_ON_TIP: &str =
    "One-click trading is on: a click sends at once. Click to ask first.";
/// ...and while it is off.
pub const ONE_CLICK_OFF_TIP: &str =
    "One-click trading is off: every order asks first. Click to send at once.";

/// What the compact ticket writes beside a ticked TP/SL it cannot use, where the whole reason
/// ([`tpsl_refusal`]) does not fit: the reason is its hover.
const TPSL_OFF_TO_SEND: &str = "Turn TP/SL off to send.";
/// ...and where not even those words fit its row.
const TPSL_OFF: &str = "Turn TP/SL off.";
const NO_BIDS_WHY: &str = "No working buy orders on this symbol.";
const NO_ASKS_WHY: &str = "No working sell orders on this symbol.";
const NO_SIDE_WHY: &str = "No price on that side of the book.";
const NO_BUYING_POWER_WHY: &str = "The account's buying power is not known.";
const NO_PRICE_WHY: &str = "No price to size against yet.";
const NO_CONVERT_WHY: &str = "No price to convert the size at.";
/// What a quick size, a value or a size reads where there is none to show.
const NO_SIZE: &str = "—";
/// The narrowest a number field is drawn, however crowded its row.
const MIN_FIELD_W: f32 = 48.0;

/// Whether `v` is a usable quantity or price: a number above zero.
fn known(v: f64) -> bool {
    v > 0.0 && v.is_finite()
}

/// Why no size can be sent while the instrument catalog has given no lot for `symbol` (pre-flight
/// I18). It never prints the lot: a `(0)` reads as a size the trader could reach.
pub fn no_lot_reason(symbol: &str) -> String {
    format!(
        "No lot size for {symbol}: the instrument catalog does not give one, so no size can be sent."
    )
}

/// Why TP/SL cannot be used for an entry of `order_type` now, or `None` when it can. The node
/// first, then the cause the app states for this account ([`TradeInputs::bracket_why`]: the
/// account, or its engine's lane), then the entry type, then reduce-only.
pub fn tpsl_block<'a>(
    state: &TradeState,
    inputs: &TradeInputs<'a>,
    order_type: OrderType,
) -> Option<&'a str> {
    if !inputs.bracket_wire {
        Some(TPSL_WIRE_WHY)
    } else if let Some(why) = inputs.bracket_why {
        Some(why)
    } else if order_type == OrderType::Stop {
        Some(TPSL_STOP_WHY)
    } else if state.reduce_only {
        Some(TPSL_REDUCE_WHY)
    } else {
        None
    }
}

/// Why the ticket's Buy and Sell send nothing while TP/SL is ticked where it cannot be used
/// (`why` is [`tpsl_block`]'s reason): the order is refused until the trader turns TP/SL off, never
/// sent without the exits they set (fix round 1, I-3).
pub fn tpsl_refusal(why: &str) -> String {
    format!("{why} Turn TP/SL off to send without exits.")
}

/// Whether `price` sits on the `tick` grid: [`sizing::on_tick`]'s nearest tick is the price itself.
/// The slack is the grid code's billionth of a tick, or, for a large price on a fine tick, a few
/// units in the last place of the price itself: a typed decimal reaches here rounded to the
/// nearest double, and `1,234,567.89` on a 0.01 tick is several billionths of a tick from its own
/// nearest tick. Any price a trader can type off the grid misses it by far more than either.
fn on_the_tick(price: f64, tick: f64) -> bool {
    let slack = (1e-9 * tick).max(8.0 * f64::EPSILON * price.abs());
    (price - sizing::on_tick(price, tick)).abs() <= slack
}

/// The best bid and ask.
fn best(inputs: &TradeInputs<'_>) -> (Option<f64>, Option<f64>) {
    (inputs.book.best_bid().map(|l| l.price), inputs.book.best_ask().map(|l| l.price))
}

/// The price a market order of `side` takes: the ask for a buy, the bid for a sell, else the last.
/// `None` refuses a market order at the click ([`order`], the Reverse button) and drops one held
/// for a confirm (`TradeState::sync`): the same rule for both.
pub(super) fn market_ref(side: i32, inputs: &TradeInputs<'_>) -> Option<f64> {
    let (bid, ask) = best(inputs);
    [if side > 0 { ask } else { bid }, inputs.last].into_iter().flatten().find(|p| known(*p))
}

/// What a Stop of `side` at `trigger` says where the trigger is on the WRONG side of the market —
/// a buy stop below the ask, a sell stop above the bid (else below or above the last price) — so
/// that it triggers at once: `Stop BUY 98.0 is below the market 100.0: it triggers at once.`, with
/// the window's own numbers. `None` on the right side, or with no market to compare with. A
/// WARNING, never a refusal: venues differ on whether they fire such a stop or reject it, and the
/// order stays the trader's to send. The ticket writes it under the trigger before the click, and
/// the confirm prompt says the same words (the owner's decision of 2026-10-03, round 2, item 9).
pub(super) fn stop_warning(side: i32, trigger: f64, inputs: &TradeInputs<'_>) -> Option<String> {
    let market = market_ref(side, inputs)?;
    let wrong = if side > 0 { trigger < market } else { trigger > market };
    let (word, place) = if side > 0 { ("BUY", "below") } else { ("SELL", "above") };
    let tick = inputs.grid.tick;
    wrong.then(|| {
        format!(
            "Stop {word} {} is {place} the market {}: it triggers at once.",
            ladder::fmt_px(trigger, tick),
            ladder::fmt_px(market, tick)
        )
    })
}

/// The typed price — a Limit's price, a Stop's trigger — when it is a number above zero.
fn typed_price(state: &TradeState) -> Option<f64> {
    input::parse_number(&state.price).filter(|p| known(*p))
}

/// The price an order is placed at, and its exits measured from: its own for a Limit or a Stop,
/// the side's best for a Market order.
fn entry_price(
    side: i32,
    order_type: OrderType,
    state: &TradeState,
    inputs: &TradeInputs<'_>,
) -> Option<f64> {
    match order_type {
        OrderType::Market => market_ref(side, inputs),
        OrderType::Limit | OrderType::Stop => typed_price(state),
    }
}

/// The base size an order of `side` and `order_type` carries: the size field, converted from the
/// quote currency at the entry price (the side's best while a Limit has no price yet), floored to
/// the lot. [`order`] sends it and the Buy and Sell buttons name it: one function, so the two
/// cannot disagree.
fn order_qty(
    side: i32,
    order_type: OrderType,
    state: &TradeState,
    inputs: &TradeInputs<'_>,
) -> Option<f64> {
    let at = entry_price(side, order_type, state, inputs).or_else(|| market_ref(side, inputs));
    sizing::base_qty(&state.size, state.unit, at, inputs.grid.lot)
}

/// Why the size field holds no size an order converted at `at` can carry.
fn size_refusal(state: &TradeState, inputs: &TradeInputs<'_>, at: Option<f64>) -> String {
    let lot = inputs.grid.lot;
    // A number so large its count of lots is not a number (`1e306` on a 0.001 lot).
    let lots = input::parse_number(&state.size).map(|v| match state.unit {
        SizeUnit::Base => v / lot,
        SizeUnit::Quote => v / at.unwrap_or(1.0) / lot,
    });
    if !known(lot) {
        no_lot_reason(inputs.symbol)
    } else if state.unit == SizeUnit::Quote && at.is_none() {
        format!("No price to convert the size at: enter it in {}.", inputs.base)
    } else if lots.is_some_and(|n| !n.is_finite()) {
        format!("{} is too large a size to send.", state.size.trim())
    } else {
        format!(
            "Enter a size of at least one lot ({} {}).",
            sizing::qty_text(lot, lot),
            inputs.base
        )
    }
}

/// The size an order converted at `at` carries, or why it cannot: one lot at least, a finite
/// number of lots (fix round 1, Minor 3: no `Place` ever carries an infinite quantity), and no
/// less than the instrument's smallest order (Minor 4). The ladder's clicks take this rule too,
/// so one rule applies wherever an order is made.
pub(super) fn checked_qty(
    qty: Option<f64>,
    state: &TradeState,
    inputs: &TradeInputs<'_>,
    at: Option<f64>,
) -> Result<f64, String> {
    let qty = qty.filter(|q| known(*q)).ok_or_else(|| size_refusal(state, inputs, at))?;
    let (lot, min) = (inputs.grid.lot, inputs.grid.min_qty);
    if known(min) && qty + 1e-9 * lot < min {
        return Err(format!(
            "Below the smallest order size: enter at least {} {}.",
            sizing::qty_text(min, lot),
            inputs.base
        ));
    }
    Ok(qty)
}

/// The ticket's order for `side`, or why it cannot be sent.
pub fn order(
    side: i32,
    state: &TradeState,
    inputs: &TradeInputs<'_>,
) -> Result<TradeAction, String> {
    order_as(side, state.order_type, state, inputs)
}

/// [`order`] for an entry of `order_type`, whatever type the ticket shows: the compact ticket's
/// Buy and Sell send at market without changing the type the trader chose (pre-flight Minor 14).
fn order_as(
    side: i32,
    order_type: OrderType,
    state: &TradeState,
    inputs: &TradeInputs<'_>,
) -> Result<TradeAction, String> {
    order_with(side, order_type, state.tpsl, state, inputs)
}

/// Why the ticket's Buy and Sell send nothing for an entry of `order_type`, with TP/SL on or off as
/// `tpsl` says, WHATEVER is typed into the price and size fields — or `None` while they can send.
/// In this order, the order [`order_with`] refuses in, for it asks this first:
/// - the window takes no order ([`why_untradable`]);
/// - TP/SL is ticked where it cannot be used ([`tpsl_block`], said as [`tpsl_refusal`]);
/// - the instrument has no lot size ([`no_lot_reason`]);
/// - TP/SL is on with a leg that is not a percentage above zero ([`TPSL_LEGS_WHY`]).
///
/// The ONE source of those sentences for every place that says why the ticket sends nothing: a
/// disabled Buy's hover, the form's own lines, and the status strip, which says it in place of
/// "Ready" while nothing waits for a confirm (FW6, I3 and I4: the render check found disabled
/// Buy and Sell under a strip reading "Ready · one-click trading is on").
pub(super) fn standing_refusal(
    state: &TradeState,
    inputs: &TradeInputs<'_>,
    order_type: OrderType,
    tpsl: bool,
) -> Option<String> {
    if let Some(why) = why_untradable(inputs) {
        return Some(why);
    }
    // ⚠ TP/SL ticked where it cannot be used refuses the order (I-3): sent without its exits, it
    // would leave the trader unprotected while the ticket shows them protected.
    if tpsl && let Some(why) = tpsl_block(state, inputs, order_type) {
        return Some(tpsl_refusal(why));
    }
    if !known(inputs.grid.lot) {
        return Some(no_lot_reason(inputs.symbol));
    }
    (tpsl && legs(state).is_none()).then(|| TPSL_LEGS_WHY.to_string())
}

/// The TP and SL distances the trader typed, in percent, while both are numbers above zero.
fn legs(state: &TradeState) -> Option<(f64, f64)> {
    let leg = |text: &str| input::parse_number(text).filter(|v| known(*v));
    leg(&state.tp_text).zip(leg(&state.sl_text))
}

/// [`order_as`] with TP/SL on or off as `tpsl` says, whatever the trader's setting: the TP/SL lines
/// ask it with TP/SL ON, to show the exits the order WOULD carry before the trader turns it on.
fn order_with(
    side: i32,
    order_type: OrderType,
    tpsl: bool,
    state: &TradeState,
    inputs: &TradeInputs<'_>,
) -> Result<TradeAction, String> {
    if let Some(why) = standing_refusal(state, inputs, order_type, tpsl) {
        return Err(why);
    }
    let price = match order_type {
        // ⚠ A market order with no price at all — no side of the book to fill against and no
        // last price — goes out blind, and the app's notional cap, which needs a price, cannot
        // hold it either: it is refused (the owner's decision of 2026-10-03, round 2, item 10).
        OrderType::Market if market_ref(side, inputs).is_none() => {
            return Err(NO_MARKET_PRICE_WHY.to_string());
        }
        OrderType::Market => None,
        OrderType::Limit => Some(typed_price(state).ok_or("Enter a price.")?),
        OrderType::Stop => Some(typed_price(state).ok_or("Enter a trigger price.")?),
    };
    // ⚠ Off the tick is refused, never rounded (I-2). A tick nobody gave is not checked: an
    // instrument the catalog does not list has no lot either, so nothing is sent for it; only a
    // catalog row that gives a lot and no tick is sent at the price as typed.
    let tick = inputs.grid.tick;
    if let Some(p) = price
        && known(tick)
        && !on_the_tick(p, tick)
    {
        let t = ladder::fmt_px(tick, tick);
        return Err(format!(
            "{} is not on the {t} tick: use a multiple of {t}.",
            state.price.trim()
        ));
    }
    let at = entry_price(side, order_type, state, inputs).or_else(|| market_ref(side, inputs));
    let qty = checked_qty(order_qty(side, order_type, state, inputs), state, inputs, at)?;
    // ⚠ BOTH exits or no order: a bracket that cannot be built refuses the order, never sends its
    // entry alone, where the old ticket sent a plain order with one leg blank.
    let exits = if tpsl { Some(bracket(side, order_type, state, inputs)?) } else { None };
    Ok(TradeAction::Place {
        side,
        order_type,
        price,
        qty,
        reduce_only: state.reduce_only,
        exits,
        origin: Origin::Ticket,
    })
}

/// The exits TP/SL puts on an entry of `side`: both, or why not. [`order_with`] has refused a leg
/// that is not a number above zero before it asks ([`standing_refusal`]).
fn bracket(
    side: i32,
    order_type: OrderType,
    state: &TradeState,
    inputs: &TradeInputs<'_>,
) -> Result<Exits, String> {
    let entry = entry_price(side, order_type, state, inputs).ok_or(TPSL_ENTRY_WHY)?;
    let (tp, sl) = legs(state).ok_or(TPSL_LEGS_WHY)?;
    sizing::exits(side, entry, tp, sl, inputs.grid.tick).ok_or_else(|| TPSL_ROOM_WHY.to_string())
}

/// A price for the price FIELD: on the tick, with no grouping, so it reads back as typed.
fn price_text(p: f64, inputs: &TradeInputs<'_>) -> String {
    let step = [inputs.grid.tick, inputs.book.tick_size].into_iter().find(|t| known(*t));
    format!("{:.*}", sizing::decimals_of(step.unwrap_or(p)), p)
}

/// The one price the size field's own arithmetic reads (its value, the share of buying power, a
/// quick size or a unit switch written in the quote currency): the typed price for a Limit or
/// Stop, else the last price, else the book's mid. An ORDER converts at its own price
/// ([`order_qty`]), and the Buy and Sell buttons name that size.
fn reference_price(
    state: &TradeState,
    order_type: OrderType,
    inputs: &TradeInputs<'_>,
) -> Option<f64> {
    let typed = if order_type == OrderType::Market { None } else { typed_price(state) };
    [typed, inputs.last, inputs.book.mid()].into_iter().flatten().find(|p| known(*p))
}

/// Put the base size `base` into the size field, in the field's unit, at `at`. It writes nothing
/// for a size that is not above zero, nor for a quote value with no price to convert at: neither
/// `size_text`'s dash nor a `-0.00` is something the field may hold.
fn write_size(state: &mut TradeState, base: f64, at: Option<f64>, lot: f64) {
    if !known(base) {
        return;
    }
    let text = sizing::size_text(base, state.unit, at, lot);
    if text != sizing::NO_QUOTE {
        state.size = text;
    }
}

/// The Buy/Sell label, the order [`order_qty`] sizes: `Buy 0.010 limit`, `Sell 0.010 MKT`.
fn side_label(
    side: i32,
    order_type: OrderType,
    state: &TradeState,
    inputs: &TradeInputs<'_>,
    market_word: &str,
) -> String {
    let verb = if side > 0 { "Buy" } else { "Sell" };
    let q = order_qty(side, order_type, state, inputs)
        .map_or_else(|| NO_SIZE.to_string(), |q| sizing::qty_text(q, inputs.grid.lot));
    let kind = match order_type {
        OrderType::Market => market_word,
        OrderType::Limit => "limit",
        OrderType::Stop => "stop",
    };
    format!("{verb} {q} {kind}")
}

/// How many of `widths`, a gap apart and in order, each row `width` wide holds, filling a row before
/// starting the next: `[2]` is one row of two, `[1, 1]` two rows of one. Measured BEFORE anything is
/// drawn, so a row of kit buttons wraps where it must and its height is known in advance. egui's
/// `horizontal_wrapped` cannot wrap a kit button: the kit lays each one out in a scope of its own,
/// placed where the row's cursor stands, so the last one ran past the ticket's edge (MEASURED by the
/// fit sweep: the fifth quick size at Comfortable density), and every row after it inherited the
/// widened width.
fn pack(widths: &[f32], gap: f32, width: f32) -> Vec<usize> {
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

/// Draw items `0..` into the rows [`pack`] gave them, each row a horizontal line.
fn in_rows(ui: &mut egui::Ui, rows: &[usize], mut item: impl FnMut(&mut egui::Ui, usize)) {
    let mut i = 0;
    for &n in rows {
        ui.horizontal(|ui| {
            for _ in 0..n {
                item(ui, i);
                i += 1;
            }
        });
    }
}

/// The width a number field takes in its row: what is left once `trailing` (the controls after
/// it, with their gaps) has its room. Without it egui's 280 pt default takes the whole row and
/// pushes those controls out of the ticket.
fn field_width(ui: &egui::Ui, trailing: f32) -> f32 {
    (ui.available_width() - trailing).max(MIN_FIELD_W)
}

fn push(job: &mut LayoutJob, text: &str, font: FontId, color: Color32) {
    job.append(text, 0.0, TextFormat::simple(font, color));
}

/// The position's facts on one line: [`position_held`], then its P/L ([`position_pl`]).
fn position_facts(t: &Tokens, inputs: &TradeInputs<'_>) -> LayoutJob {
    let mut job = position_held(t, inputs);
    if let Some((pl, col)) = position_pl(t, inputs) {
        push(&mut job, &format!(" {pl}"), t.mono(TextRole::Body), col);
    }
    job
}

/// What the account holds: LONG or SHORT in its side's colour and the size at the average price;
/// FLAT when there is none. A fill changes it; a tick of the market never does.
fn position_held(t: &Tokens, inputs: &TradeInputs<'_>) -> LayoutJob {
    let mut job = LayoutJob::default();
    match inputs.position.filter(|p| p.size != 0.0) {
        None => push(&mut job, "FLAT", t.font(TextRole::Strong), t.theme.text3),
        Some(p) => {
            let (word, col) = if p.size > 0.0 {
                ("LONG", t.market.up_text)
            } else {
                ("SHORT", t.market.down_text)
            };
            push(&mut job, word, t.font(TextRole::Strong), col);
            let at = format!(
                " {} @ {}",
                sizing::qty_text(p.size.abs(), inputs.grid.lot),
                ladder::fmt_px(p.avg_px, inputs.grid.tick)
            );
            push(&mut job, &at, t.mono(TextRole::Body), t.theme.text);
        }
    }
    job
}

/// The position's unrealized P/L and its colour, or `None` while flat. It moves with every tick of
/// the mark, and gains or loses a digit at ±10, ±100, ±1,000.
fn position_pl(t: &Tokens, inputs: &TradeInputs<'_>) -> Option<(String, Color32)> {
    let p = inputs.position.filter(|p| p.size != 0.0)?;
    let col = if p.upnl >= 0.0 { t.market.up_text } else { t.market.down_text };
    Some((format!("{:+.2} {}", p.upnl, inputs.quote), col))
}

fn is_flat(inputs: &TradeInputs<'_>) -> bool {
    inputs.position.is_none_or(|p| p.size == 0.0)
}

/// Close and Reverse, `reversed` for a right-to-left row (so they still read Close, Reverse).
fn close_reverse(
    ui: &mut egui::Ui,
    key: egui::Id,
    state: &mut TradeState,
    inputs: &TradeInputs<'_>,
    actions: &mut Vec<TradeAction>,
    reversed: bool,
) {
    let mut buttons = [
        ("Close", "No position to close.", TradeAction::ClosePosition, "close"),
        ("Reverse", "No position to reverse.", TradeAction::Reverse, "reverse"),
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
            None if matches!(act, TradeAction::Reverse) && reverse_blind => {
                Some(NO_MARKET_PRICE_WHY.to_string())
            }
            None => None,
        };
        let b = match &why {
            Some(w) => ActionButton::secondary(words).disabled_because(w),
            None => ActionButton::secondary(words),
        };
        if keyed(ui, key.with(role), b).clicked() {
            // Held, it names the size it sends and goes if the position changes before the
            // confirm (final review A, minor 3).
            let size = inputs.position.map_or(0.0, |p| p.size);
            state.submit_exit(act, size, actions);
        }
    }
}

/// The cancel row's three buttons' words: `Cancel all N`, or `Cancel all` with `counted` false (on
/// a node that cannot attribute an order, Ruling R9, and where the count does not fit the compact
/// ticket's one row), `Bids`, `Asks`.
fn cancel_words(inputs: &TradeInputs<'_>, counted: bool) -> [String; 3] {
    let all = match inputs.orders_why {
        None if counted => format!("Cancel all {}", inputs.orders.len()),
        _ => "Cancel all".to_string(),
    };
    [all, "Bids".to_string(), "Asks".to_string()]
}

/// How the cancel row fills rows `width` wide ([`pack`]): its three buttons, then the padlock.
fn cancel_rows(
    ui: &egui::Ui,
    t: &Tokens,
    inputs: &TradeInputs<'_>,
    counted: bool,
    width: f32,
) -> Vec<usize> {
    let words = cancel_words(inputs, counted);
    let mut widths: Vec<f32> = words.iter().map(|w| button_w(ui.ctx(), t, w)).collect();
    widths.push(t.metrics.control_h);
    pack(&widths, t.metrics.gap, width)
}

/// Cancel all, Bids and Asks, then the one-click padlock, in `rows` ([`cancel_rows`]). A cancel is
/// sent at once, never held. On a node that cannot say which account an order belongs to
/// (`TradeInputs::orders_why`, Ruling R9) every cancel is disabled with THAT reason and claims no
/// count (pre-flight Minor 15). Where `counted` is false and the count is known, it is on hover.
fn cancel_row(
    ui: &mut egui::Ui,
    key: egui::Id,
    state: &mut TradeState,
    inputs: &TradeInputs<'_>,
    actions: &mut Vec<TradeAction>,
    rows: &[usize],
    counted: bool,
) {
    let bids = inputs.orders.iter().filter(|o| o.side > 0).count();
    let asks = inputs.orders.iter().filter(|o| o.side < 0).count();
    let words = cancel_words(inputs, counted);
    let cancels = [
        (TradeAction::CancelAll, inputs.orders.len(), NO_ORDERS_WHY, "cancel_all"),
        (TradeAction::CancelSide(1), bids, NO_BIDS_WHY, "cancel_bids"),
        (TradeAction::CancelSide(-1), asks, NO_ASKS_WHY, "cancel_asks"),
    ];
    in_rows(ui, rows, |ui, i| {
        let Some((act, n, none, role)) = cancels.get(i) else {
            padlock(ui, key, state, actions);
            return;
        };
        let why = inputs.orders_why.or((*n == 0).then_some(*none));
        let b = match why {
            Some(w) => ActionButton::secondary(words[i].as_str()).disabled_because(w),
            None => ActionButton::secondary(words[i].as_str()),
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

/// The one-click padlock: PRESSED while one-click trading is on, the state its name states (the
/// approved prototype could not be read here; spec §3.6 and the toggle's own name decide it,
/// pre-flight Minor 17). Changing it lets go of a held order, and says so as the strip's Cancel
/// does, "Not sent." (final review A, minor 4).
fn padlock(
    ui: &mut egui::Ui,
    key: egui::Id,
    state: &mut TradeState,
    actions: &mut Vec<TradeAction>,
) {
    let (icon, tip) = if state.one_click {
        (icons::ONE_CLICK_ON, ONE_CLICK_ON_TIP)
    } else {
        (icons::ONE_CLICK_OFF, ONE_CLICK_OFF_TIP)
    };
    let lock = IconButton::new(icon, tip).selected(state.one_click);
    if keyed(ui, key.with("one_click"), lock).clicked() {
        state.one_click = !state.one_click;
        if state.held.take().is_some() {
            let text = super::status::NOT_SENT.to_string();
            actions.push(TradeAction::Note { kind: StatusKind::Info, text });
        }
    }
}

/// The unit switch. It converts the size at [`reference_price`], or, with no size to convert,
/// empties the field: a number never silently changes what it means. With no price to convert at
/// it is disabled.
fn swap_units(
    ui: &mut egui::Ui,
    state: &mut TradeState,
    inputs: &TradeInputs<'_>,
    order_type: OrderType,
) {
    let (to, words) = match state.unit {
        SizeUnit::Base => (SizeUnit::Quote, inputs.quote),
        SizeUnit::Quote => (SizeUnit::Base, inputs.base),
    };
    let tip = format!("Enter the size in {words} instead");
    let at = reference_price(state, order_type, inputs);
    let b = IconButton::new(icons::SWAP_UNITS, &tip);
    let b = if at.is_some() { b } else { b.disabled_because(NO_CONVERT_WHY) };
    if ui.add(b).clicked() {
        let base = sizing::base_qty(&state.size, state.unit, at, inputs.grid.lot);
        state.unit = to;
        match base {
            Some(q) => write_size(state, q, at, inputs.grid.lot),
            None => state.size.clear(),
        }
    }
}

/// The size field in its unit, for a row that leaves `trailing` points after it.
fn size_field(ui: &mut egui::Ui, state: &mut TradeState, inputs: &TradeInputs<'_>, trailing: f32) {
    let unit = match state.unit {
        SizeUnit::Base => inputs.base,
        SizeUnit::Quote => inputs.quote,
    };
    ui.spacing_mut().text_edit_width = field_width(ui, trailing);
    input::number(
        ui,
        &mut state.size,
        input::Field { hint: "0", unit: Some(unit), ..Default::default() },
    );
}

/// The five quick sizes, lot multiples (spec §3.6), in rows `ui`'s width wide: every one, wrapped
/// onto as many rows as they take, or, with `one_row` (the compact ticket's fixed row), as many as
/// fit on one — the rest are the full ticket's. While the lot is not known each is disabled and says
/// why, rather than reading `0`; in the quote currency with no price to convert at, too.
fn quick_sizes(
    ui: &mut egui::Ui,
    t: &Tokens,
    state: &mut TradeState,
    inputs: &TradeInputs<'_>,
    order_type: OrderType,
    one_row: bool,
) {
    let lot = inputs.grid.lot;
    let at = reference_price(state, order_type, inputs);
    let why = if !known(lot) {
        Some(no_lot_reason(inputs.symbol))
    } else if state.unit == SizeUnit::Quote && at.is_none() {
        Some(NO_CONVERT_WHY.to_string())
    } else {
        None
    };
    let sizes = sizing::quick_sizes(inputs.grid);
    let words =
        sizes.map(|q| if known(q) { sizing::qty_text(q, lot) } else { NO_SIZE.to_string() });
    let widths: Vec<f32> = words.iter().map(|w| button_w(ui.ctx(), t, w)).collect();
    let mut rows = pack(&widths, t.metrics.gap, ui.available_width());
    if one_row {
        rows.truncate(1);
    }
    in_rows(ui, &rows, |ui, i| {
        let b = match &why {
            Some(w) => ActionButton::secondary(words[i].as_str()).disabled_because(w),
            None => ActionButton::secondary(words[i].as_str()),
        };
        if ui.add(b).clicked() {
            write_size(state, sizes[i], at, lot);
        }
    });
}

/// The order's value in the quote currency, at [`reference_price`]: `≈ 1,000.00 USDT`.
fn value_text(state: &TradeState, inputs: &TradeInputs<'_>, order_type: OrderType) -> String {
    let lot = inputs.grid.lot;
    let at = reference_price(state, order_type, inputs);
    let value = sizing::base_qty(&state.size, state.unit, at, lot).zip(at).map(|(q, p)| q * p);
    let value = value.map_or_else(|| NO_SIZE.to_string(), |v| fmt_thousands_prec(v, 2));
    format!("≈ {value} {}", inputs.quote)
}

/// The largest size the free buying power allows at [`reference_price`]: `max 100.000 BTC`. A
/// KNOWN zero says so — `max 0 BTC · no buying power`, `max 0 BTC · under one lot` — and the dash
/// is only for a max nobody can compute (no price to divide by, no lot, a buying power that is
/// not a number): a dash reads "not known", and hid a known zero behind it (the F wave's review,
/// minor 3). `None` while the buying power is not known — never while a price merely moves, so
/// whether the line is there does not change with a tick.
fn max_text(state: &TradeState, inputs: &TradeInputs<'_>, order_type: OrderType) -> Option<String> {
    let (lot, base) = (inputs.grid.lot, inputs.base);
    let at = reference_price(state, order_type, inputs);
    let bp = inputs.buying_power?;
    if bp.is_finite() && bp <= 0.0 {
        return Some(format!("max 0 {base} · no buying power"));
    }
    let max = match at.filter(|_| known(lot) && bp.is_finite()) {
        Some(p) => match sizing::share_of_buying_power(100.0, bp, p, lot) {
            Some(m) => sizing::qty_text(m, lot),
            None => return Some(format!("max 0 {base} · under one lot")),
        },
        None => NO_SIZE.to_string(),
    };
    Some(format!("max {max} {base}"))
}

/// The shares of the free buying power the ticket offers, in percent.
const PCTS: [f64; 4] = [10.0, 25.0, 50.0, 100.0];

/// The shares' buttons' words: `10%`, `25%`, `50%`, `100%`.
fn share_words() -> [String; 4] {
    PCTS.map(|pct| format!("{pct:.0}%"))
}

/// 10 %, 25 %, 50 % and 100 % of the free buying power, each disabled with its own reason, in as
/// many rows of `ui`'s width as they take.
fn shares(
    ui: &mut egui::Ui,
    t: &Tokens,
    state: &mut TradeState,
    inputs: &TradeInputs<'_>,
    order_type: OrderType,
) {
    let lot = inputs.grid.lot;
    let at = reference_price(state, order_type, inputs);
    let words = share_words();
    let widths: Vec<f32> = words.iter().map(|w| button_w(ui.ctx(), t, w)).collect();
    let rows = pack(&widths, t.metrics.gap, ui.available_width());
    in_rows(ui, &rows, |ui, i| {
        let q = inputs
            .buying_power
            .zip(at)
            .and_then(|(bp, p)| sizing::share_of_buying_power(PCTS[i], bp, p, lot));
        let why = if !known(lot) {
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
        let b = match &why {
            Some(w) => ActionButton::secondary(words[i].as_str()).disabled_because(w),
            None => ActionButton::secondary(words[i].as_str()),
        };
        if ui.add(b).clicked()
            && let Some(q) = q
        {
            write_size(state, q, at, lot);
        }
    });
}

/// Post only and Leverage's words: the full ticket's, or the compact ticket's shorter `Post`.
fn greyed_words(post_word: &str) -> [&str; 2] {
    [post_word, "Leverage"]
}

/// How wide Post only and Leverage are drawn together, a gap apart.
fn greyed_w(ui: &egui::Ui, t: &Tokens, post_word: &str) -> f32 {
    let [a, b] = greyed_words(post_word).map(|w| button_w(ui.ctx(), t, w));
    a + t.metrics.gap + b
}

/// Post only and Leverage, drawn and disabled with the owner's words (spec §3.7), in `rows`
/// ([`pack`]). Neither takes a click, so neither ever produces an intent (spec §7).
fn greyed(ui: &mut egui::Ui, post_word: &str, rows: &[usize]) {
    let words = greyed_words(post_word);
    let why = [POST_WHY, LEVERAGE_WHY];
    in_rows(ui, rows, |ui, i| {
        ui.add(ActionButton::secondary(words[i]).disabled_because(why[i]));
    });
}

/// The TP/SL toggle. `Some` with its response where TP/SL can be used. Where it cannot (I-3,
/// ruled): while the trader has it TICKED it stays ticked and enabled, so it can be turned off,
/// and the order is refused until it is ([`order`]; the caller writes the reason beside it); once
/// it is off it is disabled and says why on hover. In a window that takes no order at all
/// ([`why_untradable`]) it is an order control like any other: disabled, showing the trader's
/// setting, with the window's reason on hover (W3 review minor 7).
fn tpsl_toggle(
    ui: &mut egui::Ui,
    state: &mut TradeState,
    inputs: &TradeInputs<'_>,
    order_type: OrderType,
    label: &str,
) -> Option<egui::Response> {
    if let Some(why) = why_untradable(inputs) {
        let mut shown = state.tpsl;
        let r = ui.add_enabled_ui(false, |ui| toggle::checkbox(ui, &mut shown, label)).inner;
        let _ = r.on_disabled_hover_text(why);
        return None;
    }
    match tpsl_block(state, inputs, order_type) {
        None => Some(toggle::checkbox(ui, &mut state.tpsl, label)),
        Some(why) if state.tpsl => {
            let r = toggle::checkbox(ui, &mut state.tpsl, label);
            let _ = r.on_hover_text(tpsl_refusal(why));
            None
        }
        Some(why) => {
            let mut off = false;
            let r = ui.add_enabled_ui(false, |ui| toggle::checkbox(ui, &mut off, label)).inner;
            let _ = r.on_disabled_hover_text(why);
            None
        }
    }
}

/// The reason TP/SL, ticked, refuses this ticket's orders, or `None` when it does not. A window
/// that takes no order at all says THAT instead ([`why_untradable`]): turning TP/SL off would send
/// nothing there either.
fn tpsl_refused(
    state: &TradeState,
    inputs: &TradeInputs<'_>,
    order_type: OrderType,
) -> Option<String> {
    let trades = matches!(inputs.tradable, Tradable::Yes);
    tpsl_block(state, inputs, order_type).filter(|_| state.tpsl && trades).map(tpsl_refusal)
}

/// The exits TP/SL puts on an order, for a buy and for a sell (spec §3.6, "with the resulting
/// prices shown"): a PREVIEW, asked of [`order`]'s own arithmetic with TP/SL ON whatever the toggle
/// says, so the compact toggle's hover shows what turning it on would set and no line can show a
/// price the order would not carry. Where the order itself cannot be built (no size, a price off
/// the tick, no price at all) the line says why instead.
fn tpsl_lines(state: &TradeState, inputs: &TradeInputs<'_>, order_type: OrderType) -> [String; 2] {
    [1, -1].map(|side| {
        let verb = if side > 0 { "Buy" } else { "Sell" };
        match order_with(side, order_type, true, state, inputs) {
            Ok(TradeAction::Place { exits: Some(e), .. }) => format!(
                "{verb}: TP {} · SL {}",
                ladder::fmt_px(e.take_profit, inputs.grid.tick),
                ladder::fmt_px(e.stop_loss, inputs.grid.tick)
            ),
            // `order_with` with TP/SL on answers exits or a refusal; this arm is never taken.
            Ok(_) => format!("{verb}: {TPSL_ROOM_WHY}"),
            Err(why) => format!("{verb}: {why}"),
        }
    })
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
fn send_rows(
    ui: &egui::Ui,
    t: &Tokens,
    key: egui::Id,
    state: &TradeState,
    inputs: &TradeInputs<'_>,
    entry: Entry<'_>,
    width: f32,
) -> Vec<usize> {
    let label = |side| side_label(side, entry.order_type, state, inputs, entry.market_word);
    let [buy, sell] = [1, -1].map(|side| button_w(ui.ctx(), t, &label(side)));
    let memory = key.with("trade_send_stacked");
    let was = ui.data(|d| d.get_temp::<bool>(memory)).unwrap_or(false);
    let digits = button_w(ui.ctx(), t, "00") - 2.0 * t.metrics.pad;
    let stacked = buy + t.metrics.gap + sell + if was { digits } else { 0.0 } > width;
    ui.data_mut(|d| d.insert_temp(memory, stacked));
    if stacked { vec![1, 1] } else { vec![2] }
}

/// Which Buy and Sell a ticket draws: the entry they send, and the word a market one is named by
/// (`market` in the full ticket, `MKT` in the compact one).
#[derive(Clone, Copy)]
struct Entry<'w> {
    order_type: OrderType,
    market_word: &'w str,
}

/// The Buy and Sell buttons for `entry`, in `rows` ([`send_rows`]): each names the order
/// [`order_as`] builds, and is disabled with its reason when it refuses.
fn send_buttons(
    ui: &mut egui::Ui,
    key: egui::Id,
    state: &mut TradeState,
    inputs: &TradeInputs<'_>,
    actions: &mut Vec<TradeAction>,
    entry: Entry<'_>,
    rows: &[usize],
) {
    let Entry { order_type, market_word } = entry;
    in_rows(ui, rows, |ui, i| {
        let (side, role) = if i == 0 { (1, "buy") } else { (-1, "sell") };
        let label = side_label(side, order_type, state, inputs, market_word);
        let made = order_as(side, order_type, state, inputs);
        let base = if side > 0 {
            ActionButton::buy(label.as_str())
        } else {
            ActionButton::sell(label.as_str())
        };
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
/// and Ask only TYPE that price into the field; they send nothing.
fn price_row(ui: &mut egui::Ui, t: &Tokens, state: &mut TradeState, inputs: &TradeInputs<'_>) {
    let hint = if state.order_type == OrderType::Stop { "Trigger" } else { "Price" };
    let trailing =
        button_w(ui.ctx(), t, "Bid") + button_w(ui.ctx(), t, "Ask") + 2.0 * t.metrics.gap;
    ui.spacing_mut().text_edit_width = field_width(ui, trailing);
    input::number(
        ui,
        &mut state.price,
        input::Field { hint, unit: Some(inputs.quote), ..Default::default() },
    );
    let (bid, ask) = best(inputs);
    for (words, px) in [("Bid", bid), ("Ask", ask)] {
        let b = match px {
            Some(_) => ActionButton::secondary(words),
            None => ActionButton::secondary(words).disabled_because(NO_SIDE_WHY),
        };
        if ui.add(b).clicked()
            && let Some(p) = px
        {
            state.price = price_text(p, inputs);
        }
    }
}

/// The TP/SL toggle and, while it is on and allowed, its two distances in percent.
fn tpsl_row(
    ui: &mut egui::Ui,
    t: &Tokens,
    state: &mut TradeState,
    inputs: &TradeInputs<'_>,
    order_type: OrderType,
) {
    if tpsl_toggle(ui, state, inputs, order_type, "TP/SL").is_some() && state.tpsl {
        ui.spacing_mut().text_edit_width =
            ((ui.available_width() - t.metrics.gap) / 2.0).max(MIN_FIELD_W);
        input::number(
            ui,
            &mut state.tp_text,
            input::Field { hint: "TP", unit: Some("% TP"), ..Default::default() },
        );
        input::number(
            ui,
            &mut state.sl_text,
            input::Field { hint: "SL", unit: Some("% SL"), ..Default::default() },
        );
    }
}

fn caption(ui: &mut egui::Ui, t: &Tokens, text: impl Into<String>) {
    ui.label(caption_text(t, text));
}

/// A caption's font: the mono Caption role.
fn caption_font(t: &Tokens) -> FontId {
    t.mono(TextRole::Caption)
}

/// A caption's words, in [`caption_font`] and the third text colour.
fn caption_text(t: &Tokens, text: impl Into<String>) -> RichText {
    RichText::new(text).font(caption_font(t)).color(t.theme.text3)
}

/// A caption on a line of its own, ONE row high whatever it says (cut, with its whole text on
/// hover, only where it alone is wider than its row): for a number that moves with the market,
/// whose next digit must never move what is under it.
fn caption_line(ui: &mut egui::Ui, t: &Tokens, text: impl Into<String>) {
    ui.add(egui::Label::new(caption_text(t, text)).truncate());
}

/// A line that says why, in `colour`, cut to the row with the whole text on hover (egui shows an
/// elided label's text on hover): for a fixed row that has room for one line.
fn reason_line(ui: &mut egui::Ui, t: &Tokens, words: &str, colour: Color32) {
    let text = RichText::new(words).font(t.font(TextRole::Body)).color(colour);
    ui.add(egui::Label::new(text).truncate());
}

/// A child of `ui` laid out top to bottom in `rect` and clipped to it, its id `salt` under `ui`'s.
/// It keeps `ui`'s spacing.
fn part(ui: &mut egui::Ui, rect: Rect, salt: &str) -> egui::Ui {
    let mut child = ui.new_child(
        UiBuilder::new().id_salt(salt).max_rect(rect).layout(Layout::top_down(Align::Min)),
    );
    child.shrink_clip_rect(rect);
    child
}

/// What a screen reader calls the full ticket's scrolling part (`trade_fit.rs` reads it too).
pub const FORM_NAME: &str = "Order form";
/// How the full ticket's Buy and Sell name a market order.
const MARKET: &str = "market";

/// The full ticket (beside the ladder, or alone): a form that scrolls, and under it, PINNED, Buy,
/// Sell and the cancel row.
///
/// ⚠ Buy, Sell and the cancel row are never inside the scroll (W3 review hand-off 1): they are what
/// the ticket is for, and the last rows of a scrolling form are the first a loose look or a short
/// window pushes out of view. Their rows are counted BEFORE the form is drawn, from the labels as
/// this frame found them — so the form knows its height — and Buy and Sell stack, one to a row,
/// where both labels do not fit side by side (a label names the order and is never cut). An edit
/// in the form that changes how they stack is drawn the next frame, which it asks for.
pub(super) fn full(
    ui: &mut egui::Ui,
    t: &Tokens,
    state: &mut TradeState,
    inputs: &TradeInputs<'_>,
    actions: &mut Vec<TradeAction>,
) {
    let area = ui.available_rect_before_wrap();
    let (gap, control_h) = (t.metrics.gap, t.metrics.control_h);
    // Every order and cancel button is keyed under the ticket's own id ([`keyed`]).
    let key = ui.id();
    ui.spacing_mut().item_spacing = egui::vec2(gap, gap);
    let rows_now = |ui: &egui::Ui, state: &TradeState| {
        let entry = Entry { order_type: state.order_type, market_word: MARKET };
        let send = send_rows(ui, t, key, state, inputs, entry, area.width());
        (send, cancel_rows(ui, t, inputs, true, area.width()))
    };
    let (send, cancel) = rows_now(ui, state);
    let n = (send.len() + cancel.len()) as f32;
    let top = (area.max.y - (n * control_h + (n - 1.0) * gap)).max(area.min.y);
    let form = Rect::from_min_max(area.min, pos2(area.max.x, (top - gap).max(area.min.y)));
    let mut form_ui = part(ui, form, "trade_ticket_form");
    name_group(&form_ui, form, FORM_NAME);
    let scroll = egui::ScrollArea::vertical()
        .id_salt("trade_ticket_full")
        .auto_shrink([false, false])
        .show(&mut form_ui, |ui| form_rows(ui, t, key, state, inputs, actions));
    // A form that goes on below what it shows SAYS so (ruling B2 of the render check, 2026-10-03):
    // egui's floating scroll bar is invisible until hovered, and Buy and Sell name only side, size
    // and type, so a ticked Reduce only scrolled out of view showed nowhere. A divider across the
    // ticket, in the gap between the form and the rows pinned under it: painted, never laid out,
    // so it moves no widget and no id.
    if hidden_below(scroll.content_size.y, scroll.state.offset.y, scroll.inner_rect.height()) {
        let y = ui.painter().round_to_pixel_center(form.max.y + gap / 2.0);
        ui.painter().hline(area.x_range(), y, egui::Stroke::new(1.0, t.theme.border));
    }
    let (send_now, cancel_now) = rows_now(ui, state);
    if send_now != send || cancel_now != cancel {
        ui.ctx().request_repaint();
    }
    let mut pinned = part(ui, Rect::from_min_max(pos2(area.min.x, top), area.max), "trade_pinned");
    let entry = Entry { order_type: state.order_type, market_word: MARKET };
    send_buttons(&mut pinned, key, state, inputs, actions, entry, &send);
    cancel_row(&mut pinned, key, state, inputs, actions, &cancel, true);
}

/// Whether a scroll area's content goes on below what it shows: content `content_h` tall, scrolled
/// `offset` down, in a view `visible_h` tall. Half a point of slack, egui's rounding, so a form
/// that fits exactly, or one scrolled to its end, says nothing.
fn hidden_below(content_h: f32, offset: f32, visible_h: f32) -> bool {
    content_h - (offset + visible_h) > 0.5
}

/// Post only's words in the full ticket.
const POST_ONLY: &str = "Post only";

/// The rows the quick sizes are given where a window is sized to show its whole form
/// ([`opening_h`]): two, the most the five multiples of a lot with up to five decimals (`0.00001`
/// to `0.00100`) take across the full ticket in any look (`trade_fit.rs`'s spot content holds it).
/// A coarser lot may need only one, and a window opens before it has an instrument to measure; a
/// finer lot makes the form scroll, and the divider under it says so.
const QUICK_ROWS: f32 = 2.0;

/// The rows pinned under the form in the state a window opens in: Buy and Sell side by side, then
/// the cancel row with the padlock. Where Buy and Sell stack (a long size) or the cancel row wraps,
/// they take a row from the form, which then scrolls, and says so.
const PINNED_ROWS: f32 = 2.0;

/// How tall the full ticket is, `width` wide, with its WHOLE form in view, in the state a window
/// opens in (ruling B1 of the render check, 2026-10-03): the form, section by section in
/// [`form_rows`]' order and a gap apart, then the gap and the rows pinned under it, as [`full`]
/// lays them out. The state is a fresh ticket on an account that trades: a Limit (so a price
/// row), nothing ticked, a position held (the taller of the two lines the position section can
/// show) and the buying power known (the max line). A control row is the kit's `control_h`, a line
/// of text the label its font draws ([`label_h`]), and a row of buttons wraps by [`pack`] over
/// [`button_w`], as the form wraps it: the form's own sizes, read where it reads them.
/// `layout::window_size` puts the instrument bar, the strip and the window around it.
pub(super) fn opening_h(ctx: &egui::Context, t: &Tokens, width: f32) -> f32 {
    let (gap, control_h) = (t.metrics.gap, t.metrics.control_h);
    let line = |font: FontId| label_h(ctx, &font, t);
    let rows = |n: f32| n * control_h + (n - 1.0) * gap;
    let wrapped = |words: &[&str]| {
        let widths: Vec<f32> = words.iter().map(|w| button_w(ctx, t, w)).collect();
        rows(pack(&widths, gap, width).len() as f32)
    };
    let shares = share_words();
    // [`position_held`]: the side's word and the size at the price; under it the P/L
    // ([`position_pl`]), or the flat caption.
    let held = line(t.font(TextRole::Strong)).max(line(t.mono(TextRole::Body)));
    let under_held = line(t.mono(TextRole::Body)).max(line(t.font(TextRole::Caption)));
    let sections = [
        held + gap + under_held,
        control_h,                                       // Close and Reverse
        control_h,                                       // the order type
        control_h,                                       // the price
        control_h + 2.0 * (gap + line(caption_font(t))), // the size, its value and its max
        rows(QUICK_ROWS),
        wrapped(&shares.each_ref().map(String::as_str)[..]),
        control_h, // Reduce only
        wrapped(&greyed_words(POST_ONLY)[..]),
        control_h, // TP/SL
    ];
    let form = sections.iter().sum::<f32>() + (sections.len() - 1) as f32 * gap;
    form + gap + rows(PINNED_ROWS)
}

/// The full ticket's form, top to bottom: the position, Close and Reverse, why the window takes no
/// order (when it does not), the order type, the price, a wrong-side Stop's warning (when there is
/// one), the size with its value, the quick sizes, the shares of the buying power, Reduce only,
/// Post only and Leverage, and TP/SL with the exits it sets or why it cannot. Each in its own
/// [`section`].
fn form_rows(
    ui: &mut egui::Ui,
    t: &Tokens,
    key: egui::Id,
    state: &mut TradeState,
    inputs: &TradeInputs<'_>,
    actions: &mut Vec<TradeAction>,
) {
    let gap = t.metrics.gap;
    ui.spacing_mut().item_spacing = egui::vec2(gap, gap);
    section(ui, key, "form_position", |ui| {
        // The position WRAPS here, where the form has the height for it (W4 fix round 1: cut to
        // one line, an eight-digit average price lost the P/L at every width of a 260 pt ticket).
        // Only a fill changes what it says.
        ui.add(egui::Label::new(position_held(t, inputs)).wrap());
        match position_pl(t, inputs) {
            // The P/L moves with every tick and gains a digit at ±10, ±100, ±1,000: on a line of
            // its own, one row high whatever it says, a tick never moves the form under the
            // trader's pointer (W4 fix round 2, N2). Cut only where the P/L alone is wider than
            // the form; egui shows a cut label whole on hover.
            Some((pl, col)) => {
                let pl = RichText::new(pl).font(t.mono(TextRole::Body)).color(col);
                ui.add(egui::Label::new(pl).truncate());
            }
            None => {
                ui.label(
                    RichText::new("No position on this account")
                        .font(t.font(TextRole::Caption))
                        .color(t.theme.text3),
                );
            }
        }
    });
    section(ui, key, "form_close", |ui| {
        ui.horizontal(|ui| close_reverse(ui, key, state, inputs, actions, false));
    });
    if let Some(why) = why_untradable(inputs) {
        section(ui, key, "form_untradable", |ui| {
            ui.label(RichText::new(why).font(t.font(TextRole::Body)).color(t.theme.text2));
        });
    }
    section(ui, key, "form_type", |ui| {
        segmented::segmented(
            ui,
            &mut state.order_type,
            &[
                segmented::Segment {
                    value: OrderType::Limit,
                    label: "Limit",
                    why: "A limit order at your price",
                },
                segmented::Segment {
                    value: OrderType::Market,
                    label: "Market",
                    why: "Fills now at the best price",
                },
                segmented::Segment {
                    value: OrderType::Stop,
                    label: "Stop",
                    why: "A market order when the trigger trades",
                },
            ],
        );
    });
    let order_type = state.order_type;
    if order_type != OrderType::Market {
        section(ui, key, "form_price", |ui| {
            ui.horizontal(|ui| price_row(ui, t, state, inputs));
        });
    }
    // A Stop whose trigger would fire at once says so before the click, a line for each side it
    // would (both, for a trigger inside the spread), in the warning colour: written, not hovered.
    let warnings: Vec<String> = match (order_type, typed_price(state)) {
        (OrderType::Stop, Some(trigger)) => {
            [1, -1].into_iter().filter_map(|side| stop_warning(side, trigger, inputs)).collect()
        }
        _ => Vec::new(),
    };
    if !warnings.is_empty() {
        section(ui, key, "form_stop_warning", |ui| {
            for words in warnings {
                let words = RichText::new(words).font(t.font(TextRole::Body));
                ui.label(words.color(Status::Warning.color()));
            }
        });
    }
    section(ui, key, "form_size", |ui| {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Size").font(t.font(TextRole::Caption)).color(t.theme.text3));
            size_field(ui, state, inputs, t.metrics.control_h + gap);
            swap_units(ui, state, inputs, order_type);
        });
        // The value and the max, each on a row of its own. A Market order's value moves with every
        // tick and gains a digit at 1,000, 10,000, …: one WRAPPING line holding both moved
        // everything under it by a line when a digit crossed the form's edge (the P/L's sibling,
        // W4 fix round 2's N2). Whether the max's row is there follows the buying power, which a
        // tick never changes.
        caption_line(ui, t, value_text(state, inputs, order_type));
        if let Some(max) = max_text(state, inputs, order_type) {
            caption_line(ui, t, max);
        }
    });
    section(ui, key, "form_quick", |ui| quick_sizes(ui, t, state, inputs, order_type, false));
    section(ui, key, "form_shares", |ui| shares(ui, t, state, inputs, order_type));
    section(ui, key, "form_reduce", |ui| {
        ui.horizontal(|ui| {
            toggle::checkbox(ui, &mut state.reduce_only, "Reduce only");
        });
    });
    section(ui, key, "form_greyed", |ui| {
        let greyed_widths = greyed_words(POST_ONLY).map(|w| button_w(ui.ctx(), t, w));
        let greyed_rows = pack(&greyed_widths, gap, ui.available_width());
        greyed(ui, POST_ONLY, &greyed_rows);
    });
    section(ui, key, "form_tpsl", |ui| {
        ui.horizontal(|ui| tpsl_row(ui, t, state, inputs, order_type));
    });
    if let Some(refusal) = tpsl_refused(state, inputs, order_type) {
        section(ui, key, "form_tpsl_refused", |ui| {
            // Written, not only hovered: the trader must see why Buy and Sell send nothing.
            ui.label(
                RichText::new(refusal).font(t.font(TextRole::Body)).color(Status::Warning.color()),
            );
        });
    } else if state.tpsl && why_untradable(inputs).is_none() {
        section(ui, key, "form_tpsl_exits", |ui| {
            for line in tpsl_lines(state, inputs, order_type) {
                caption(ui, t, line);
            }
        });
    }
}

/// The compact ticket (under the ladder), at most seven fixed rows (`layout::under_ticket_h` counts
/// them): the position strip, the size with its value, the quick sizes, the toggles, Buy and Sell at
/// market (a row each where both labels do not fit side by side), and the cancel row. Limit and stop
/// orders come from the ladder (spec §3.6). The TP/SL distances are the full ticket's; the toggle's
/// hover says what they are and where they put the exits.
///
/// Seven rows have no room to wrap, so where a row's content does not fit its width (a narrow
/// window, a loose look), the row gives up what the full ticket also offers — measured, never cut:
/// - the quick sizes show as many as fit;
/// - Post and Leverage, greyed placeholders, are left out where the toggles leave no room (and a
///   ticked TP/SL that refuses the order writes "Turn TP/SL off to send." in their place);
/// - `Cancel all N` drops its count where the row cannot hold it (the count is then on hover);
/// - "Turn TP/SL off to send." says "Turn TP/SL off." where the row cannot hold it;
/// - in a window that takes no order, the toggles' row says why instead ([`why_untradable`]):
///   there is no hint line under the ladder in this layout, so the reason is written here.
pub(super) fn compact(
    ui: &mut egui::Ui,
    t: &Tokens,
    state: &mut TradeState,
    inputs: &TradeInputs<'_>,
    actions: &mut Vec<TradeAction>,
) {
    let gap = t.metrics.gap;
    // Every order and cancel button is keyed under the ticket's own id ([`keyed`]).
    let key = ui.id();
    ui.spacing_mut().item_spacing = egui::vec2(gap, gap);
    ui.add_space(t.metrics.pad);
    ui.horizontal(|ui| {
        // Close and Reverse first, from the right edge, so long facts are cut short beside them
        // rather than lying under them.
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            close_reverse(ui, key, state, inputs, actions, true);
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                ui.add(egui::Label::new(position_facts(t, inputs)).truncate());
            });
        });
    });
    ui.horizontal(|ui| {
        // The field takes two fifths of the row; the unit switch and the value share the rest.
        let trailing = ui.available_width() * 0.6;
        size_field(ui, state, inputs, trailing);
        swap_units(ui, state, inputs, OrderType::Market);
        caption_line(ui, t, value_text(state, inputs, OrderType::Market));
    });
    quick_sizes(ui, t, state, inputs, OrderType::Market, true);
    ui.horizontal(|ui| match why_untradable(inputs) {
        Some(why) => {
            reason_line(ui, t, &why, t.theme.text2);
        }
        None => compact_toggles(ui, t, state, inputs),
    });
    let entry = Entry { order_type: OrderType::Market, market_word: "MKT" };
    let send = send_rows(ui, t, key, state, inputs, entry, ui.available_width());
    send_buttons(ui, key, state, inputs, actions, entry, &send);
    // One row, all four — none of them is something the full ticket alone offers — so where the
    // count does not fit (three digits of working orders, a narrow window), Cancel all drops it.
    let counted = cancel_rows(ui, t, inputs, true, ui.available_width()).len() == 1;
    cancel_row(ui, key, state, inputs, actions, &[4], counted);
}

/// The compact ticket's toggles: TP/SL (its hover previews the exits), Reduce, then Post and
/// Leverage where they fit — or, while a ticked TP/SL refuses the order, "Turn TP/SL off to send."
/// with the whole reason on hover.
fn compact_toggles(
    ui: &mut egui::Ui,
    t: &Tokens,
    state: &mut TradeState,
    inputs: &TradeInputs<'_>,
) {
    if let Some(r) = tpsl_toggle(ui, state, inputs, OrderType::Market, "TP/SL") {
        let [buy, sell] = tpsl_lines(state, inputs, OrderType::Market);
        let _ = r.on_hover_text(format!(
            "TP {} % and SL {} % from the entry, set in the full ticket.\n{buy}\n{sell}",
            state.tp_text.trim(),
            state.sl_text.trim()
        ));
    }
    toggle::checkbox(ui, &mut state.reduce_only, "Reduce");
    match tpsl_refused(state, inputs, OrderType::Market) {
        Some(refusal) => {
            // The words that fit the row, never cut (a 260 pt window cut the long ones at
            // Comfortable density and Large text): the whole reason is the hover.
            let font = t.font(TextRole::Caption);
            let fits = |w: &str| {
                let g = ui.painter().layout_no_wrap(w.to_string(), font.clone(), t.theme.text);
                g.size().x <= ui.available_width()
            };
            let said = if fits(TPSL_OFF_TO_SEND) { TPSL_OFF_TO_SEND } else { TPSL_OFF };
            let words = RichText::new(said).font(font.clone()).color(Status::Warning.color());
            let _ = ui.add(egui::Label::new(words).truncate()).on_hover_text(refusal);
        }
        None if greyed_w(ui, t, "Post") <= ui.available_width() => greyed(ui, "Post", &[2]),
        None => {}
    }
}

#[cfg(test)]
mod tests {
    use super::{hidden_below, pack};

    /// Ruling B2's condition: the divider is drawn while the form's content goes on below what it
    /// shows, and not where it fits, nor where the trader has scrolled it to its end.
    #[test]
    fn a_form_is_hidden_below_only_while_its_content_goes_on_past_the_view() {
        assert!(hidden_below(400.0, 0.0, 300.0), "100 pt below the view");
        assert!(hidden_below(400.0, 99.0, 300.0), "a point short of the end");
        assert!(!hidden_below(400.0, 100.0, 300.0), "scrolled to its end");
        assert!(!hidden_below(300.0, 0.0, 300.0), "it fits exactly");
        assert!(!hidden_below(300.4, 0.0, 300.0), "within egui's rounding");
        assert!(!hidden_below(200.0, 0.0, 300.0), "it fits with room to spare");
    }

    /// W4 fix round 1, M1: `pack` at its edges. A row that is filled EXACTLY holds its last item;
    /// a point over starts the next row; an item wider than any row still gets a row of its own
    /// (it is never dropped, and never shares one); nothing packs into no rows.
    #[test]
    fn pack_fills_a_row_exactly_and_never_drops_an_item() {
        // 40 + 6 + 40 + 6 + 40 = 132.
        assert_eq!(pack(&[40.0, 40.0, 40.0], 6.0, 132.0), [3], "an exact fit is one row");
        assert_eq!(pack(&[40.0, 40.0, 40.0], 6.0, 131.0), [2, 1], "a point over starts the next");
        assert_eq!(pack(&[300.0], 6.0, 132.0), [1], "an item wider than the row keeps its own row");
        assert_eq!(pack(&[40.0, 300.0, 40.0], 6.0, 132.0), [1, 1, 1], "and shares it with nothing");
        assert_eq!(pack(&[], 6.0, 132.0), Vec::<usize>::new(), "nothing packs into no rows");
    }
}
