//! The tickets (spec §3.6): the full one beside the ladder (or alone), and the compact one under
//! it, for a narrow window.
//!
//! # The look
//!
//! Both draw the owner's v3 design (its capture's geometry dumps, beside the ladder and under it,
//! hold the sizes): rows of CHOICES are equal buttons across the ticket — the
//! order types, the quick sizes, the shares, Buy and Sell, Close and Reverse (`fit::TradeWindowTicketRowFill::Equal`);
//! toggles are buttons that read pressed, never check boxes (Reduce only, TP/SL, the padlock); the
//! position is a bordered card (the full ticket) or a strip (the compact one); Buy and Sell are tall.
//! The controls are `button::TradeWindowTicketButton` and `button::number_field`, drawn from the kit's tokens.
//! Where the design has two shapes for a row, the ticket measures and takes the one that fits —
//! never cuts a word (`trade_fit.rs` holds that at every width, density and text size).
//!
//! # ⚠ The money path
//!
//! [`order`] is the one place the ticket becomes a [`TradeWindowAction::Place`]. The Buy and Sell buttons
//! send what it answers, name the size it computes, and are disabled with the reason it gives when
//! it refuses, so a button can never name one order and send another.
//!
//! - **A bracket is both legs or no order** (spec §4.4, "no lone exits"). With TP/SL on where it is
//!   allowed, a leg that is blank, not a number or not above zero refuses the order, and so does a
//!   grid with no room for an exit ([`sizing::exits`]). The entry is never sent without them.
//! - **TP/SL ticked where it is refused refuses the order** ([`take_profit_stop_loss_block`]: the node, the account,
//!   the entry type, reduce-only; fix round 1, I-3, ruled). The toggle stays ticked, the reason is
//!   written under it ([`take_profit_stop_loss_refusal`]), and it is enabled only so it can be turned off: the
//!   ticket never sends an order without the exits the trader set. A ladder click carries
//!   no exits by design (spec §3.6).
//! - **A typed price sits on the tick or is refused** (fix round 1, I-2, ruled): the core would
//!   round it to the nearest tick, past the trader's limit, with its exits measured from a price
//!   the order does not sit at.
//! - **A market order needs a price** (the owner's decision of 2026-10-03): with no side of the
//!   book to fill against and no last price, it is refused ([`NO_MARKET_PRICE_WHY`]). So is a
//!   Reverse, whose second half opens a position at market; a Close, which only reduces, is not.
//!   The same rule holds at the confirm: a market order or a Reverse held for one is dropped, with
//!   a note, when its side loses its price while it waits (`TradeWindowState::sync`).
//! - **A Stop whose trigger would fire at once is WARNED, never refused** (`stop_warning`): a
//!   line under the trigger before the click, and the same words in the confirm prompt.
//! - **A size converts at the order's own price**: a Limit's price or a Stop's trigger, else the
//!   side's best (the ask for a buy, the bid for a sell), else the last price, as a ladder click
//!   converts at its row's price.
//! - **No size is invented** (pre-flight I18). A lot the catalog has not given refuses every size,
//!   offers no quick size and never prints `(0)`; a quote value with no price to convert at is never
//!   written into the size field.

mod button;
pub(super) mod compact;
mod fit;
pub(super) mod full;
mod rows;
mod take_profit_stop_loss;
#[cfg(test)]
mod tests;

use super::describe::why_untradable;
use super::{
    TradeWindowAction, TradeWindowInputs, TradeWindowOrigin, TradeWindowSizingUnit,
    TradeWindowState, TradeWindowTicketExits, TradeWindowTicketKind, ladder, sizing,
};
use vike_ui_theme::components::input;

/// Why Post only is greyed against a node too old to carry it. It says the true cause and the cure:
/// that node would drop the order's lifetime and run it as a plain limit order that takes liquidity
/// ([`TradeWindowInputs::post_only_wire`]).
pub const POST_NODE_WHY: &str = "Post only is not available: the node you are connected to is too old \
                                 to carry post-only orders. Update the node.";

