//! The tickets (spec §3.6): the full one beside the ladder (or alone), and the compact one under
//! it, for a narrow window.
//!
//! # The look
//!
//! Both draw the owner's v3 design (its capture's geometry dumps, beside the ladder and under it,
//! hold the sizes): rows of CHOICES are equal buttons across the ticket — the
//! order types, the quick sizes, the shares, Buy and Sell, Close and Reverse ([`Fill::Equal`]);
//! toggles are buttons that read pressed, never check boxes (Reduce only, TP/SL, the padlock); the
//! position is a bordered card (the full ticket) or a strip (the compact one); Buy and Sell are tall.
//! The controls are [`controls::Btn`] and [`controls::number_field`], drawn from the kit's tokens.
//! Where the design has two shapes for a row, the ticket measures and takes the one that fits —
//! never cuts a word (`trade_fit.rs` holds that at every width, density and text size).
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

mod controls;

use egui::text::{LayoutJob, TextFormat};
use egui::{Align, Color32, FontId, Layout, Rect, RichText, UiBuilder, pos2};
use vike_ui_theme::components::{Status, Tokens, input};
use vike_ui_theme::fmt::fmt_thousands_prec;
use vike_ui_theme::icons;
use vike_ui_theme::metrics::{Metrics, RADIUS, stroke};
use vike_ui_theme::side::Pair;
use vike_ui_theme::type_scale::TextRole;
use vike_ui_theme::value::trade;

