//! The control door's POST-ONLY CROSSING check (decision 0242, item 8): a post-only LIMIT order whose price
//! would cross the book the node already holds is refused BY NAME before anything is sent, with both numbers.
//!
//! **Why here.** A post-only order priced through the book is a limit order that BECOMES a market order, and the
//! node already refuses to place an order until it has data from the venue. Binance, Deribit and Hyperliquid
//! refuse a crossing post-only order themselves (a typed `RejectCause::PostOnlyWouldCross` on the order), but
//! Bybit and OKX ACCEPT it and cancel it within a millisecond, which the node recorded as a plain `CANCELED`
//! with no reason. This is the one rule for every venue that has a post-only order: the door meets the order's
//! price with the best bid and best ask its own price tap last saw ([`SymbolFacts::top_of_book`]) and says so,
//! in the same words whatever the venue. The venue's own post-only stays the last guard for a book that moved in
//! the last milliseconds.
//!
//! **What it is not.**
//! * Not a guess: a tap that holds only a last price or a mid (a trade print, a one-sided quote) or whose top is
//!   older than the tap's price bound answers `None`, and the order goes to the venue to be judged. A mid is
//!   never read as a bid and an ask.
//! * Not for a plain limit order (a marketable plain limit is legal), a market or stop order (refused elsewhere:
//!   `vike_model::lifetime_refusal`), a venue with no post-only order (the core's capability row refuses it by
//!   name, `TIF_UNSUPPORTED`), or a `paper` book (it keeps its own law, `vike_fills`' `post_only_would_cross`
//!   against what its last bar offered; this door reads the same predicate and a LIVE book).
//! * Not in the core: the check is a read of the tap on a connection thread, once per post-only order, ahead of
//!   the command lane. Nothing is added to the fold, `run_submit_hop`, the dispatch or the publish; and a refusal
//!   here is never journaled, so a replay (which holds no prices) cannot disagree with the run that refused.
//! * Not an order row: a refusal at the door creates no order, so there is no `OrderRejected` to carry the typed
//!   cause; the refusal reaches the operator as the door's answer (the CLI's error, the desktop strip's refusal
//!   line, the Telegram reply), the same words everywhere.

use vike_exec::VenueBlock;
use vike_model::accounts::account_keys::{AccountLabel, parse_wire_account, route_key_of};
use vike_model::{AccountTier, OrderType, TimeInForce};
use vike_tradehub_client::wire::WireCommand;

use super::symbol_admission::SymbolFacts;

/// The refusal for `cmd`, or `None` when it is not a post-only limit order that the node's own book shows would
/// cross (see the module doc for every case that answers `None`). `symbol` is the canonical spelling the order
/// goes out on when the plan changed it (the preview's), else the command's own.
pub(super) fn post_only_cross_refusal(
    cmd: &WireCommand,
    symbol: Option<&str>,
    blocks: &[VenueBlock],
    facts: Option<&dyn SymbolFacts>,
) -> Option<String> {
    let WireCommand::Submit(req) = cmd else { return None };
    if !matches!(TimeInForce::parse(req.time_in_force.as_deref()?), Ok(TimeInForce::PostOnly))
        || !matches!(OrderType::parse(&req.order_type), Ok(OrderType::Limit))
        || !vike_venues::caps_for(&req.venue).supports_post_only
    {
        return None;
    }
    let limit = req.price.filter(|p| p.is_finite() && *p > 0.0)?;
    let label = match req.account.as_deref() {
        None => AccountLabel::Default,
        // A label this gate cannot parse belongs to `lower_command`'s refusal.
        Some(text) => parse_wire_account(text).ok()?,
    };
    let key = route_key_of(&req.venue, &label);
    let engine = blocks.iter().find(|b| b.route_key == key)?;
    // A `paper` book judges its own post-only orders (one law with the simulator); an engine that says no tier
    // is not assumed to be a venue's.
    if !matches!(engine.tier, Some(AccountTier::Demo | AccountTier::Live)) {
        return None;
    }
    let symbol = symbol.unwrap_or(&req.symbol);
    let (bid, ask) = facts?
        .top_of_book(&key, &req.venue, symbol)
        .filter(|(bid, ask)| bid.is_finite() && ask.is_finite() && *bid > 0.0 && ask >= bid)?;
    let (word, taker, touched) = if req.side > 0 {
        ("buy", ask, "ask")
    } else if req.side < 0 {
        ("sell", bid, "bid")
    } else {
        return None;
    };
    vike_fills::fill_model::post_only_would_cross(req.side, limit, taker).then(|| {
        let rest = if req.side > 0 { "below the ask" } else { "above the bid" };
        format!(
            "post-only {word} {limit} would cross the best {touched} {taker} ({symbol} on {venue}): it \
             would execute immediately as a taker; the order was NOT sent. Price it {rest} to rest in the \
             book as a maker, or send a plain limit order to take liquidity",
            venue = req.venue
        )
    })
}