/// Why Post only is greyed for any order but a Limit one: a market order takes liquidity at once and a
/// stop takes it when it fires, so neither has a resting price to protect.
pub const POST_LIMIT_WHY: &str = "Post only needs a Limit order: a market order takes liquidity at \
                                  once and a stop takes it when it fires.";

/// ...and the compact ticket's own words for it: that ticket sends market orders whatever order type
/// was chosen, so no choice there can make Post only available; the full ticket can.
pub const POST_COMPACT_WHY: &str = "Post only needs a Limit order, and this compact ticket sends \
                                    market orders only. Make the window wider for the full ticket.";

/// Why an order with Post only AND TP/SL on is refused: a bracket's entry has no lifetime on the wire,
/// so the pair cannot go out as the trader set it. Neither is turned off for them.
pub const POST_TAKE_PROFIT_STOP_LOSS_WHY: &str = "Post only and TP/SL cannot go together yet: a TP/SL entry carries \
                                 no post-only. Turn one of them off.";

/// Why a ladder click is refused while Post only is on and the click would cross the market: the
/// ladder turns that click into a stop, which takes liquidity when it fires.
pub(crate) const POST_LADDER_WHY: &str = "Post only is on: a click through the market would be a stop, which \
                                   takes liquidity. Click a price that rests, or turn Post only off.";

/// Why Post only is greyed on a venue Vike has no post-only order for (`capabilities.supports_post_only`).
pub fn post_venue_why(venue_label: &str) -> String {
    format!(
        "Post only is not available on {venue_label}: Vike has no post-only order for this venue."
    )
}

/// The owner's words for the greyed-out Leverage (spec §3.7).
pub const LEVERAGE_WHY: &str = "Not available yet: Vike cannot set leverage on the exchange.";

/// Why TP/SL is off while the node cannot carry a bracket ([`TradeWindowInputs::bracket_wire`], Ruling
/// R8). The owner's "Not available yet" wording, as for Post only and Leverage. It outranks every
/// other reason: no account and no entry type changes it.
pub const TAKE_PROFIT_STOP_LOSS_WIRE_WHY: &str =
    "Not available yet: Vike cannot send TP/SL orders through the node.";

/// Why TP/SL is off for a window that names no account to send it to (a bracket names its account,
/// `vike_model::OrderBracket::account`, so a labelled account is no reason of its own any more). The
/// GLUE states it, in [`TradeWindowInputs::bracket_why`]; the ticket says whatever cause it is handed and
/// never picks this one itself.
pub const TAKE_PROFIT_STOP_LOSS_ACCOUNT_WHY: &str = "TP/SL is not available on this account yet: the window names no \
                                    account the node runs to send a bracket to.";

/// Why TP/SL is off for a Stop entry: a bracket has no stop entry (spec §3.6).
pub const TAKE_PROFIT_STOP_LOSS_STOP_WHY: &str = "TP/SL needs a Market or Limit entry.";

/// Why TP/SL is off with Reduce only: a bracket is for an order that opens or adds.
pub const TAKE_PROFIT_STOP_LOSS_REDUCE_WHY: &str =
    "TP/SL needs an order that opens or adds; Reduce only is on.";

/// Why an order with TP/SL on is refused while a leg is blank, not a number or not above zero: a
/// bracket is both exits or no order, never its entry alone.
pub const TAKE_PROFIT_STOP_LOSS_LEGS_WHY: &str =
    "TP/SL is on: enter both TP and SL as percentages above zero, or turn TP/SL off.";

/// Why an order with TP/SL on is refused where its exits cannot be placed ([`sizing::exits`]
/// answers `None`): no tick grid, or an exit that would fall to zero or below.
pub const TAKE_PROFIT_STOP_LOSS_ROOM_WHY: &str = "TP/SL cannot be set here: each exit needs a price on the tick \
                                 grid, above zero and at least one tick from the entry.";