use self::controls::{Btn, Look, mono_semibold, number_field, semibold, tall_h};
use super::{
    Exits, OrderType, Origin, SizeUnit, StatusKind, Tradable, TradeAction, TradeInputs, TradeState,
    keyed, label_h, ladder, name_group, section, sizing, text_w, why_untradable,
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
/// ...and where not even those words fit its row; where not even these do (Large text in a 260 pt
/// window), a warning mark, with the same hover.
const TPSL_OFF: &str = "Turn TP/SL off.";
const NO_BIDS_WHY: &str = "No working buy orders on this symbol.";
const NO_ASKS_WHY: &str = "No working sell orders on this symbol.";
const NO_SIDE_WHY: &str = "No price on that side of the book.";
const NO_BUYING_POWER_WHY: &str = "The account's buying power is not known.";
const NO_PRICE_WHY: &str = "No price to size against yet.";
const NO_CONVERT_WHY: &str = "No price to convert the size at.";
/// What a quick size, a value or a size reads where there is none to show.
const NO_SIZE: &str = "—";
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

/// The Buy/Sell label's three runs, the order [`order_qty`] sizes: `Buy`, `0.010`, `limit` (or
/// `Sell`, `0.010`, `MKT`). The ticket draws the number in the mono face and names the button by
/// the three, a space apart.
fn side_words(
    side: i32,
    order_type: OrderType,
    state: &TradeState,
    inputs: &TradeInputs<'_>,
    market_word: &str,
) -> (String, String, String) {
    let verb = if side > 0 { "Buy" } else { "Sell" };
    let q = order_qty(side, order_type, state, inputs)
        .map_or_else(|| NO_SIZE.to_string(), |q| sizing::qty_text(q, inputs.grid.lot));
    let kind = match order_type {
        OrderType::Market => market_word,
        OrderType::Limit => "limit",
        OrderType::Stop => "stop",
    };
    (verb.to_string(), q, kind.to_string())
}

/// How many of `widths`, a gap apart and in order, each row `width` wide holds, filling a row before
/// starting the next: `[2]` is one row of two, `[1, 1]` two rows of one. Measured BEFORE anything is
/// drawn, so a row of buttons wraps where it must and its height is known in advance. egui's
/// `horizontal_wrapped` cannot wrap a button that is told its own width, so the last one ran past
/// the ticket's edge (MEASURED by the fit sweep: the fifth quick size at Comfortable density), and
/// every row after it inherited the widened width.
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

/// `rows` evened out: the same rows holding the same items, the last no lonelier than the first (five
/// quick sizes in two rows are three and two, not four and one). No row grows past what the packing
/// found would fit: they are at most `ceil(items / rows)`, and the first of those held more.
fn balance(rows: Vec<usize>) -> Vec<usize> {
    let (k, n) = (rows.len(), rows.iter().sum::<usize>());
    if k < 2 {
        return rows;
    }
    (0..k).map(|i| n / k + usize::from(i < n % k)).collect()
}

/// How the items of a packed row share the row's width.
#[derive(Clone, Copy)]
enum Fill {
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
fn row_widths(fill: Fill, first: usize, naturals: &[f32], gap: f32, avail: f32) -> Vec<f32> {
    let n = naturals.len();
    let gaps = gap * n.saturating_sub(1) as f32;
    let grows = match fill {
        Fill::Equal => return vec![((avail - gaps) / n.max(1) as f32).max(0.0); n],
        Fill::LastOfRow => n.checked_sub(1),
        Fill::Item(k) => k.checked_sub(first).filter(|i| *i < n),
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
fn in_rows(
    ui: &mut egui::Ui,
    t: &Tokens,
    rows: &[usize],
    naturals: &[f32],
    fill: Fill,
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
fn digits_btn<'a>(t: &Tokens, words: &str) -> Btn<'a> {
    Btn::new(words).digits(t, words).snug()
}

/// Whether something `needed` wide fits a row `avail` wide, REMEMBERED under `key` from frame to
/// frame so a width that moves by a digit at the boundary does not make the row flicker between
/// its two shapes: it stops fitting the moment it does not, and fits again only with `margin` to
/// spare ([`send_rows`] argues the same for Buy and Sell). A row never seen fits.
fn fits(ui: &egui::Ui, key: egui::Id, needed: f32, avail: f32, margin: f32) -> bool {
    let was = ui.data(|d| d.get_temp::<bool>(key)).unwrap_or(true);
    let now = needed + if was { 0.0 } else { margin } <= avail;
    ui.data_mut(|d| d.insert_temp(key, now));
    now
}

/// Two digits of the mono Caption face: the margin a remembered fit ([`fits`]) keeps.
fn two_digits(ctx: &egui::Context, t: &Tokens) -> f32 {
    text_w(ctx, "00", &caption_font(t), t)
}

/// A run of the card's facts: `line` high (the design's line height of 1, the text's own size, so a
/// line of facts is as high as the text it holds), the run centred in it.
fn push(job: &mut LayoutJob, text: &str, font: FontId, color: Color32, line: f32) {
    let format = TextFormat { font_id: font, color, line_height: Some(line), ..Default::default() };
    job.append(text, 0.0, format);
}

/// What the account holds: LONG or SHORT in its side's colour and the size at the average price;
/// FLAT when there is none. A fill changes it; a tick of the market never does.
fn position_held(t: &Tokens, inputs: &TradeInputs<'_>) -> LayoutJob {
    let mut job = LayoutJob::default();
    let side = semibold(t, TextRole::Caption);
    let line = t.text.px(TextRole::Body);
    match position_side(t, inputs) {
        None => push(&mut job, "FLAT", side, t.theme.text3, line),
        Some((word, col, facts)) => {
            push(&mut job, word, side, col, line);
            push(&mut job, &format!(" {facts}"), t.mono(TextRole::Body), t.theme.text, line);
        }
    }
    job
}

/// The held position as its two runs: the side's word in the side's colour, and the size at the
/// average price (`0.050 @ 65,410.2`); `None` while flat.
fn position_side(t: &Tokens, inputs: &TradeInputs<'_>) -> Option<(&'static str, Color32, String)> {
    let p = inputs.position.filter(|p| p.size != 0.0)?;
    let long = p.size > 0.0;
    let (word, col) = (if long { "LONG" } else { "SHORT" }, Pair::LongShort.text(long, t));
    let facts = format!(
        "{} @ {}",
        sizing::qty_text(p.size.abs(), inputs.grid.lot),
        ladder::fmt_px(p.avg_px, inputs.grid.tick)
    );
    Some((word, col, facts))
}

/// The position's unrealized P/L and its colour, or `None` while flat. It moves with every tick of
/// the mark, and gains or loses a digit at ±10, ±100, ±1,000.
fn position_pl(t: &Tokens, inputs: &TradeInputs<'_>) -> Option<(String, Color32)> {
    let p = inputs.position.filter(|p| p.size != 0.0)?;
    let col = Pair::GainLoss.text(p.upnl >= 0.0, t);
    Some((format!("{:+.2} {}", p.upnl, inputs.quote), col))
}

fn is_flat(inputs: &TradeInputs<'_>) -> bool {
    inputs.position.is_none_or(|p| p.size == 0.0)
}

/// Close and Reverse, `reversed` for a right-to-left row (so they still read Close, Reverse); each
/// `each` wide where the row fills its width, else as wide as its word.
#[allow(clippy::too_many_arguments)]
fn close_reverse(
    ui: &mut egui::Ui,
    t: &Tokens,
    key: egui::Id,
    state: &mut TradeState,
    inputs: &TradeInputs<'_>,
    actions: &mut Vec<TradeAction>,
    reversed: bool,
    each: Option<f32>,
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
        let b = Btn::text(t, words);
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
fn cancel_words(inputs: &TradeInputs<'_>, counted: bool) -> [String; 3] {
    let all = match cancel_count(inputs, counted) {
        Some(n) => format!("Cancel all {n}"),
        None => "Cancel all".to_string(),
    };
    [all, "Bids".to_string(), "Asks".to_string()]
}

/// The count Cancel all carries in its pill: the working orders, where the row claims one.
fn cancel_count(inputs: &TradeInputs<'_>, counted: bool) -> Option<usize> {
    (inputs.orders_why.is_none() && counted).then_some(inputs.orders.len())
}

/// The cancel row's four buttons as they are drawn — Cancel all, Bids, Asks, then the one-click
/// padlock — before any is given its reason or its width, so the row can measure them first.
fn cancel_btns<'a>(
    t: &Tokens,
    inputs: &TradeInputs<'_>,
    counted: bool,
    one_click: bool,
    snug: bool,
) -> [Btn<'a>; 4] {
    let words = cancel_words(inputs, counted);
    cancel_btns_of(t, &words[0], cancel_count(inputs, counted), one_click, snug)
}

/// [`cancel_btns`] from the two things it reads: the name of Cancel all and the count in its pill.
fn cancel_btns_of<'a>(
    t: &Tokens,
    name: &str,
    count: Option<usize>,
    one_click: bool,
    snug: bool,
) -> [Btn<'a>; 4] {
    let all = Btn::new(name).words(t, "Cancel all").snug();
    let all = match count {
        Some(n) => all.badge(n.to_string()),
        None => all,
    };
    // Bids, Asks and the padlock keep their own padding, and give up half of it where the row would
    // otherwise wrap (a loose look, a three-digit count).
    let own = |b: Btn<'a>| if snug { b.snug() } else { b };
    let muted = |name: &str| own(Btn::new(name).words_in(t, name, t.theme.text2));
    [all, muted("Bids"), muted("Asks"), own(padlock_btn(t, one_click))]
}

/// How the cancel row fills rows `width` wide ([`pack`]): its three buttons, then the padlock, and
/// whether its Bids, Asks and padlock are snug. They have their own padding where the row fits one
/// line with it, and half of it where that is what keeps the row one line (a row that wraps either
/// way keeps the padding: snug buttons would only move where it wraps).
fn cancel_layout(
    ui: &egui::Ui,
    t: &Tokens,
    inputs: &TradeInputs<'_>,
    one_click: bool,
    counted: bool,
    width: f32,
) -> (Vec<usize>, bool) {
    cancel_layout_of(t, width, |snug| cancel_naturals(ui, t, inputs, one_click, counted, snug))
}

/// [`cancel_layout`] over the four buttons' natural widths (`naturals(snug)`): how a window that has
/// no orders yet counts the row it must have room for ([`opening_h`]).
fn cancel_layout_of(
    t: &Tokens,
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
    t: &Tokens,
    inputs: &TradeInputs<'_>,
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
/// (`TradeInputs::orders_why`, Ruling R9) every cancel is disabled with THAT reason and claims no
/// count (pre-flight Minor 15). Where `counted` is false and the count is known, it is on hover.
#[allow(clippy::too_many_arguments)]
fn cancel_row(
    ui: &mut egui::Ui,
    t: &Tokens,
    key: egui::Id,
    state: &mut TradeState,
    inputs: &TradeInputs<'_>,
    actions: &mut Vec<TradeAction>,
    (rows, snug): (&[usize], bool),
    counted: bool,
) {
    let bids = inputs.orders.iter().filter(|o| o.side > 0).count();
    let asks = inputs.orders.iter().filter(|o| o.side < 0).count();
    let cancels = [
        (TradeAction::CancelAll, inputs.orders.len(), NO_ORDERS_WHY, "cancel_all"),
        (TradeAction::CancelSide(1), bids, NO_BIDS_WHY, "cancel_bids"),
        (TradeAction::CancelSide(-1), asks, NO_ASKS_WHY, "cancel_asks"),
    ];
    let naturals = cancel_naturals(ui, t, inputs, state.one_click, counted, snug);
    in_rows(ui, t, rows, &naturals, Fill::Item(0), |ui, i, w| {
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
fn padlock_btn<'a>(t: &Tokens, one_click: bool) -> Btn<'a> {
    let (icon, tip) = if one_click {
        (icons::ONE_CLICK_ON, ONE_CLICK_ON_TIP)
    } else {
        (icons::ONE_CLICK_OFF, ONE_CLICK_OFF_TIP)
    };
    Btn::new(tip).icon(t, icon, None).toggled(one_click)
}

/// The one-click padlock. Changing it lets go of a held order, and says so as the strip's Cancel
/// does, "Not sent." (final review A, minor 4).
fn padlock(
    ui: &mut egui::Ui,
    key: egui::Id,
    lock: Btn<'_>,
    state: &mut TradeState,
    actions: &mut Vec<TradeAction>,
) {
    let tip = if state.one_click { ONE_CLICK_ON_TIP } else { ONE_CLICK_OFF_TIP };
    if keyed(ui, key.with("one_click"), lock).on_hover_text(tip).clicked() {
        state.one_click = !state.one_click;
        if state.held.take().is_some() {
            let text = super::status::NOT_SENT.to_string();
            actions.push(TradeAction::Note { kind: StatusKind::Info, text });
        }
    }
}

/// The unit switch: the unit the size is in, and the arrows. It converts the size at
/// [`reference_price`], or, with no size to convert, empties the field: a number never silently
/// changes what it means. With no price to convert at it is disabled.
fn unit_btn<'a>(
    t: &Tokens,
    state: &TradeState,
    inputs: &TradeInputs<'_>,
    convertible: bool,
) -> Btn<'a> {
    let (to, now) = match state.unit {
        SizeUnit::Base => (inputs.quote, inputs.base),
        SizeUnit::Quote => (inputs.base, inputs.quote),
    };
    let b = Btn::new(format!("Enter the size in {to} instead")).words(t, now).icon(
        t,
        icons::SWAP_UNITS,
        Some(t.theme.text3),
    );
    if convertible { b } else { b.disabled_because(NO_CONVERT_WHY) }
}

fn swap_units(
    ui: &mut egui::Ui,
    t: &Tokens,
    state: &mut TradeState,
    inputs: &TradeInputs<'_>,
    order_type: OrderType,
) {
    let to = match state.unit {
        SizeUnit::Base => SizeUnit::Quote,
        SizeUnit::Quote => SizeUnit::Base,
    };
    let at = reference_price(state, order_type, inputs);
    let tip = match to {
        SizeUnit::Quote => format!("Enter the size in {} instead", inputs.quote),
        SizeUnit::Base => format!("Enter the size in {} instead", inputs.base),
    };
    if ui.add(unit_btn(t, state, inputs, at.is_some())).on_hover_text(tip).clicked() {
        let base = sizing::base_qty(&state.size, state.unit, at, inputs.grid.lot);
        state.unit = to;
        match base {
            Some(q) => write_size(state, q, at, inputs.grid.lot),
            None => state.size.clear(),
        }
    }
}

/// The caption of a field's row, at its left (`Price`, `Trigger`, `Size`).
fn row_label(ui: &mut egui::Ui, t: &Tokens, words: &str) {
    ui.label(RichText::new(words).font(t.font(TextRole::Caption)).color(t.theme.text3));
}

/// The size field in its row, `Size`, the field and the unit switch, for a field `field_w` wide
/// where the caller has fixed it, else the width the row leaves. Its answer is whether the field
/// holds something that is not a number.
fn size_row(
    ui: &mut egui::Ui,
    t: &Tokens,
    state: &mut TradeState,
    inputs: &TradeInputs<'_>,
    order_type: OrderType,
    field_w: Option<f32>,
) -> bool {
    let ctx = ui.ctx().clone();
    let gap = t.metrics.gap;
    let at = reference_price(state, order_type, inputs);
    let unit_w = unit_btn(t, state, inputs, at.is_some()).natural_w(&ctx, t);
    row_label(ui, t, "Size");
    let w = field_w.unwrap_or_else(|| ui.available_width() - unit_w - gap).max(trade::MIN_FIELD_W);
    let (resp, bad) = number_field(ui, t, &mut state.size, "0", w);
    if bad {
        let _ = resp.on_hover_text("Not a number");
    }
    swap_units(ui, t, state, inputs, order_type);
    bad
}

/// The five quick sizes, lot multiples (spec §3.6), in rows `ui`'s width wide: every one, wrapped
/// onto as many rows as they take, or, with `one_row` (the compact ticket's fixed row), as many as
/// fit on one — the rest are the full ticket's. Each row's buttons share its width, and the one
/// that IS the size field's reads chosen. While the lot is not known each is disabled and says
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
    let held = sizing::base_qty(&state.size, state.unit, at, lot);
    let naturals: Vec<f32> =
        words.iter().map(|w| digits_btn(t, w).natural_w(ui.ctx(), t)).collect();
    let mut rows = pack(&naturals, t.metrics.gap, ui.available_width());
    if one_row {
        rows.truncate(1);
    } else {
        rows = balance(rows);
    }
    in_rows(ui, t, &rows, &naturals, Fill::Equal, |ui, i, w| {
        let chosen = known(sizes[i]) && held.is_some_and(|q| (q - sizes[i]).abs() < 0.5 * lot);
        let b = digits_btn(t, &words[i]).width(w);
        let b = if chosen { b.look(Look::Chosen) } else { b };
        let b = match &why {
            Some(w) => b.disabled_because(w),
            None => b,
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
/// many rows of `ui`'s width as they take, each row's buttons sharing it.
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
    let naturals: Vec<f32> =
        words.iter().map(|w| digits_btn(t, w).natural_w(ui.ctx(), t)).collect();
    let rows = balance(pack(&naturals, t.metrics.gap, ui.available_width()));
    in_rows(ui, t, &rows, &naturals, Fill::Equal, |ui, i, w| {
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
        let b = digits_btn(t, &words[i]).width(w);
        let b = match &why {
            Some(w) => b.disabled_because(w),
            None => b,
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

/// The Reduce toggle: PRESSED while the order only reduces, its check drawn in the accent. Named
/// by its words, whatever it draws; `snug` is the compact ticket's tightest row.
fn reduce_btn<'a>(t: &Tokens, on: bool, word: &str, snug: bool) -> Btn<'a> {
    let b = Btn::new(word);
    let b = if on { b.icon(t, icons::CHECK, Some(t.theme.accent)) } else { b };
    let b = b.words(t, word).toggled(on);
    if snug { b.snug() } else { b }
}

/// Post only and Leverage, drawn and disabled with the owner's words (spec §3.7), the last growing
/// to fill its row. Neither takes a click, so neither ever produces an intent (spec §7).
fn greyed_btn<'a>(t: &Tokens, words: &str, why: &'a str) -> Btn<'a> {
    Btn::text(t, words).disabled_because(why)
}

/// The TP/SL toggle. `Some` with its response where TP/SL can be used. Where it cannot (I-3,
/// ruled): while the trader has it TICKED it stays ticked and enabled, so it can be turned off,
/// and the order is refused until it is ([`order`]; the caller writes the reason beside it); once
/// it is off it is disabled and says why on hover. In a window that takes no order at all
/// ([`why_untradable`]) it is an order control like any other: disabled, showing the trader's
/// setting, with the window's reason on hover (W3 review minor 7). Its name to a screen reader is
/// `TP/SL` whatever face it wears ([`TpslFace`]).
fn tpsl_toggle(
    ui: &mut egui::Ui,
    t: &Tokens,
    state: &mut TradeState,
    inputs: &TradeInputs<'_>,
    order_type: OrderType,
    face: TpslFace,
) -> Option<egui::Response> {
    let b = |on: bool| tpsl_btn(t, on, face, &state.tp_text, &state.sl_text);
    if let Some(why) = why_untradable(inputs) {
        let _ = ui.add(b(state.tpsl).disabled_because(&why));
        return None;
    }
    match tpsl_block(state, inputs, order_type) {
        None => {
            let r = ui.add(b(state.tpsl));
            if r.clicked() {
                state.tpsl = !state.tpsl;
            }
            Some(r)
        }
        Some(why) if state.tpsl => {
            let r = ui.add(b(true));
            if r.clicked() {
                state.tpsl = false;
            }
            let _ = r.on_hover_text(tpsl_refusal(why));
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
struct TpslFace {
    distances: bool,
    snug: bool,
}

/// The TP/SL toggle, in state `on`, with the face `face` and the distances `tp` and `sl` as typed.
fn tpsl_btn<'a>(t: &Tokens, on: bool, face: TpslFace, tp: &str, sl: &str) -> Btn<'a> {
    let b = Btn::new("TP/SL");
    let b = if on { b.icon(t, icons::CHECK, Some(t.theme.accent)) } else { b };
    let b = if face.distances {
        b.words(t, "TP")
            .digits(t, &format!("+{}%", leg_words(tp)))
            .words(t, "SL")
            .digits(t, &format!("−{}%", leg_words(sl)))
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

/// The exits a BUY would carry, as the design's caption beside the toggle writes them: `TP 65,759.6`
/// and `SL 65,236.1` — from the entry price and the distances typed, never from a size, so it reads
/// whatever the size field holds; a dash for an exit nothing can be set for.
fn exit_words(state: &TradeState, inputs: &TradeInputs<'_>, order_type: OrderType) -> [String; 2] {
    let tick = inputs.grid.tick;
    let exits = entry_price(1, order_type, state, inputs)
        .zip(legs(state))
        .and_then(|(entry, (tp, sl))| sizing::exits(1, entry, tp, sl, tick));
    let px = |p: Option<f64>| p.map_or_else(|| NO_SIZE.to_string(), |p| ladder::fmt_px(p, tick));
    [
        format!("TP {}", px(exits.map(|e| e.take_profit))),
        format!("SL {}", px(exits.map(|e| e.stop_loss))),
    ]
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
    let [buy, sell] =
        [1, -1].map(|side| send_btn(t, side, entry, state, inputs).natural_w(ui.ctx(), t));
    let memory = key.with("trade_send_stacked");
    let was = ui.data(|d| d.get_temp::<bool>(memory)).unwrap_or(false);
    let digits = text_w(ui.ctx(), "00", &mono_semibold(t, TextRole::Strong), t);
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

/// A Buy or Sell button as it is drawn: `Buy 0.010 limit`, its number in the mono face.
fn send_btn<'a>(
    t: &Tokens,
    side: i32,
    entry: Entry<'_>,
    state: &TradeState,
    inputs: &TradeInputs<'_>,
) -> Btn<'a> {
    let (verb, qty, kind) = side_words(side, entry.order_type, state, inputs, entry.market_word);
    send_btn_of(t, side, (&verb, &qty, &kind))
}

/// [`send_btn`] for the words `(verb, qty, kind)`.
fn send_btn_of<'a>(t: &Tokens, side: i32, (verb, qty, kind): (&str, &str, &str)) -> Btn<'a> {
    let strong = semibold(t, TextRole::Strong);
    Btn::new(format!("{verb} {qty} {kind}"))
        .part(&format!("{verb} "), strong.clone(), None)
        .part(qty, mono_semibold(t, TextRole::Strong), None)
        .part(&format!(" {kind}"), strong, None)
        .look(if side > 0 { Look::Buy } else { Look::Sell })
        .inline()
        .snug()
        .tall()
}

/// The Buy and Sell buttons for `entry`, in `rows` ([`send_rows`]): each names the order
/// [`order_as`] builds, and is disabled with its reason when it refuses.
#[allow(clippy::too_many_arguments)]
fn send_buttons(
    ui: &mut egui::Ui,
    t: &Tokens,
    key: egui::Id,
    state: &mut TradeState,
    inputs: &TradeInputs<'_>,
    actions: &mut Vec<TradeAction>,
    entry: Entry<'_>,
    rows: &[usize],
) {
    let naturals: Vec<f32> =
        [1, -1].map(|side| send_btn(t, side, entry, state, inputs).natural_w(ui.ctx(), t)).to_vec();
    in_rows(ui, t, rows, &naturals, Fill::Equal, |ui, i, w| {
        let (side, role) = if i == 0 { (1, "buy") } else { (-1, "sell") };
        let made = order_as(side, entry.order_type, state, inputs);
        let base = send_btn(t, side, entry, state, inputs).width(w);
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
fn price_row(
    ui: &mut egui::Ui,
    t: &Tokens,
    state: &mut TradeState,
    inputs: &TradeInputs<'_>,
) -> bool {
    let hint = if state.order_type == OrderType::Stop { "Trigger" } else { "Price" };
    let ctx = ui.ctx().clone();
    let gap = t.metrics.gap;
    let (bid, ask) = best(inputs);
    let (bid_ink, ask_ink) = Pair::BidAsk.texts(t);
    let sides = [("Bid", bid, bid_ink), ("Ask", ask, ask_ink)];
    let buttons = sides.map(|(words, _, ink)| Btn::new(words).words_in(t, words, ink));
    let trailing: f32 = buttons.iter().map(|b| b.natural_w(&ctx, t) + gap).sum();
    row_label(ui, t, hint);
    let w = (ui.available_width() - trailing).max(trade::MIN_FIELD_W);
    let (_, bad) = number_field(ui, t, &mut state.price, hint, w);
    for ((_, px, _), b) in sides.into_iter().zip(buttons) {
        let b = if px.is_some() { b } else { b.disabled_because(NO_SIDE_WHY) };
        if ui.add(b).clicked()
            && let Some(p) = px
        {
            state.price = price_text(p, inputs);
        }
    }
    bad
}

/// The TP/SL toggle and, beside it, what a buy's exits would be at the distances typed
/// ([`exit_words`]): grey while TP/SL is off, in the market's colours while it is on and allowed.
/// Where the toggle's row has no room for them they are left out, never cut: the same exits are on
/// the toggle's hover ([`tpsl_lines`]), and written under the row while TP/SL is on.
fn tpsl_row(
    ui: &mut egui::Ui,
    t: &Tokens,
    key: egui::Id,
    state: &mut TradeState,
    inputs: &TradeInputs<'_>,
    order_type: OrderType,
) -> bool {
    let toggle = tpsl_toggle(ui, t, state, inputs, order_type, TpslFace::default());
    let allowed = toggle.is_some() && state.tpsl;
    if let Some(r) = &toggle {
        let [buy, sell] = tpsl_lines(state, inputs, order_type);
        let _ = r.clone().on_hover_text(format!("{buy}\n{sell}"));
    }
    let words = exit_words(state, inputs, order_type);
    let font = caption_font(t);
    let gap = t.metrics.gap;
    let needed: f32 = words.iter().map(|w| text_w(ui.ctx(), w, &font, t)).sum::<f32>() + gap;
    if fits(ui, key.with("tpsl_exits_fit"), needed, ui.available_width(), two_digits(ui.ctx(), t)) {
        let (tp_ink, sl_ink) = Pair::TpSl.texts(t);
        for (w, ink) in words.into_iter().zip([tp_ink, sl_ink]) {
            let ink = if allowed { ink } else { t.theme.text3 };
            ui.label(RichText::new(w).font(font.clone()).color(ink));
        }
    }
    allowed
}

/// The TP and SL distances, in percent, while TP/SL is on and allowed: the two fields the design's
/// fixed distances have no place for.
fn tpsl_legs(ui: &mut egui::Ui, t: &Tokens, state: &mut TradeState) {
    ui.spacing_mut().item_spacing.x = t.metrics.gap;
    let each = ((ui.available_width() - t.metrics.gap) / 2.0).max(trade::MIN_FIELD_W);
    ui.spacing_mut().text_edit_width = each;
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
pub(super) fn full(
    ui: &mut egui::Ui,
    t: &Tokens,
    state: &mut TradeState,
    inputs: &TradeInputs<'_>,
    actions: &mut Vec<TradeAction>,
) {
    let (gap, pad) = (t.metrics.gap, t.metrics.pad);
    let area = ui.available_rect_before_wrap().shrink2(egui::vec2(pad, pad));
    // Every order and cancel button is keyed under the ticket's own id ([`keyed`]).
    let key = ui.id();
    ui.spacing_mut().item_spacing = egui::vec2(gap, gap);
    ui.spacing_mut().interact_size.y = t.metrics.control_h;
    let rows_now = |ui: &egui::Ui, state: &TradeState| {
        let entry = Entry { order_type: state.order_type, market_word: MARKET };
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
    if hidden_below(scroll.content_size.y, scroll.state.offset.y, scroll.inner_rect.height()) {
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
    let entry = Entry { order_type: state.order_type, market_word: MARKET };
    send_buttons(&mut pinned, t, key, state, inputs, actions, entry, &send);
    cancel_row(&mut pinned, t, key, state, inputs, actions, (&cancel.0, cancel.1), true);
}

/// How tall the rows pinned under the full ticket's form are: Buy and Sell, a tall row each where
/// they stack, then the cancel row, a row of controls, wrapped as it wraps.
fn pinned_h(t: &Tokens, send_rows: usize, cancel_rows: usize) -> f32 {
    let (gap, control_h) = (t.metrics.gap, t.metrics.control_h);
    let rows = (send_rows + cancel_rows) as f32;
    send_rows as f32 * tall_h(t) + cancel_rows as f32 * control_h + (rows - 1.0).max(0.0) * gap
}

/// Whether a scroll area's content goes on below what it shows: content `content_h` tall, scrolled
/// `offset` down, in a view `visible_h` tall. Half a point of slack, egui's rounding, so a form
/// that fits exactly, or one scrolled to its end, says nothing.
fn hidden_below(content_h: f32, offset: f32, visible_h: f32) -> bool {
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
/// `control_h`, a line of text the label its font draws ([`label_h`]), and a row of buttons wraps
/// by [`pack`] over [`Btn::natural_w`], as the form wraps it: the form's own sizes, read where it
/// reads them. `layout::window_size` puts the instrument bar, the strip and the window around it.
///
/// That the form is counted at its tallest costs nothing where it is shorter: the pinned rows
/// follow the form, and the spare room is under them.
pub(super) fn opening_h(ctx: &egui::Context, t: &Tokens, width: f32) -> f32 {
    let (gap, control_h, pad) = (t.metrics.gap, t.metrics.control_h, t.metrics.pad);
    let inner = width - 2.0 * pad;
    let line = |font: FontId| label_h(ctx, &font, t);
    let rows = |n: f32| n * control_h + (n - 1.0).max(0.0) * gap;
    let wrapped = |naturals: &[f32]| rows(pack(naturals, gap, inner).len() as f32);
    let shares = share_words();
    let shares = shares.each_ref().map(|w| digits_btn(t, w).natural_w(ctx, t));
    let quick = FINEST_QUICK.map(|w| digits_btn(t, w).natural_w(ctx, t));
    let toggles = [
        reduce_btn(t, false, "Reduce only", false).natural_w(ctx, t),
        greyed_btn(t, POST_ONLY, POST_WHY).natural_w(ctx, t),
        greyed_btn(t, "Leverage", LEVERAGE_WHY).natural_w(ctx, t),
    ];
    // [`position_card`]: a border and a pad round the facts (the side's word and the size at the
    // price) and, under them, the P/L, then Close and Reverse.
    let facts = t.text.px(TextRole::Body);
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
        |side| send_btn_of(t, side, (if side > 0 { "Buy" } else { "Sell" }, "0.00010", "limit"));
    let both = widest(1).natural_w(ctx, t) + gap + widest(-1).natural_w(ctx, t);
    let send_rows = if both > inner { 2 } else { 1 };
    // The cancel row at its widest, a three-digit count in Cancel all's pill (the sweep's `123
    // orders`): one row where it fits, two where Large's wider words leave no room. It was counted
    // as one row always, and a window opened for Comfortable density at Large text hid TP/SL 36 pt
    // below the fold.
    let cancel = cancel_layout_of(t, inner, |snug| {
        cancel_btns_of(t, "Cancel all 123", Some(123), true, snug)
            .iter()
            .map(|b| b.natural_w(ctx, t))
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
    t: &Tokens,
    key: egui::Id,
    state: &mut TradeState,
    inputs: &TradeInputs<'_>,
    actions: &mut Vec<TradeAction>,
) {
    let gap = t.metrics.gap;
    ui.spacing_mut().item_spacing = egui::vec2(gap, gap);
    section(ui, key, "form_position", |ui| position_card(ui, t, key, state, inputs, actions));
    if let Some(why) = why_untradable(inputs) {
        section(ui, key, "form_untradable", |ui| {
            ui.label(RichText::new(why).font(t.font(TextRole::Body)).color(t.theme.text2));
        });
    }
    section(ui, key, "form_type", |ui| order_types(ui, t, state));
    let order_type = state.order_type;
    if order_type == OrderType::Market {
        section(ui, key, "form_price", |ui| {
            // What a market order fills at, where the price field would be: the design's note.
            ui.label(
                RichText::new(MARKET_NOTE).font(t.font(TextRole::Caption)).color(t.theme.text3),
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
        if ui.horizontal(|ui| size_row(ui, t, state, inputs, order_type, None)).inner {
            not_a_number(ui, t);
        }
        sub_lines(ui, t, key, state, inputs, order_type);
    });
    section(ui, key, "form_quick", |ui| quick_sizes(ui, t, state, inputs, order_type, false));
    section(ui, key, "form_shares", |ui| shares(ui, t, state, inputs, order_type));
    section(ui, key, "form_toggles", |ui| toggles(ui, t, state, POST_ONLY, "Reduce only"));
    let mut allowed = false;
    section(ui, key, "form_tpsl", |ui| {
        allowed = ui.horizontal(|ui| tpsl_row(ui, t, key, state, inputs, order_type)).inner;
    });
    if allowed {
        section(ui, key, "form_tpsl_legs", |ui| {
            ui.horizontal(|ui| tpsl_legs(ui, t, state));
        });
    }
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

/// What the ticket writes where a market order has no price to type (the design's words).
const MARKET_NOTE: &str = "Fills now at the best ask for a buy, the best bid for a sell.";

/// Under a number field that holds no number: the words the border's red stands for.
fn not_a_number(ui: &mut egui::Ui, t: &Tokens) {
    ui.label(
        RichText::new("Not a number").font(t.font(TextRole::Caption)).color(Status::Error.color()),
    );
}

/// The three order types, equal buttons across the ticket, the chosen one outlined in the accent.
fn order_types(ui: &mut egui::Ui, t: &Tokens, state: &mut TradeState) {
    let types = [
        (OrderType::Limit, "Limit", "A limit order at your price"),
        (OrderType::Market, "Market", "Fills now at the best price"),
        (OrderType::Stop, "Stop", "A market order when the trigger trades"),
    ];
    let naturals: Vec<f32> = types
        .iter()
        .map(|(_, words, _)| Btn::text(t, words).snug().natural_w(ui.ctx(), t))
        .collect();
    let rows = balance(pack(&naturals, t.metrics.gap, ui.available_width()));
    in_rows(ui, t, &rows, &naturals, Fill::Equal, |ui, i, w| {
        let (value, words, why) = types[i];
        let on = state.order_type == value;
        if ui.add(Btn::text(t, words).snug().width(w).toggled(on)).on_hover_text(why).clicked() {
            state.order_type = value;
        }
    });
}

/// Reduce only, Post only and Leverage, in as many rows as they take, the last of each growing.
fn toggles(
    ui: &mut egui::Ui,
    t: &Tokens,
    state: &mut TradeState,
    post_word: &str,
    reduce_word: &str,
) {
    let ctx = ui.ctx().clone();
    let reduce = reduce_btn(t, state.reduce_only, reduce_word, false);
    let [post, lev] = greyed_words(post_word);
    let naturals = [
        reduce.natural_w(&ctx, t),
        greyed_btn(t, post, POST_WHY).natural_w(&ctx, t),
        greyed_btn(t, lev, LEVERAGE_WHY).natural_w(&ctx, t),
    ];
    let rows = pack(&naturals, t.metrics.gap, ui.available_width());
    in_rows(ui, t, &rows, &naturals, Fill::LastOfRow, |ui, i, w| match i {
        0 => {
            let b = reduce_btn(t, state.reduce_only, reduce_word, false).width(w);
            if ui.add(b).clicked() {
                state.reduce_only = !state.reduce_only;
            }
        }
        1 => {
            ui.add(greyed_btn(t, post, POST_WHY).width(w));
        }
        _ => {
            ui.add(greyed_btn(t, lev, LEVERAGE_WHY).width(w));
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
    t: &Tokens,
    key: egui::Id,
    state: &TradeState,
    inputs: &TradeInputs<'_>,
    order_type: OrderType,
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
    t: &Tokens,
    key: egui::Id,
    state: &mut TradeState,
    inputs: &TradeInputs<'_>,
    actions: &mut Vec<TradeAction>,
) {
    let (gap, pad) = (t.metrics.gap, t.metrics.pad);
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
fn position_lines(ui: &mut egui::Ui, t: &Tokens, key: egui::Id, inputs: &TradeInputs<'_>) {
    let gap = t.metrics.gap;
    let held = position_held(t, inputs);
    let Some((pl, col)) = position_pl(t, inputs) else {
        text_row(ui, |ui| {
            ui.label(held);
            ui.label(
                RichText::new("No position on this account")
                    .font(t.font(TextRole::Caption))
                    .color(t.theme.text3),
            );
        });
        return;
    };
    let font = t.mono(TextRole::Body);
    let pl = RichText::new(pl)
        .font(font.clone())
        .color(col)
        .line_height(Some(t.text.px(TextRole::Body)));
    // The P/L's room is a four-digit one at least (`+9999.99`), so a tick that adds a digit to it
    // never moves the card: it has the room it will need, or it is two lines whatever it says.
    let reserve = text_w(ui.ctx(), &format!("{:+.2} {}", 9999.99, inputs.quote), &font, t);
    let (word, word_col, facts) = position_side(t, inputs).expect("a P/L is a position's");
    let side = semibold(t, TextRole::Caption);
    let held_w = text_w(ui.ctx(), word, &side, t) + gap + text_w(ui.ctx(), &facts, &font, t);
    let pl_w = text_w(ui.ctx(), pl.text(), &font, t).max(reserve);
    let needed = held_w + pl_w;
    let one_line =
        fits(ui, key.with("card_one_line"), needed, ui.available_width(), two_digits(ui.ctx(), t));
    if one_line {
        // Two labels the row centres on each other, as the design's flex row does: one job would
        // sit the smaller word on the larger number's bottom edge, not its middle.
        let line = Some(t.text.px(TextRole::Body));
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

/// The height of the compact ticket's position strip: Close and Reverse, a pad above and below
/// them, and the rule under the strip.
fn strip_h(m: &Metrics) -> f32 {
    m.control_h + 2.0 * m.pad + stroke::HAIRLINE
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
pub fn compact_h(m: &Metrics) -> f32 {
    let tall = m.control_h + m.pad;
    strip_h(m) + m.pad + 4.0 * m.control_h + 2.0 * tall + 5.0 * m.gap + m.pad
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
pub(super) fn compact(
    ui: &mut egui::Ui,
    t: &Tokens,
    state: &mut TradeState,
    inputs: &TradeInputs<'_>,
    actions: &mut Vec<TradeAction>,
) {
    let (gap, pad, control_h) = (t.metrics.gap, t.metrics.pad, t.metrics.control_h);
    let region = ui.available_rect_before_wrap();
    // Every order and cancel button is keyed under the ticket's own id ([`keyed`]).
    let key = ui.id();
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
    let entry = Entry { order_type: OrderType::Market, market_word: "MKT" };
    let send = send_rows(ui, t, key, state, inputs, entry, body.width());
    let wanted =
        |rows: usize| 3.0 * control_h + rows as f32 * tall + (rows as f32 + 3.0) * gap + control_h;
    let roomy = wanted(send.len()) <= body.height() + 0.5;
    ui.horizontal(|ui| {
        size_row(ui, t, state, inputs, OrderType::Market, Some(3.5 * control_h));
        // The value, at the right, where the unit switch leaves room for it. Cut, with its whole
        // text on hover, only where it alone is wider than what is left.
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.add(
                egui::Label::new(caption_text(t, value_text(state, inputs, OrderType::Market)))
                    .truncate(),
            );
        });
    });
    if roomy {
        quick_sizes(ui, t, state, inputs, OrderType::Market, true);
    }
    ui.horizontal(|ui| match why_untradable(inputs) {
        Some(why) => {
            reason_line(ui, t, &why, t.theme.text2);
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
    t: &Tokens,
    key: egui::Id,
    strip: Rect,
    state: &mut TradeState,
    inputs: &TradeInputs<'_>,
    actions: &mut Vec<TradeAction>,
) {
    let pad = t.metrics.pad;
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
fn job_h(ctx: &egui::Context, job: LayoutJob) -> f32 {
    ctx.fonts_mut(|f| f.layout_job(job)).size().y
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
    t: &Tokens,
    state: &mut TradeState,
    inputs: &TradeInputs<'_>,
) {
    let gap = t.metrics.gap;
    let ctx = ui.ctx().clone();
    let avail = ui.available_width();
    let refusal = tpsl_refused(state, inputs, OrderType::Market);
    let font = t.font(TextRole::Caption);
    // The three ways to say it, widest first: the long words, the short words, a warning mark.
    let said =
        [text_w(&ctx, TPSL_OFF_TO_SEND, &font, t), text_w(&ctx, TPSL_OFF, &font, t), font.size];
    // What the row asks for in each shape, the widest first; the last is what it falls back to.
    // A refused order's row has no distances: its words need the room.
    let shapes: Vec<(TpslFace, usize)> = if refusal.is_some() {
        vec![
            (TpslFace { distances: false, snug: false }, 0),
            (TpslFace { distances: false, snug: true }, 0),
            (TpslFace { distances: false, snug: true }, 1),
            (TpslFace { distances: false, snug: true }, 2),
        ]
    } else {
        vec![
            (TpslFace { distances: true, snug: false }, 0),
            (TpslFace { distances: false, snug: false }, 0),
            (TpslFace { distances: false, snug: true }, 0),
        ]
    };
    let width_of = |(face, words): &(TpslFace, usize)| {
        let tp = tpsl_btn(t, state.tpsl, *face, &state.tp_text, &state.sl_text);
        let reduce = reduce_btn(t, state.reduce_only, "Reduce", face.snug);
        let own = tp.natural_w(&ctx, t) + gap + reduce.natural_w(&ctx, t);
        own + if refusal.is_some() { gap + said[*words] } else { 0.0 }
    };
    let (face, words) = *shapes
        .iter()
        .find(|s| width_of(s) <= avail)
        .unwrap_or_else(|| shapes.last().expect("the row has shapes"));
    if let Some(r) = tpsl_toggle(ui, t, state, inputs, OrderType::Market, face) {
        let [buy, sell] = tpsl_lines(state, inputs, OrderType::Market);
        let _ = r.on_hover_text(format!(
            "TP {} % and SL {} % from the entry, set in the full ticket.\n{buy}\n{sell}",
            state.tp_text.trim(),
            state.sl_text.trim()
        ));
    }
    if ui.add(reduce_btn(t, state.reduce_only, "Reduce", face.snug)).clicked() {
        state.reduce_only = !state.reduce_only;
    }
    match refusal {
        Some(refusal) => {
            // The words that fit the row, never cut (a 260 pt window cut the long ones at
            // Comfortable density and Large text): the whole reason is the hover.
            let colour = Status::Warning.color();
            let said = match words {
                2 => icons::WARNING.rich().size(font.size).color(colour),
                n => {
                    RichText::new([TPSL_OFF_TO_SEND, TPSL_OFF][n]).font(font.clone()).color(colour)
                }
            };
            let _ = ui.add(egui::Label::new(said).truncate()).on_hover_text(refusal);
        }
        None => {
            // The greyed placeholders, as many as the row has room for: Post, then a Leverage that
            // takes what is left.
            let post = greyed_btn(t, "Post", POST_WHY);
            if post.natural_w(&ctx, t) <= ui.available_width() {
                ui.add(post);
                let lev = greyed_btn(t, "Leverage", LEVERAGE_WHY);
                if lev.natural_w(&ctx, t) <= ui.available_width() {
                    let w = ui.available_width();
                    ui.add(lev.width(w));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Fill, balance, hidden_below, pack, row_widths};

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

    /// The v3 design's rows of choices are EQUAL buttons: `Equal` gives every item of a row the same
    /// width with the whole row between them, whatever each needs; `LastOfRow` and `Item` leave
    /// every item its own width but one, which takes what is left (never less than it needs).
    #[test]
    fn a_row_shares_its_width_as_its_fill_says() {
        // 3 items, 2 gaps of 6: (132 - 12) / 3.
        let eq = row_widths(Fill::Equal, 0, &[30.0, 50.0, 40.0], 6.0, 132.0);
        assert_eq!(eq, [40.0, 40.0, 40.0]);
        let last = row_widths(Fill::LastOfRow, 0, &[30.0, 50.0, 40.0], 6.0, 132.0);
        assert_eq!(last, [30.0, 50.0, 132.0 - 12.0 - 80.0]);
        // The item that grows is counted over every row: item 4 is the second row's second item.
        let second = row_widths(Fill::Item(4), 3, &[30.0, 40.0], 6.0, 100.0);
        assert_eq!(second, [30.0, 64.0]);
        let elsewhere = row_widths(Fill::Item(0), 3, &[30.0, 40.0], 6.0, 100.0);
        assert_eq!(elsewhere, [30.0, 40.0], "a row without the growing item grows none");
        // An item that does not fit its row keeps the width it needs.
        let tight = row_widths(Fill::LastOfRow, 0, &[60.0, 60.0], 6.0, 100.0);
        assert_eq!(tight, [60.0, 60.0]);
    }

    /// Five quick sizes that wrap are three and two, never four and one; a row that fits is left
    /// alone, and the items are never lost or moved between rows' order.
    #[test]
    fn rows_that_wrap_are_evened_out() {
        assert_eq!(balance(vec![4, 1]), [3, 2]);
        assert_eq!(balance(vec![3, 3, 1]), [3, 2, 2]);
        assert_eq!(balance(vec![5]), [5]);
        assert_eq!(balance(vec![2, 2]), [2, 2]);
        assert_eq!(balance(Vec::new()), Vec::<usize>::new());
    }
}
