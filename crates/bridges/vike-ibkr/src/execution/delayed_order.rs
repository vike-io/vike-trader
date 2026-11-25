//! **An order priced off DELAYED data may be a limit within a band, never a market order** (E07, the owner's standing
//! default of 2026-10-27; amends decision 0140's "An order MAY be priced off delayed data ... never a refusal").
//!
//! The owner's accounts hold no real-time stock entitlement, so every stock streams DELAYED, about 15 minutes behind
//! (`docs/ops/ibkr-socket-gateway.md`, "Delayed data prices a new stock"). Until E07 nothing judged an order against that
//! age: a MARKET order's notional, the account exposure and the buying-power check read a mark a quarter of an hour old,
//! and the price collar that would judge a limit against it was wired in no production path. The rule, applied here to
//! every order the venue is handed whose stream the core stamped non-real-time (`OrderRequest::data_delay`):
//!
//! * a MARKET order that OPENS exposure is refused, in words (it would trade at a price nobody can see). A
//!   market-to-limit is one (its first fill is a market fill, and its limit is that fill's price), and so is a
//!   market order at an auction (market-on-open, market-on-close: the auction's price is not seen either);
//! * a PEGGED order is refused, in words, whatever its anchor and WHETHER IT OPENS OR CLOSES (the owner's ruling of
//!   2026-11-09): the venue keeps its price at a live quote (the touch, the mid, the far side) that nobody here can see
//!   either, so the closing-order exemption below does not reach it;
//! * a LIMIT (or trigger) price that sits more than [`BAND`] (5 percent) from the delayed mark is refused, in words
//!   (a stop-limit and a limit-if-touched carry both, and either one outside refuses it; a market-if-touched is
//!   judged on its trigger, as a stop is; a DISCRETIONARY limit L is judged at L and at its worst price, L + d for a
//!   buy and L - d for a sell, which it may execute at; an ADJUSTABLE stop at its own trigger and at its target's stop
//!   and limit, which it may become, so at the worst of them);
//! * an order of any other kind that only CLOSES (`reduce_only`: a flatten, a margin call, a protective exit) is never
//!   refused for this, the collar's own rule (`vike_execution`'s `RiskGate`: a covered reduce is priced far from the mark by design);
//! * an order with no label, or a real-time one, is judged by nothing here: byte-identical to before.
//!
//! **Why it lives in the BRIDGE and not in the core's pre-trade.** The label is not journaled (`vike_core`'s
//! `runtime/data_delay.rs`: `Ingest::DataDelay` is not a journal record), so a refusal the core made on it would not be
//! reproduced by a replay of the same journal. A bridge refusal is a venue event (`OrderRejected`), which the journal
//! carries and the replay folds. It is also a one-venue rule, which belongs in that venue's bridge (the owner's ruling
//! of 2026-10-25).
//!
//! **Where the mark comes from.** The bridge's own quote pump notes the mid of every two-sided quote of a non-real-time
//! stream (`note_quote`, behind the feed features) in a process-wide table keyed by the symbol, and the exec loop reads the latest one no older
//! than [`MARK_FRESH`] ([`recent_mark`]). The gatewayhub hosts the price stream and the exec of an account in one process,
//! so the table the stream fills is the one the order reads. ⚠ **Fail-open on the band only:** an order with no recent mark
//! in the table (a stream this process does not run, a quiet stock) is not judged against a band, because there is nothing
//! to judge it against; the MARKET refusal needs no mark and always holds. ⚠ A price AMENDMENT (`Modify`) is not judged.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex, PoisonError};
use std::time::{Duration, Instant};

use vike_model::{OrderRequest, OrderStopAdjustTo, OrderType, StreamDelay};

/// The widest a limit or trigger price may sit from the delayed mark, as a fraction of it: the owner's 5 percent.
pub(crate) const BAND: f64 = 0.05;

/// How old a noted mark may be and still judge an order. The delayed feed ticks in bursts 1 to 25 seconds apart and a quiet
/// stock can say nothing for minutes, so the bound is generous; the label, not this age, says the price is a quarter of an
/// hour behind.
pub(crate) const MARK_FRESH: Duration = Duration::from_secs(300);

/// How many symbols the table holds: the engine's own cap on admitted symbols is far below it, so a full table means a
/// runaway caller, and the symbol past the cap is simply not noted (its band is then not judged).
#[cfg(any(feature = "ibkr-socket", feature = "ibkr-cpapi"))]
const MAX_MARKS: usize = 256;