/// Why an order with TP/SL on is refused while there is no price to measure its exits from.
pub(crate) const TAKE_PROFIT_STOP_LOSS_ENTRY_WHY: &str =
    "TP/SL is on, but there is no price to set its exits from.";

/// Why a market Buy or Sell is refused where there is no price for it at all: no side of the book
/// to fill against and no last price (the owner's decision of 2026-10-03, round 2, item 10).
pub const NO_MARKET_PRICE_WHY: &str = "No price yet: a market order needs a book or a last price.";

/// Why the cancel buttons are disabled with nothing to cancel.
pub const NO_ORDERS_WHY: &str = "No working orders on this symbol.";

/// The one-click padlock's name and tip while one-click trading is on.
pub(crate) const ONE_CLICK_ON_TIP: &str =
    "One-click trading is on: a click sends at once. Click to ask first.";

/// ...and while it is off.
pub(crate) const ONE_CLICK_OFF_TIP: &str =
    "One-click trading is off: every order asks first. Click to send at once.";

/// What the compact ticket writes beside a ticked TP/SL it cannot use, where the whole reason
/// ([`take_profit_stop_loss_refusal`]) does not fit: the reason is its hover.
const TAKE_PROFIT_STOP_LOSS_OFF_TO_SEND: &str = "Turn TP/SL off to send.";

/// ...and where not even those words fit its row; where not even these do (Large text in a 260 pt
/// window), a warning mark, with the same hover.
const TAKE_PROFIT_STOP_LOSS_OFF: &str = "Turn TP/SL off.";

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
/// first, then the cause the app states for this account ([`TradeWindowInputs::bracket_why`]: the
/// account, or its engine's lane), then the entry type, then reduce-only.
pub fn take_profit_stop_loss_block<'a>(
    state: &TradeWindowState,
    inputs: &TradeWindowInputs<'a>,
    order_type: TradeWindowTicketKind,
) -> Option<&'a str> {
    if !inputs.bracket_wire {
        Some(TAKE_PROFIT_STOP_LOSS_WIRE_WHY)
    } else if let Some(why) = inputs.bracket_why {
        Some(why)
    } else if order_type == TradeWindowTicketKind::Stop {
        Some(TAKE_PROFIT_STOP_LOSS_STOP_WHY)
    } else if state.reduce_only {
        Some(TAKE_PROFIT_STOP_LOSS_REDUCE_WHY)
    } else {
        None
    }
}

/// Why the ticket's Buy and Sell send nothing while TP/SL is ticked where it cannot be used
/// (`why` is [`take_profit_stop_loss_block`]'s reason): the order is refused until the trader turns TP/SL off, never
/// sent without the exits they set (fix round 1, I-3).
pub fn take_profit_stop_loss_refusal(why: &str) -> String {
    format!("{why} Turn TP/SL off to send without exits.")
}

