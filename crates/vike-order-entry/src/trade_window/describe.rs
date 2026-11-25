//! The words the window says about an order and about itself: an order as the confirm prompt says
//! it, and why the window takes no order now (spec §4.3).

use super::{
    TradeWindowAction, TradeWindowInputs, TradeWindowTicketKind, TradeWindowTradable, ladder,
    sizing,
};
use vike_model::Grid;

/// The words for an order, as the confirm prompt says them, with what it carries:
/// `Buy 0.010 limit @ 65,432.4, TP 65,759.6 / SL 65,236.1`, or `…, no TP/SL` (fix round 1, I-3:
/// the confirm names the exits, or says there are none).
pub fn describe(a: &TradeWindowAction, grid: Grid) -> String {
    match describe_parts(a, grid, None) {
        (order, Some(exits)) => format!("{order}, {exits}"),
        (order, None) => order,
    }
}

/// [`describe`]'s two halves: the order (side, size, type and price), and what it carries (`TP … /
/// SL …` or `no TP/SL`, after `reduce only` for an order that is), `None` for an action that
/// carries no exits at all (Close, Reverse). The status strip prints them on two lines, so a narrow
/// strip can never cut the exits off.
///
/// A Close or a Reverse names the side and the size it sends where `position` (the signed size it
/// was asked against) is given — the side opposite the position, and its size, twice it for a
/// Reverse, as the app's dispatcher sends them from the position at the confirm (final review A,
/// minor 3) — and says only what it does where it is not.
pub(super) fn describe_parts(
    a: &TradeWindowAction,
    grid: Grid,
    position: Option<f64>,
) -> (String, Option<String>) {
    match a {
        TradeWindowAction::Place {
            side,
            order_type,
            price,
            quantity,
            reduce_only,
            post_only,
            exits,
            ..
        } => {
            let verb = if *side > 0 { "Buy" } else { "Sell" };
            // Never "Buy 0 limit" for a lot no longer known (final review A, minor 8).
            let q = sizing::quantity_text(*quantity, grid.lot_size);
            let order = match (order_type, price) {
                (TradeWindowTicketKind::Market, _) => format!("{verb} {q} at market"),
                // ⚠ NOT "at market": the app's dispatcher REFUSES a priceless Limit/Stop
                // (`OrderEntryDispatchRejectReason::MissingPrice`), so the confirm must not promise a
                // market fill it will never send. This arm used to share the Market wording.
                (TradeWindowTicketKind::Limit, None) => {
                    format!("{verb} {q} limit, needs a price: not sent")
                }
                (TradeWindowTicketKind::Stop, None) => {
                    format!("{verb} {q} stop, needs a price: not sent")
                }
                (TradeWindowTicketKind::Limit, Some(p)) => {
                    format!("{verb} {q} limit @ {}", ladder::fmt_price(*p, grid.tick_size))
                }
                (TradeWindowTicketKind::Stop, Some(p)) => {
                    format!("{verb} {q} stop @ {}", ladder::fmt_price(*p, grid.tick_size))
                }
            };
            let carries = match exits {
                Some(e) => format!(
                    "TP {} / SL {}",
                    ladder::fmt_price(e.take_profit, grid.tick_size),
                    ladder::fmt_price(e.stop_loss, grid.tick_size)
                ),
                None => "no TP/SL".to_string(),
            };
            // A held order keeps the post-only and the reduce-only it was made with, whatever the
            // boxes show by the time the trader confirms it: the prompt says which (final review A,
            // minor 2).
            let carries = if *post_only { format!("post only, {carries}") } else { carries };
            let carries = if *reduce_only { format!("reduce only, {carries}") } else { carries };
            (order, Some(carries))
        }
        TradeWindowAction::ClosePosition | TradeWindowAction::Reverse => {
            let reverse = matches!(a, TradeWindowAction::Reverse);
            let what = if reverse { "Reverse the position" } else { "Close the position" };
            let order = match position.filter(|s| *s != 0.0 && s.is_finite()) {
                Some(s) => {
                    let verb = if s > 0.0 { "sell" } else { "buy" };
                    let q = if reverse { 2.0 * s.abs() } else { s.abs() };
                    format!("{what}: {verb} {} at market", sizing::quantity_text(q, grid.lot_size))
                }
                None => format!("{what} at market"),
            };
            (order, None)
        }
        _ => (String::new(), None),
    }
}