/// The latest delayed mid per symbol (the symbol as the quote pump subscribed it, which is the engine's spelling) and when
/// it was noted.
static MARKS: LazyLock<Mutex<HashMap<String, (Instant, f64)>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// The quote pump saw a two-sided quote of `symbol` on a stream labelled `delay`: keep its mid when the stream is not
/// real-time. Per quote of a delayed stream (a few a minute), never of a real-time one: one label test and nothing else.
#[cfg(any(feature = "ibkr-socket", feature = "ibkr-cpapi"))]
pub(crate) fn note_quote(symbol: &str, delay: Option<StreamDelay>, bid: f64, ask: f64) {
    if !delay.is_some_and(|d| !d.is_real_time()) || !(bid > 0.0 && ask >= bid) {
        return;
    }
    let mid = (bid + ask) / 2.0;
    let mut marks = MARKS.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(slot) = marks.get_mut(symbol) {
        *slot = (Instant::now(), mid);
    } else if marks.len() < MAX_MARKS {
        marks.insert(symbol.to_string(), (Instant::now(), mid));
    }
}

/// The latest delayed mid noted for `symbol` no more than [`MARK_FRESH`] ago.
pub(crate) fn recent_mark(symbol: &str) -> Option<f64> {
    let marks = MARKS.lock().unwrap_or_else(PoisonError::into_inner);
    marks.get(symbol).filter(|(at, _)| at.elapsed() <= MARK_FRESH).map(|(_, mid)| *mid)
}

/// Why `req` is refused for being priced off non-real-time data, or `None` (the module doc's rule). `mark_of` is asked for
/// the symbol's delayed mark only when a price has to be judged, so an order that needs none costs no lookup.
pub(crate) fn refusal(
    req: &OrderRequest,
    mark_of: impl FnOnce(&str) -> Option<f64>,
) -> Option<String> {
    let delay = req.data_delay.filter(|d| !d.is_real_time())?;
    let (word, symbol) = (delay.word(), &req.symbol);
    // Asked before the closing-order exemption: a pegged order that only closes follows the unseen quote too.
    if req.order_type == OrderType::Pegged {
        return Some(format!(
            "ibkr: a PEGGED order on {symbol} is refused: its price is {word} data{}, and the venue keeps a pegged \
             order at a live quote nobody here can see, whatever its anchor, whether it opens or closes a position. \
             Send a LIMIT order priced within {:.0} percent of the {word} price",
            age_words(delay),
            BAND * 100.0
        ));
    }
    if req.reduce_only || !req.combo_legs.is_empty() {
        return None;
    }
    let market_kind = match req.order_type {
        OrderType::Market => Some("MARKET"),
        OrderType::MarketToLimit => Some("MARKET-TO-LIMIT"),
        _ => None,
    };
    if let Some(kind) = market_kind {
        return Some(format!(
            "ibkr: a {kind} order on {symbol} is refused: its price is {word} data{}, so a market order would trade at \
             a price nobody here can see. Send a LIMIT order priced within {:.0} percent of the {word} price (an order \
             that only closes a position, reduce-only, is never refused for this)",
            age_words(delay),
            BAND * 100.0
        ));
    }
    // A discretionary limit L may execute up to L + d for a buy, L - d for a sell: that worst price is judged too.
    let discretion = req.attributes.and_then(|a| a.discretionary_amount).zip(req.price);
    let worst = discretion.map(|(d, l)| if req.side >= 0 { l + d } else { l - d });
    // An adjustable stop may become its target: the target's stop and limit are judged too.
    let target = req.adjust.map(|a| match a.to {
        OrderStopAdjustTo::StopLimit { stop, limit } => [Some(stop), Some(limit)],
        OrderStopAdjustTo::Stop { stop } | OrderStopAdjustTo::Trailing { stop, .. } => {
            [Some(stop), None]
        }
    });
    let priced: Vec<f64> = [req.price, req.trigger_price, worst]
        .into_iter()
        .chain(target.into_iter().flatten())
        .flatten()
        .collect();
    if priced.is_empty() {
        return None;
    }
    let mark = mark_of(symbol.as_str()).filter(|m| m.is_finite() && *m > 0.0)?;
    let off = priced.into_iter().find(|p| !p.is_finite() || (p - mark).abs() > BAND * mark)?;
    Some(format!(
        "ibkr: the price {off} of the order on {symbol} is refused: it is {} percent from the {word} price {mark}{}, \
         outside the {:.0} percent band this node allows on {word} data. Correct the price",
        if off.is_finite() {
            format!("{:.1}", (off - mark).abs() / mark * 100.0)
        } else {
            "an unknown".into()
        },
        age_words(delay),
        BAND * 100.0
    ))
}

/// ` (about 15 minutes old)` for a delayed stream, `` for a frozen one (the last value the venue holds, from the close or
/// earlier): the one figure IBKR's pages give for stock data without a real-time entitlement.
fn age_words(delay: StreamDelay) -> &'static str {
    match delay {
        StreamDelay::Delayed { .. } => " (about 15 minutes old)",
        StreamDelay::RealTime | StreamDelay::Frozen => "",
    }
}

#[cfg(test)]
#[path = "delayed_order_tests.rs"]
mod delayed_order_tests;