/// Why Post only cannot be used for an entry of `order_type` now, or `None` when it can. The venue
/// first, since no update cures it (`capabilities.supports_post_only`, the one row the core's preflight
/// refuses by name from), then the node (an older one drops the lifetime and runs a plain limit
/// order, [`TradeWindowInputs::post_only_wire`]), then the entry type (a Limit order alone has a resting
/// price to protect). Every reason is true and plain: a greyed button never shows the next one's.
///
/// ⚠ The ONE source of the question: the toggle's state, [`TradeWindowState::sync`]'s turning the choice
/// off, and `order_with`'s last refusal all ask it, so the button, the state and the order cannot
/// disagree. It does not look at [`TradeWindowTradable`](super::TradeWindowTradable): a window that takes no order says THAT instead
/// (`why_untradable`).
pub fn post_only_block(
    inputs: &TradeWindowInputs<'_>,
    order_type: TradeWindowTicketKind,
) -> Option<String> {
    if !inputs.capabilities.supports_post_only {
        let label = if inputs.venue_label.is_empty() { inputs.venue } else { inputs.venue_label };
        Some(post_venue_why(label))
    } else if !inputs.post_only_wire {
        Some(POST_NODE_WHY.to_string())
    } else if order_type != TradeWindowTicketKind::Limit {
        Some(POST_LIMIT_WHY.to_string())
    } else {
        None
    }
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

/// The best bid and ask: the book's, else the venue's quote where it streams no book.
pub(super) fn best(inputs: &TradeWindowInputs<'_>) -> (Option<f64>, Option<f64>) {
    (
        inputs.book.best_bid().map(|l| l.price).or(inputs.top.map(|q| q.bid)),
        inputs.book.best_ask().map(|l| l.price).or(inputs.top.map(|q| q.ask)),
    )
}

/// The price a market order of `side` takes: the ask for a buy, the bid for a sell, else the last.
/// `None` refuses a market order at the click ([`order`], the Reverse button) and drops one held
/// for a confirm (`TradeWindowState::sync`): the same rule for both.
pub(super) fn market_ref(side: i32, inputs: &TradeWindowInputs<'_>) -> Option<f64> {
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
pub(super) fn stop_warning(
    side: i32,
    trigger: f64,
    inputs: &TradeWindowInputs<'_>,
) -> Option<String> {
    let market = market_ref(side, inputs)?;
    let wrong = if side > 0 { trigger < market } else { trigger > market };
    let (word, place) = if side > 0 { ("BUY", "below") } else { ("SELL", "above") };
    let tick = inputs.grid.tick_size;
    wrong.then(|| {
        format!(
            "Stop {word} {} is {place} the market {}: it triggers at once.",
            ladder::fmt_price(trigger, tick),
            ladder::fmt_price(market, tick)
        )
    })
}

/// The typed price — a Limit's price, a Stop's trigger — when it is a number above zero.
fn typed_price(state: &TradeWindowState) -> Option<f64> {
    input::parse_number(&state.price).filter(|p| known(*p))
}

/// The price an order is placed at, and its exits measured from: its own for a Limit or a Stop,
/// the side's best for a Market order.
fn entry_price(
    side: i32,
    order_type: TradeWindowTicketKind,
    state: &TradeWindowState,
    inputs: &TradeWindowInputs<'_>,
) -> Option<f64> {
    match order_type {
        TradeWindowTicketKind::Market => market_ref(side, inputs),
        TradeWindowTicketKind::Limit | TradeWindowTicketKind::Stop => typed_price(state),
    }
}

/// The base size an order of `side` and `order_type` carries: the size field, converted from the
/// quote currency at the entry price (the side's best while a Limit has no price yet), floored to
/// the lot. [`order`] sends it and the Buy and Sell buttons name it: one function, so the two
/// cannot disagree.
fn order_quantity(
    side: i32,
    order_type: TradeWindowTicketKind,
    state: &TradeWindowState,
    inputs: &TradeWindowInputs<'_>,
) -> Option<f64> {
    let at = entry_price(side, order_type, state, inputs).or_else(|| market_ref(side, inputs));
    sizing::base_quantity(&state.size, state.unit, at, inputs.grid.lot_size, inputs.value)
}

/// Why the size field holds no size an order converted at `at` can carry.
fn size_refusal(
    state: &TradeWindowState,
    inputs: &TradeWindowInputs<'_>,
    at: Option<f64>,
) -> String {
    let Some(lot) = inputs.grid.lot_size else { return no_lot_reason(inputs.symbol) };
    // A number so large its count of lots is not a number (`1e306` on a 0.001 lot).
    let lots = input::parse_number(&state.size).map(|v| match state.unit {
        TradeWindowSizingUnit::Base => v / lot,
        TradeWindowSizingUnit::Quote => {
            inputs.value.units_worth(v, at.unwrap_or(1.0), 1.0).unwrap_or(v) / lot
        }
    });
    if state.unit == TradeWindowSizingUnit::Quote && at.is_none() {
        format!("No price to convert the size at: enter it in {}.", inputs.base)
    } else if lots.is_some_and(|n| !n.is_finite()) {
        format!("{} is too large a size to send.", state.size.trim())
    } else {
        format!(
            "Enter a size of at least one lot ({} {}).",
            sizing::quantity_text(lot, Some(lot)),
            inputs.base
        )
    }
}

/// The size an order converted at `at` carries, or why it cannot: one lot at least, a finite
/// number of lots (fix round 1, Minor 3: no `Place` ever carries an infinite quantity), and no
/// less than the instrument's smallest order (Minor 4). The ladder's clicks take this rule too,
/// so one rule applies wherever an order is made.
pub(super) fn checked_quantity(
    quantity: Option<f64>,
    state: &TradeWindowState,
    inputs: &TradeWindowInputs<'_>,
    at: Option<f64>,
) -> Result<f64, String> {
    let quantity = quantity.filter(|q| known(*q)).ok_or_else(|| size_refusal(state, inputs, at))?;
    let (lot, min) = (inputs.grid.lot_size, inputs.grid.min_qty);
    // A known quantity was floored to a known lot (`sizing::base_quantity`); no lot, no slack.
    if let Some(min) = min
        && quantity + lot.map_or(0.0, |lot| 1e-9 * lot) < min
    {
        return Err(format!(
            "Below the smallest order size: enter at least {} {}.",
            sizing::quantity_text(min, lot),
            inputs.base
        ));
    }
    Ok(quantity)
}

/// The ticket's order for `side`, or why it cannot be sent.
pub fn order(
    side: i32,
    state: &TradeWindowState,
    inputs: &TradeWindowInputs<'_>,
) -> Result<TradeWindowAction, String> {
    order_as(side, state.order_type, state, inputs)
}

/// [`order`] for an entry of `order_type`, whatever type the ticket shows: the compact ticket's
/// Buy and Sell send at market without changing the type the trader chose (pre-flight Minor 14).
fn order_as(
    side: i32,
    order_type: TradeWindowTicketKind,
    state: &TradeWindowState,
    inputs: &TradeWindowInputs<'_>,
) -> Result<TradeWindowAction, String> {
    order_with(side, order_type, state.take_profit_stop_loss, state, inputs)
}

/// Why the ticket's Buy and Sell send nothing for an entry of `order_type`, with TP/SL on or off as
/// `tpsl` says, WHATEVER is typed into the price and size fields — or `None` while they can send.
/// In this order, the order [`order_with`] refuses in, for it asks this first:
/// - the window takes no order (`why_untradable`);
/// - TP/SL is ticked where it cannot be used ([`take_profit_stop_loss_block`], said as [`take_profit_stop_loss_refusal`]);
/// - the instrument has no lot size ([`no_lot_reason`]);
/// - Post only is ticked where it cannot be used ([`post_only_block`]; [`TradeWindowState::sync`] has
///   turned it off by the next frame, this is the order that is never sent in between) or together
///   with TP/SL ([`POST_TAKE_PROFIT_STOP_LOSS_WHY`]);
/// - TP/SL is on with a leg that is not a percentage above zero ([`TAKE_PROFIT_STOP_LOSS_LEGS_WHY`]).
///
/// The ONE source of those sentences for every place that says why the ticket sends nothing: a
/// disabled Buy's hover, the form's own lines, and the status strip, which says it in place of
/// "Ready" while nothing waits for a confirm (FW6, I3 and I4: the render check found disabled
/// Buy and Sell under a strip reading "Ready · one-click trading is on").
pub(super) fn standing_refusal(
    state: &TradeWindowState,
    inputs: &TradeWindowInputs<'_>,
    order_type: TradeWindowTicketKind,
    take_profit_stop_loss: bool,
) -> Option<String> {
    if let Some(why) = why_untradable(inputs) {
        return Some(why);
    }
    // ⚠ TP/SL ticked where it cannot be used refuses the order (I-3): sent without its exits, it
    // would leave the trader unprotected while the ticket shows them protected.
    if take_profit_stop_loss
        && let Some(why) = take_profit_stop_loss_block(state, inputs, order_type)
    {
        return Some(take_profit_stop_loss_refusal(why));
    }
    if inputs.grid.lot_size.is_none() {
        return Some(no_lot_reason(inputs.symbol));
    }
    // ⚠ A maker-only order that cannot be one is REFUSED, never sent as a taker; and one with exits
    // would be the bracket path, whose entry carries no lifetime (`TradeWindowAction::Place::post_only`).
    if state.post_only {
        if let Some(why) = post_only_block(inputs, order_type) {
            return Some(why);
        }
        if take_profit_stop_loss {
            return Some(POST_TAKE_PROFIT_STOP_LOSS_WHY.to_string());
        }
    }
    (take_profit_stop_loss && legs(state).is_none())
        .then(|| TAKE_PROFIT_STOP_LOSS_LEGS_WHY.to_string())
}

/// The TP and SL distances the trader typed, in percent, while both are numbers above zero.
fn legs(state: &TradeWindowState) -> Option<(f64, f64)> {
    let leg = |text: &str| input::parse_number(text).filter(|v| known(*v));
    leg(&state.take_profit_text).zip(leg(&state.stop_loss_text))
}

/// [`order_as`] with TP/SL on or off as `tpsl` says, whatever the trader's setting: the TP/SL lines
/// ask it with TP/SL ON, to show the exits the order WOULD carry before the trader turns it on.
fn order_with(
    side: i32,
    order_type: TradeWindowTicketKind,
    take_profit_stop_loss: bool,
    state: &TradeWindowState,
    inputs: &TradeWindowInputs<'_>,
) -> Result<TradeWindowAction, String> {
    if let Some(why) = standing_refusal(state, inputs, order_type, take_profit_stop_loss) {
        return Err(why);
    }
    let price = match order_type {
        // ⚠ A market order with no price at all — no side of the book to fill against and no
        // last price — goes out blind, and the app's notional cap, which needs a price, cannot
        // hold it either: it is refused (the owner's decision of 2026-10-03, round 2, item 10).
        TradeWindowTicketKind::Market if market_ref(side, inputs).is_none() => {
            return Err(NO_MARKET_PRICE_WHY.to_string());
        }
        TradeWindowTicketKind::Market => None,
        TradeWindowTicketKind::Limit => Some(typed_price(state).ok_or("Enter a price.")?),
        TradeWindowTicketKind::Stop => Some(typed_price(state).ok_or("Enter a trigger price.")?),
    };
    // ⚠ Off the tick is refused, never rounded (I-2). A tick nobody gave is not checked: an
    // instrument the catalog does not list has no lot either, so nothing is sent for it; only a
    // catalog row that gives a lot and no tick is sent at the price as typed.
    if let Some(p) = price
        && let Some(tick) = inputs.grid.tick_size
        && !on_the_tick(p, tick)
    {
        let t = ladder::fmt_price(tick, Some(tick));
        return Err(format!(
            "{} is not on the {t} tick: use a multiple of {t}.",
            state.price.trim()
        ));
    }
    let at = entry_price(side, order_type, state, inputs).or_else(|| market_ref(side, inputs));
    let quantity =
        checked_quantity(order_quantity(side, order_type, state, inputs), state, inputs, at)?;
    // ⚠ BOTH exits or no order: a bracket that cannot be built refuses the order, never sends its
    // entry alone, where the old ticket sent a plain order with one leg blank.
    let exits =
        if take_profit_stop_loss { Some(bracket(side, order_type, state, inputs)?) } else { None };
    Ok(TradeWindowAction::Place {
        side,
        order_type,
        price,
        quantity,
        reduce_only: state.reduce_only,
        post_only: state.post_only,
        exits,
        origin: TradeWindowOrigin::Ticket,
    })
}

/// The exits TP/SL puts on an entry of `side`: both, or why not. [`order_with`] has refused a leg
/// that is not a number above zero before it asks ([`standing_refusal`]).
fn bracket(
    side: i32,
    order_type: TradeWindowTicketKind,
    state: &TradeWindowState,
    inputs: &TradeWindowInputs<'_>,
) -> Result<TradeWindowTicketExits, String> {
    let entry =
        entry_price(side, order_type, state, inputs).ok_or(TAKE_PROFIT_STOP_LOSS_ENTRY_WHY)?;
    let (take_profit, stop_loss) = legs(state).ok_or(TAKE_PROFIT_STOP_LOSS_LEGS_WHY)?;
    sizing::exits(side, entry, take_profit, stop_loss, inputs.grid.tick_size)
        .ok_or_else(|| TAKE_PROFIT_STOP_LOSS_ROOM_WHY.to_string())
}

/// A price for the price FIELD: on the tick, with no grouping, so it reads back as typed.
fn price_text(p: f64, inputs: &TradeWindowInputs<'_>) -> String {
    let step = inputs.grid.tick_size.or(super::grid_value(inputs.book.tick_size));
    format!("{:.*}", sizing::decimals_of(step.unwrap_or(p)), p)
}

/// The one price the size field's own arithmetic reads (its value, the share of buying power, a
/// quick size or a unit switch written in the quote currency): the typed price for a Limit or
/// Stop, else the last price, else the book's mid. An ORDER converts at its own price
/// ([`order_quantity`]), and the Buy and Sell buttons name that size.
fn reference_price(
    state: &TradeWindowState,
    order_type: TradeWindowTicketKind,
    inputs: &TradeWindowInputs<'_>,
) -> Option<f64> {
    let typed = if order_type == TradeWindowTicketKind::Market { None } else { typed_price(state) };
    [typed, inputs.last, inputs.book.mid()].into_iter().flatten().find(|p| known(*p))
}

/// Put the base size `base` into the size field, in the field's unit, at `at`. It writes nothing
/// for a size that is not above zero, nor for a quote value with no price to convert at: neither
/// `size_text`'s dash nor a `-0.00` is something the field may hold.
fn write_size(
    state: &mut TradeWindowState,
    base: f64,
    at: Option<f64>,
    inputs: &TradeWindowInputs<'_>,
) {
    if !known(base) {
        return;
    }
    let text = sizing::size_text(base, state.unit, at, inputs.grid.lot_size, inputs.value);
    if text != sizing::NO_QUOTE {
        state.size = text;
    }
}

/// The Buy/Sell label's three runs, the order [`order_quantity`] sizes: `Buy`, `0.010`, `limit` (or
/// `Sell`, `0.010`, `MKT`). The ticket draws the number in the mono face and names the button by
/// the three, a space apart.
fn side_words(
    side: i32,
    order_type: TradeWindowTicketKind,
    state: &TradeWindowState,
    inputs: &TradeWindowInputs<'_>,
    market_word: &str,
) -> (String, String, String) {
    let verb = if side > 0 { "Buy" } else { "Sell" };
    let q = order_quantity(side, order_type, state, inputs)
        .map_or_else(|| NO_SIZE.to_string(), |q| sizing::quantity_text(q, inputs.grid.lot_size));
    let kind = match order_type {
        TradeWindowTicketKind::Market => market_word,
        TradeWindowTicketKind::Limit => "limit",
        TradeWindowTicketKind::Stop => "stop",
    };
    (verb.to_string(), q, kind.to_string())
}

/// What a screen reader calls the full ticket's scrolling part (`trade_fit.rs` reads it too).
pub const FORM_NAME: &str = "Order form";