/// The line an untradable symbol shows in place of the order controls (spec §4.3). The ladder and
/// the ticket both say it, so it lives here rather than in either. An EMPTY list proves nothing
/// about the symbol, so it says the account cannot trade it now — never "does not trade" (W4 fix
/// round 1, I-3; final review A, minor 6). A long list names its first two symbols and counts the
/// rest (`NAMED`; FW6, I4): `It trades ETH-PERPETUAL, SOL-PERPETUAL and 3 more.`
///
/// Private: only [`why_untradable`] may call it, because the two guards live there (a cause the
/// app states, and a list that names the symbol itself).
pub(super) fn untradable_reason(symbol: &str, trades: &[String]) -> String {
    if trades.is_empty() {
        cannot_trade_now(symbol)
    } else {
        format!("This account does not trade {symbol}. It trades {}.", traded_list(trades))
    }
}

/// How many of an account's symbols [`untradable_reason`] names before it counts the rest: with
/// the sentence's other words, two long venue symbols hold the status strip's two lines in a
/// 280 pt window, where a whole list ran on past them (FW6, I4: the render check's 320 pt strip cut
/// it to `…It trades ETHUS…`).
const NAMED: usize = 2;

/// `trades` as a sentence names them: every one while there are at most one more than [`NAMED`]
/// ("and 1 more" says less than the name it hides), else the first [`NAMED`] and how many more.
fn traded_list(trades: &[String]) -> String {
    match trades.split_at_checked(NAMED) {
        Some((named, rest)) if rest.len() > 1 => {
            format!("{} and {} more", named.join(", "), rest.len())
        }
        _ => trades.join(", "),
    }
}

/// That the account cannot trade `symbol` now: what a refusal says with no cause stated and no
/// list that names other symbols.
fn cannot_trade_now(symbol: &str) -> String {
    format!("This account cannot trade {symbol} now.")
}

/// Why the window takes no order now, or `None` when it takes them ([`TradeWindowTradable::Yes`]). Every
/// place that refuses because of [`TradeWindowTradable::No`] says THIS: the ticket's line and its disabled
/// buttons, the ladder's hint and its hover, the compact ticket's line.
///
/// The cause is the app's to state, never the widget's to guess (the F wave's ruling):
/// - a cause the app states (`why`: a stopped core, a halted account, an account the server does
///   not run, …) is said in the app's words — the words its status strip uses for it;
/// - with none stated, a list that names other symbols and not this one: `untradable_reason`,
///   which is then true — the only case that may say "does not trade";
/// - otherwise: that the account cannot trade the symbol NOW. An EMPTY list never proves "does
///   not trade" (W4 fix round 1, I-3), and a list that names the symbol cannot say it without
///   contradicting itself.
///
/// ⚠ It never reads [`TradeWindowInputs::status`]. The strip also carries the window's own last
/// rejection, a widget note, a refused ladder click: an error there is not, by being there, the
/// reason the window takes no order (W4 fix round 2's two wrong cases, ended by the slot).
pub(crate) fn why_untradable(inputs: &TradeWindowInputs<'_>) -> Option<String> {
    let TradeWindowTradable::No { trades, why } = inputs.tradable else {
        return None;
    };
    let names_others = !trades.is_empty() && !trades.iter().any(|s| s == inputs.symbol);
    Some(match why {
        Some(why) => why.to_string(),
        None if names_others => untradable_reason(inputs.symbol, trades),
        None => cannot_trade_now(inputs.symbol),
    })
}
