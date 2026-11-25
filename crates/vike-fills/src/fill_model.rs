//! Pluggable fill-price models, selected by available data (tiered fill engine).
//!
//! - [`FillModelBar`] (no ticks): delegate to `order_fill_price` — the pre-tick behavior.
//! - [`FillModelTick`] (L1 quote bars): market orders cross the real spread (buy@ask /
//!   sell@bid) when the bar carries bid/ask; single-quote-tick bars (high == low) trigger
//!   resting orders off the quote side; consolidated bars delegate.
//! - [`FillModelL2Book`] (L1 + L2 book): a taker is priced by CONSUMING the resting book, not
//!   by reading one tick price.
//! - [`trailing_path_fill`] (a trailing order with an ACTIVATION level, on a bar): the bar walked
//!   as a path, the one walk the simulator and the paper book share; a caller sends such an order
//!   there instead of to its tier, and on a tick only activates it ([`trailing_activation_reached`]).
//! - [`side_bar`] (a bar carrying the [`BarSides`] the venue's quotes drew over its window): which
//!   path an order is judged on, a buy the ask's and a sell the bid's, so the bar laws above fill it
//!   like the venue, never at the mid or a print (the paper book's live bars; owner 2026-11-16).
//!
//! All tiers return a RAW price; the engine still applies `adverse_fill_price` slippage and
//! routes the fill through `compute_fill` (parity preserved).
//!
//! # Why the L2 tier exists (and why it is not one strategy's bug)
//!
//! Every tier above it prices a taker at a price NOBODY WAS OFFERING: [`FillModelTick`] fills a
//! market buy at `bar.ask` (the top of book, at any size), and on a quote-less trade tape
//! `order_fill_price` fills at the PRINT, where somebody else already took the liquidity. A
//! taker can obtain neither. The error always favours the strategy and scales with size, because
//! top-of-book is the cheapest share on the ladder.
//!
//! Measured on the Polymarket `btc-updown-5m` tape over 835 entries (`cheap_np_depth`, PR #650):
//! the tape print averaged **0.2683** while the resting best ask at the same instant averaged
//! **0.3063**, and in 738 of 835 cases there was NO resting ask at the printed price at all. That
//! is a ~4 cent per-share error on a strategy whose whole edge bar is 5.5 cents — and it is not
//! specific to that strategy. Any strategy that takes liquidity on the tick path is mispriced the
//! same way. Hence the fix lives HERE, in the engine, and not in a strategy.
//!
//! [`FillModelL2Book`] is ADDITIVE: it is reached only via `FillModelKind::L2Book`, which nothing
//! selects by default, so every existing model, test and published number is byte-identical.

// Both laws live in vike-model and are named there, never re-exported here: `order_fill_price` is
// the fill-trigger oracle (shared with the vike-core OrderEmulationBook emulator), and
// `vike_marketdata::book_taker_price` is THE taker-price law — a property of an order book, not of a
// simulation, that the live path needs too (pre-trade sizing, checking realised fills).
use vike_marketdata::{Bar, BarSides, L2Book};
use vike_model::{
    ContractValue, OrderKind, OrderResting, limit_part, order_fill_price,
    trailing_activation_reached,
};

pub trait FillModel {
    fn fill_price(&self, order: &mut OrderResting, bar: &Bar) -> Option<f64>;

    /// The BOOK-AWARE fill price. Defaults to [`FillModel::fill_price`], so a tier with no use for
    /// depth is unchanged; only [`FillModelL2Book`] overrides it.
    fn fill_price_book(
        &self,
        order: &mut OrderResting,
        bar: &Bar,
        book: Option<&L2Book>,
    ) -> Option<f64> {
        let _ = book;
        self.fill_price(order, bar)
    }
}

/// Bar-level fills (no tick data) — identical to the pre-tick engine.
#[derive(Debug, Default, Clone, Copy)]
pub struct FillModelBar;

impl FillModel for FillModelBar {
    fn fill_price(&self, order: &mut OrderResting, bar: &Bar) -> Option<f64> {
        order_fill_price(order, bar)
    }
}

/// L1 spread-crossing fill model.
#[derive(Debug, Default, Clone, Copy)]
pub struct FillModelTick;

impl FillModel for FillModelTick {
    fn fill_price(&self, order: &mut OrderResting, bar: &Bar) -> Option<f64> {
        let has_quote = bar.bid.is_some() && bar.ask.is_some();
        let buy = order.side > 0;
        let kind = order.kind;
        // Slice-1 preserved: a MARKET order crosses the real spread whenever quotes are
        // present, for ANY bar shape (consolidated multi-tick bar or single tick). A
        // market-to-limit is that market order on its first event: an L1 quote has no depth, so it
        // fills whole there.
        if matches!(kind, OrderKind::Market | OrderKind::MarketToLimit) && has_quote {
            return Some(if buy { bar.ask.unwrap() } else { bar.bid.unwrap() });
        }
        // Slice-2: quote-side triggering for resting orders + market_close, ONLY for a SINGLE
        // quote tick (high == low). A consolidated bar (high != low) delegates.
        if has_quote && bar.high == bar.low {
            let (bid, ask) = (bar.bid.unwrap(), bar.ask.unwrap());
            let quote = if buy { ask } else { bid };
            match kind {
                OrderKind::MarketClose => return Some(quote),
                // a discretionary limit is judged at its discretion (`order_fill_price`'s law)
                OrderKind::Limit | OrderKind::LimitClose => {
                    let price = order.discretion_limit(order.price.expect("limit requires price"));
                    return if buy {
                        if ask <= price { Some(quote) } else { None }
                    } else if bid >= price {
                        Some(quote)
                    } else {
                        None
                    };
                }
                // a stop the quote does not fire is adjusted when the quote reached its level
                OrderKind::Stop => {
                    let price = order.price.expect("stop requires price");
                    let fill = if buy {
                        if ask >= price { Some(quote) } else { None }
                    } else if bid <= price {
                        Some(quote)
                    } else {
                        None
                    };
                    if fill.is_none() {
                        vike_model::apply_stop_adjust(order, quote, quote);
                    }
                    return fill;
                }
                OrderKind::Trailing => {
                    return quote_trailing_fire(order, bid, ask)
                        .map(|_| if order.side < 0 { bid } else { ask });
                }
                // `order_fill_price`'s trailing-limit law on the quote: it fires as the trailing
                // stop above does, its limit is set off the stop it fired at, and the limit part
                // fills at the quote within L, else the order rests as the Limit at L.
                OrderKind::TrailingLimit => {
                    let stop = quote_trailing_fire(order, bid, ask)?;
                    let limit = order.trailing_limit_at(stop);
                    order.limit = Some(limit);
                    if (buy && quote <= limit) || (!buy && quote >= limit) {
                        return Some(quote);
                    }
                    order.rest_as_limit();
                    return None;
                }
                // `order_fill_price`'s trailing take-profit law on the quote: the mirror of the
                // trailing stop's (a sell trails the lowest bid and fires on a bid at or above T, a
                // buy the highest ask); with an offset, the limit part as the trailing limit's.
                OrderKind::TrailingTakeProfit => {
                    let trigger = quote_trailing_touch(order, bid, ask)?;
                    if order.limit_offset.is_none() {
                        return Some(quote);
                    }
                    let limit = order.trailing_limit_at(trigger);
                    order.limit = Some(limit);
                    if (buy && quote <= limit) || (!buy && quote >= limit) {
                        return Some(quote);
                    }
                    order.rest_as_limit();
                    return None;
                }
                // `order_fill_price`'s stop-limit / if-touched law on the quote: the stop-limit
                // triggers as the stop above does, the take-profit as the limit does; then a
                // take-profit with no limit fills at the quote, and the limit part fills at the
                // quote when it is within L, else the order rests as the Limit at L.
                OrderKind::StopLimit | OrderKind::TakeProfit => {
                    let trigger = order.price.expect("a trigger kind requires price");
                    // a stop-limit buy and a take-profit sell fire on a quote at or above T
                    let fires_rising = (kind == OrderKind::StopLimit) == buy;
                    let fired = if fires_rising { quote >= trigger } else { quote <= trigger };
                    if !fired {
                        vike_model::apply_stop_adjust(order, quote, quote);
                        return None;
                    }
                    if kind == OrderKind::TakeProfit && order.limit.is_none() {
                        return Some(quote);
                    }
                    let limit = order.limit.expect("stop_limit requires limit");
                    if (buy && quote <= limit) || (!buy && quote >= limit) {
                        return Some(quote);
                    }
                    order.rest_as_limit();
                    return None;
                }
                // unreachable: handled above when has_quote
                OrderKind::Market | OrderKind::MarketToLimit => {}
                // an auction clears at ONE price, the traded open or close: no side of the quote
                // is crossed, so the bar law below prices it
                OrderKind::AuctionOpen
                | OrderKind::AuctionClose
                | OrderKind::AuctionOpenToLimit => {}
            }
        }
        order_fill_price(order, bar)
    }
}

/// The trailing law on ONE quote tick, for `Trailing` and `TrailingLimit`: `Some(stop)` when the
/// quote reaches the stop the extreme implies, else the extreme ratchets with the quote and `None`.
fn quote_trailing_fire(order: &mut OrderResting, bid: f64, ask: f64) -> Option<f64> {
    // side<0 protects a long (sell-stop trailing the bid);
    // side>0 protects a short (buy-stop trailing the ask).
    let extreme = order.extreme.expect("trailing requires extreme");
    let trail = order.trail_distance(extreme);
    if order.side < 0 {
        let trigger = extreme - trail;
        if bid <= trigger {
            return Some(trigger);
        }
        order.extreme = Some(extreme.max(bid));
        return None;
    }
    let trigger = extreme + trail;
    if ask >= trigger {
        return Some(trigger);
    }
    order.extreme = Some(extreme.min(ask));
    None
}

/// The trailing take-profit law on ONE quote tick: `Some(trigger)` when the quote reaches, from the
/// favourable side, the trigger the extreme implies (a sell's bid at or above low + trail, a buy's ask
/// at or below high − trail), else the extreme ratchets with the quote and `None`.
fn quote_trailing_touch(order: &mut OrderResting, bid: f64, ask: f64) -> Option<f64> {
    let extreme = order.extreme.expect("trailing_take_profit requires extreme");
    let trail = order.trail_distance(extreme);
    if order.side < 0 {
        let trigger = extreme + trail;
        if bid >= trigger {
            return Some(trigger);
        }
        order.extreme = Some(extreme.min(bid));
        return None;
    }
    let trigger = extreme - trail;
    if ask <= trigger {
        return Some(trigger);
    }
    order.extreme = Some(extreme.max(ask));
    None
}

/// **The trailing PATH walk**: one bar of a `Trailing` or `TrailingLimit` order that has an
/// activation level. ONE walk for the simulator and the paper book, so the two cannot disagree.
///
/// **Inputs**, all on `o` but the bar:
/// - `bar`, the path: open, the nearer extreme, the farther one, close (an exact tie goes low first;
///   unmeasured: no measured exit depends on it);
/// - `o.side`: a sell (`< 0`) trails the high and fires on the way down, a buy the low, on the way up;
/// - `o.trail`: the distance in price, required unless `o.trail_percent` states it as a percent of the
///   extreme ([`OrderResting::trail_distance`]);
/// - `o.activation`: the level, REQUIRED (an order with none is not this walk's: it takes
///   `vike_model::order_fill_price`'s plain law); [`trailing_activation_reached`] judges each point;
/// - `o.extreme`: `None` until the level is reached (set at the first point that reaches it), the
///   best price since then;
/// - `o.limit_offset`: a `TrailingLimit`'s offset, required there.
///
/// **Output**: `Some(price)` when the order fires on this bar and fills, else `None`. Each point
/// moves the extreme only after the stop P it implies (`extreme - trail` for a sell, `extreme +
/// trail` for a buy) has been tested; P fires at its own level, or at the point before it when a gap
/// already carried the price past it (only an open can do that): the fire price A. A `Trailing`
/// fills at A. A `TrailingLimit` sets `o.limit` to L ([`OrderResting::trailing_limit_at`] of P) and
/// takes the stop-limit's limit part with that A ([`limit_part`], decision 0257): the better of L
/// and A when the bar reaches L, else `None` and the order RESTS, in place, as the plain `Limit` at
/// L ([`OrderResting::rest_as_limit`]). A is a price the path traded and L is no better than P, so on
/// one bar only an open gapped past L rests it; finer data (a sub-bar, a tick) can show a hole
/// anywhere. Mutates `o.extreme`, and a fired `TrailingLimit`'s `limit` (and kind, when it rests).
///
/// ⚠ **A caller walks it on EVERY bar the order lives, not only the one that activates it.** The
/// plain arm of `vike_model::order_fill_price` tests the PRIOR bar's extreme and ratchets afterwards,
/// so it cannot stop on a high made inside the same bar. Measured against one reference list of 579
/// trailing exits: 579 exact with this walk, 441 with the plain arm after activation. A tick has no
/// path: there a caller only activates the order on its quote and hands it to its tier's own law.
pub fn trailing_path_fill(o: &mut OrderResting, bar: &Bar) -> Option<f64> {
    let level = o.activation.expect("the trailing path walk requires an activation");
    let (stop, fired) = path_fire(o, level, bar)?;
    if o.kind != OrderKind::TrailingLimit {
        return Some(fired);
    }
    o.limit = Some(o.trailing_limit_at(stop));
    limit_part(o, bar, fired)
}

/// **Which of several triggered legs of ONE order list fills first inside one bar**: the indices of
/// `prices` in the order the bar's path (the one [`trailing_path_fill`] walks) first reaches each,
/// a tie keeping the given order. `prices[i]` is where leg `i` fills on this bar: its level the
/// path crosses, or the open it gapped past (reached at once). ONE rule for the simulator and the
/// paper book (two entries of a straddle in one bar: the first to fill is the one its OCO or OUO
/// link resolves first), and an ASSUMPTION, as any order inside one bar is.
#[must_use]
pub fn path_order(bar: &Bar, prices: &[f64]) -> Vec<usize> {
    let [open, near, _, _] = bar.path_points();
    // the distance the path travels before it first prints `px`
    let reach = |px: f64| {
        if (px - open) * (near - open) >= 0.0 && (px - open).abs() <= (near - open).abs() {
            (px - open).abs()
        } else {
            (near - open).abs() + (px - near).abs()
        }
    };
    let mut order: Vec<usize> = (0..prices.len()).collect();
    order.sort_by(|&a, &b| reach(prices[a]).total_cmp(&reach(prices[b])));
    order
}

/// [`trailing_path_fill`]'s walk: `Some((stop P, fire price A))` on the point that fires it.
fn path_fire(o: &mut OrderResting, level: f64, bar: &Bar) -> Option<(f64, f64)> {
    let mut previous = bar.open;
    for point in bar.path_points() {
        match o.extreme {
            None if trailing_activation_reached(o.side, point, level) => {
                o.extreme = Some(point);
            }
            None => {}
            Some(extreme) if o.side < 0 => {
                // a percent trail's distance moves with the extreme (`OrderResting::trail_distance`)
                let trigger = extreme - o.trail_distance(extreme);
                if point <= trigger {
                    return Some((trigger, trigger.min(previous)));
                }
                o.extreme = Some(extreme.max(point));
            }
            Some(extreme) => {
                let trigger = extreme + o.trail_distance(extreme);
                if point >= trigger {
                    return Some((trigger, trigger.max(previous)));
                }
                o.extreme = Some(extreme.min(point));
            }
        }
        previous = point;
    }
    None
}

/// L1 + **L2** fills: the trigger law of [`FillModelTick`], the PRICE from walking the book.
///
/// * **WHEN** an order fills is unchanged — [`FillModelTick`] decides it, including the stop /
///   trailing trigger arithmetic and its `extreme` bookkeeping.
/// * **WHAT PRICE** it fills at is re-derived by `vike_marketdata::book_taker_price`: the VWAP of
///   consuming the resting book for the order's OWN SIZE, best level outward (harmonic on an
///   inverse contract, [`FillModelL2Book::fill_price_book_valued`]). A resting LIMIT also caps the
///   walk at its own price, so only liquidity it would genuinely have crossed can fill it.
///
/// **No book, no change.** With `book == None` this tier IS [`FillModelTick`], byte for byte, so
/// mixing a book-carrying symbol and a bookless one in a single run is well-defined.
///
/// **Insufficient depth does not fill.** When the displayed book cannot cover the order's size
/// (within its limit, if any) the order stays resting and waits for liquidity — the one
/// behavioural difference a strategy will notice, and the honest one: the alternative fills size
/// the market never showed. It is also the venue semantics this tier was built for: an order
/// marketable when the strategy decides but not when the matching engine looks **rests on the
/// book** rather than filling or vanishing. Composed with `vike_simulator::LatencyModelKind`, the
/// "decide at `T`, match at `T + Δ`" rule falls out — see
/// `vike_simulator::LatencyModelKind::venue_hold_ms`.
#[derive(Debug, Default, Clone, Copy)]
pub struct FillModelL2Book;

impl FillModel for FillModelL2Book {
    /// Bookless: exactly [`FillModelTick`]. See the type doc — the tier degrades, it does not
    /// invent.
    fn fill_price(&self, order: &mut OrderResting, bar: &Bar) -> Option<f64> {
        FillModelTick.fill_price(order, bar)
    }

    /// [`FillModelL2Book::fill_price_book_valued`] at the default linear unit.
    fn fill_price_book(
        &self,
        order: &mut OrderResting,
        bar: &Bar,
        book: Option<&L2Book>,
    ) -> Option<f64> {
        self.fill_price_book_valued(order, bar, book, ContractValue::default())
    }
}

impl FillModelL2Book {
    /// The book-aware fill price of an order on a contract worth `value` a unit: every walk is
    /// [`ContractValue::taker_price`], `book_taker_price` itself on a linear contract (bit for bit)
    /// and the walked levels' HARMONIC average on an inverse one, whose cost is the coin each level
    /// takes (`ContractValue::average_price`).
    pub fn fill_price_book_valued(
        &self,
        order: &mut OrderResting,
        bar: &Bar,
        book: Option<&L2Book>,
        value: ContractValue,
    ) -> Option<f64> {
        let Some(book) = book else { return FillModelTick.fill_price(order, bar) };
        match order.kind {
            // A market taker crosses whatever is resting, uncapped, for its own size.
            OrderKind::Market | OrderKind::MarketClose => {
                value.taker_price(book, order.side, order.size, None)
            }
            // A marketable limit crosses only up to its own price; one the book has moved away from
            // gets `None` (the within-limit depth cannot cover it) and therefore RESTS.
            OrderKind::Limit | OrderKind::LimitClose => {
                // a discretionary limit crosses up to its discretion
                let price = order.discretion_limit(order.price?);
                value.taker_price(book, order.side, order.size, Some(price))
            }
            // Stops and trailing stops keep the L1 TRIGGER law verbatim (it owns the `extreme`
            // mutation), then are priced as the takers they are; size the book cannot cover leaves
            // the stop armed for the next event, like every other kind here.
            OrderKind::Stop | OrderKind::Trailing => {
                FillModelTick.fill_price(order, bar)?;
                value.taker_price(book, order.side, order.size, None)
            }
            // A stop-limit, if-touched, trailing-limit or trailing take-profit order keeps the L1 law
            // for its trigger (and for its limit at the quote), then walks the book capped at its
            // limit: depth the limit cannot cover rests it as the `Limit` at L. With no limit it walks
            // uncapped and, like a stop, stays armed for the next event when the book cannot cover it.
            OrderKind::StopLimit
            | OrderKind::TakeProfit
            | OrderKind::TrailingLimit
            | OrderKind::TrailingTakeProfit => {
                FillModelTick.fill_price(order, bar)?;
                let px = value.taker_price(book, order.side, order.size, order.limit);
                if px.is_none() && order.limit.is_some() {
                    order.rest_as_limit();
                }
                px
            }
            // A market-to-limit takes ONLY the best opposite level, at its price, and its limit is
            // that price. This tier fills whole or not at all, so a best level that cannot cover the
            // size rests the WHOLE order as the `Limit` there (no partial fill to keep). An empty
            // side leaves it a market-to-limit for the next event.
            OrderKind::MarketToLimit => {
                let best = if order.side > 0 { book.best_ask() } else { book.best_bid() }?;
                order.limit = Some(best.price);
                let px = value.taker_price(book, order.side, order.size, order.limit);
                if px.is_none() {
                    order.rest_as_limit();
                }
                px
            }
            // An auction clears at one price, not by walking the book that stood before it.
            OrderKind::AuctionOpen | OrderKind::AuctionClose | OrderKind::AuctionOpenToLimit => {
                FillModelTick.fill_price(order, bar)
            }
        }
    }
}

/// **The post-only law**: would a LIMIT order at `limit_px` TAKE liquidity against `taker_px`, the
/// price the other side of the book is offering right now (the ask for a buy, the bid for a sell)?
/// A post-only order that would is REFUSED when it arrives — it never executes, at the limit or at
/// the better price, and it never rests (a venue answers a crossing post-only order with a
/// rejection or a cancel, never a taker fill). Touching the price counts as crossing: a buy at the
/// ask lifts it. `side` is the workspace sign (+1 buy / -1 sell); a non-finite price, or a side
/// that is neither, crosses nothing (`false`: the caller's own price checks refuse those).
///
/// ONE law for the simulator (`vike_simulator`'s `OrderBrokerSimulator::submit_limit_post_only`) and the paper
/// exchange (`vike_paper`'s `submit`), so backtest == paper holds for a maker-only order too.
#[must_use]
pub fn post_only_would_cross(side: i32, limit_px: f64, taker_px: f64) -> bool {
    match side.signum() {
        1 => limit_px >= taker_px,
        -1 => limit_px <= taker_px,
        _ => false,
    }
}

/// The taker price a post-only check reads off a bar: the quoted ask (buy) or bid (sell) when the
/// bar carries one, else its close (the last price traded is the only reference a quote-less bar
/// has). `side` as in [`post_only_would_cross`].
#[must_use]
pub fn post_only_reference(side: i32, bar: &Bar) -> f64 {
    let quote = if side > 0 { bar.ask } else { bar.bid };
    quote.unwrap_or(bar.close)
}

/// **The quote-side law** (owner, 2026-11-16: "paper fills like the real venue, never at the mid"):
/// the path an order of `side` and `kind` is judged on, when its bar carries the [`BarSides`] the
/// venue's quotes drew over its window (a mid bar, a candle of trades or of mids alike). A BUY meets
/// the ASK's path and a SELL the BID's, for every kind, so a
/// market buy fills at the ask, a buy limit only once the ask reaches it, a sell stop triggers on
/// the bid, and the bar laws ([`order_fill_price`], [`trailing_path_fill`]) run unchanged on that
/// side's prices. On a window of ONE quote this is [`FillModelTick`]'s quote rule exactly (the tick
/// tier the simulator fills quote ticks with). An auction clears at one traded price and crosses no
/// side ([`FillModelTick`]'s rule): it keeps `bar`. No sides (every stored bar a backtest replays, a
/// bar no quote came beside): `bar` itself, so nothing changes.
#[must_use]
pub fn side_bar<'a>(
    side: i32,
    kind: OrderKind,
    bar: &'a Bar,
    sides: Option<&'a BarSides>,
) -> &'a Bar {
    match sides {
        None => bar,
        Some(_)
            if matches!(
                kind,
                OrderKind::AuctionOpen | OrderKind::AuctionClose | OrderKind::AuctionOpenToLimit
            ) =>
        {
            bar
        }
        Some(sides) if side > 0 => &sides.ask,
        Some(sides) => &sides.bid,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vike_marketdata::L2BookLevel;

    /// The path order of a straddle's two legs (a buy at 105, a sell at 95): the nearer extreme
    /// first, an exact tie low first, an open gapped past a level first of all, and a tie between
    /// two equal reaches kept in the given order.
    #[test]
    fn several_legs_in_one_bar_fill_in_the_bars_path_order() {
        let ohlc = |o, h, l| Bar::ohlcv(0, o, h, l, o, 1.0);
        let straddle = [105.0, 95.0];
        assert_eq!(path_order(&ohlc(100.0, 106.0, 90.0), &straddle), [0, 1], "the high is nearer");
        assert_eq!(path_order(&ohlc(100.0, 110.0, 94.0), &straddle), [1, 0], "the low is nearer");
        assert_eq!(path_order(&ohlc(100.0, 106.0, 94.0), &straddle), [1, 0], "a tie: low first");
        // the sell gapped: it fills at the open, before the path moves at all
        assert_eq!(path_order(&ohlc(94.0, 106.0, 93.0), &[105.0, 94.0]), [1, 0], "the open");
        // three legs, one past the nearer extreme on the far side, two at one price
        let legs = [97.0, 103.0, 97.0];
        assert_eq!(path_order(&ohlc(100.0, 104.0, 95.0), &legs), [1, 0, 2], "high, then low twice");
        assert!(path_order(&ohlc(100.0, 101.0, 99.0), &[]).is_empty());
    }

    #[test]
    fn a_post_only_buy_at_or_above_the_ask_crosses_and_one_below_rests() {
        assert!(post_only_would_cross(1, 101.0, 100.0), "through the ask");
        assert!(post_only_would_cross(1, 100.0, 100.0), "touching the ask lifts it");
        assert!(!post_only_would_cross(1, 99.99, 100.0), "below the ask rests");
        assert!(post_only_would_cross(-1, 99.0, 100.0), "through the bid");
        assert!(post_only_would_cross(-1, 100.0, 100.0), "touching the bid hits it");
        assert!(!post_only_would_cross(-1, 100.01, 100.0), "above the bid rests");
        assert!(!post_only_would_cross(0, 100.0, 100.0), "no side, no verdict");
        assert!(!post_only_would_cross(1, f64::NAN, 100.0), "a NaN price crosses nothing");
    }

    /// P5.1 (owner 2026-11-13): an order snapped onto its grid and the book's level at the same
    /// tick are ONE number, so a post-only order AT the touch is judged crossing. 0.35 at tick 0.01
    /// is a level where `35 × 0.01` (0.35000000000000003) and the decimal part ways: under the old
    /// product snap a post-only sell at the 0.35 bid read as resting above it.
    #[test]
    fn a_post_only_order_snapped_to_the_touch_crosses_where_the_old_product_differed() {
        let mut book = vike_marketdata::L2Book::new(0.01);
        book.apply_snapshot(1, &[L2BookLevel::new(0.35, 5.0)], &[L2BookLevel::new(0.57, 5.0)]);
        let (bid, ask) = (book.best_bid().unwrap().price, book.best_ask().unwrap().price);
        assert_eq!((bid, ask), (0.35, 0.57));
        assert_ne!(35.0 * 0.01, 0.35, "the old snap");
        assert_ne!(57.0 * 0.01, 0.57, "the old snap");
        let snap = |px: f64| vike_model::round_to(px, Some(0.01));
        assert_eq!((snap(0.35), snap(0.57)), (bid, ask), "the order and the book name one number");
        assert!(post_only_would_cross(-1, snap(0.35), bid), "a post-only sell at the bid hits it");
        assert!(post_only_would_cross(1, snap(0.57), ask), "a post-only buy at the ask lifts it");
        assert!(!post_only_would_cross(-1, snap(0.36), bid), "a tick above the bid rests");
        assert!(!post_only_would_cross(-1, 35.0 * 0.01, bid), "what the old product snap read");
    }

    #[test]
    fn the_reference_is_the_quote_on_the_far_side_else_the_close() {
        let mut bar = Bar::ohlcv(0, 50.0, 50.0, 50.0, 50.0, 1.0);
        assert_eq!(post_only_reference(1, &bar), 50.0, "no quote: the last price");
        bar.bid = Some(49.0);
        bar.ask = Some(51.0);
        assert_eq!(post_only_reference(1, &bar), 51.0, "a buy takes the ask");
        assert_eq!(post_only_reference(-1, &bar), 49.0, "a sell takes the bid");
    }

    /// A window's sides from its quotes `(bid, ask)`, in order: each side's open, high, low, close.
    fn sides(quotes: &[(f64, f64)]) -> BarSides {
        let path = |px: Vec<f64>| {
            let (high, low) =
                px.iter().fold((f64::MIN, f64::MAX), |(h, l), &p| (h.max(p), l.min(p)));
            Bar::ohlcv(60_000, px[0], high, low, *px.last().expect("one quote at least"), 0.0)
        };
        BarSides {
            bid: path(quotes.iter().map(|q| q.0).collect()),
            ask: path(quotes.iter().map(|q| q.1).collect()),
        }
    }

    /// The fill of `order` on its side of the window, by the bar law.
    fn on_side(mut order: OrderResting, mid: &Bar, s: &BarSides) -> Option<f64> {
        let bar = side_bar(order.side, order.kind, mid, Some(s)).clone();
        FillModelBar.fill_price(&mut order, &bar)
    }

    /// The owner's examples (2026-11-16) and their short-side mirrors, on the law: a market order
    /// crosses the spread, a limit waits for its side to reach it, a stop triggers on its side.
    #[test]
    fn the_owners_examples_fill_on_the_side_never_at_the_mid() {
        // Polymarket 0.48 / 0.52: a market buy pays the ask, a market sell gets the bid
        let poly = sides(&[(0.48, 0.52)]);
        let poly_mid = Bar::ohlcv(60_000, 0.50, 0.50, 0.50, 0.50, 0.0);
        let market = |side| ord(OrderKind::Market, side, 100.0, None);
        assert_eq!(on_side(market(1), &poly_mid, &poly), Some(0.52));
        assert_eq!(on_side(market(-1), &poly_mid, &poly), Some(0.48));
        assert_eq!(FillModelBar.fill_price(&mut market(1), &poly_mid), Some(0.50), "the mid's");

        // cTrader EURUSD 1.08495 / 1.08505
        let fx = sides(&[(1.08495, 1.08505)]);
        let fx_mid = Bar::ohlcv(60_000, 1.08500, 1.08500, 1.08500, 1.08500, 0.0);
        assert_eq!(on_side(market(1), &fx_mid, &fx), Some(1.08505));
        assert_eq!(on_side(market(-1), &fx_mid, &fx), Some(1.08495));

        // a buy limit at 1.08490: the mid touches it, the ask (1.08495) does not
        let dip = sides(&[(1.08495, 1.08505), (1.08485, 1.08495)]);
        let dip_mid = Bar::ohlcv(60_000, 1.08500, 1.08500, 1.08490, 1.08490, 0.0);
        let buy_limit = || ord(OrderKind::Limit, 1, 100_000.0, Some(1.08490));
        assert_eq!(FillModelBar.fill_price(&mut buy_limit(), &dip_mid), Some(1.08490), "the mid's");
        assert_eq!(on_side(buy_limit(), &dip_mid, &dip), None, "the ask never reached it");
        let reached = sides(&[(1.08495, 1.08505), (1.08480, 1.08490)]);
        assert_eq!(on_side(buy_limit(), &dip_mid, &reached), Some(1.08490), "the ask did");
        // its mirror: a sell limit at 1.08510 waits for the BID, not the mid
        let rally = sides(&[(1.08495, 1.08505), (1.08505, 1.08515)]);
        let rally_mid = Bar::ohlcv(60_000, 1.08500, 1.08510, 1.08500, 1.08510, 0.0);
        let sell_limit = || ord(OrderKind::Limit, -1, 100_000.0, Some(1.08510));
        assert_eq!(FillModelBar.fill_price(&mut sell_limit(), &rally_mid), Some(1.08510));
        assert_eq!(on_side(sell_limit(), &rally_mid, &rally), None, "the bid never reached it");

        // a sell stop at 1.08480 fires when the BID reaches it (the mid is half a pip above)
        let fall = sides(&[(1.08495, 1.08505), (1.08480, 1.08490)]);
        let fall_mid = Bar::ohlcv(60_000, 1.08500, 1.08500, 1.08485, 1.08485, 0.0);
        let sell_stop = || ord(OrderKind::Stop, -1, 100_000.0, Some(1.08480));
        assert_eq!(FillModelBar.fill_price(&mut sell_stop(), &fall_mid), None, "the mid's: late");
        assert_eq!(on_side(sell_stop(), &fall_mid, &fall), Some(1.08480));
        // its mirror: a buy stop at 1.08520 fires when the ASK reaches it
        let rise = sides(&[(1.08495, 1.08505), (1.08510, 1.08520)]);
        let rise_mid = Bar::ohlcv(60_000, 1.08500, 1.08515, 1.08500, 1.08515, 0.0);
        let buy_stop = || ord(OrderKind::Stop, 1, 100_000.0, Some(1.08520));
        assert_eq!(FillModelBar.fill_price(&mut buy_stop(), &rise_mid), None, "the mid's: late");
        assert_eq!(on_side(buy_stop(), &rise_mid, &rise), Some(1.08520));
    }

    /// No sides is the bar itself, and an auction keeps the traded bar: nothing a backtest replays
    /// moves.
    #[test]
    fn without_sides_or_for_an_auction_the_bar_is_the_bar() {
        let bar = Bar::ohlcv(0, 100.0, 101.0, 99.0, 100.5, 1.0);
        let s = sides(&[(99.0, 101.0)]);
        for side in [1, -1] {
            assert!(std::ptr::eq(side_bar(side, OrderKind::Limit, &bar, None), &bar));
            for kind in
                [OrderKind::AuctionOpen, OrderKind::AuctionClose, OrderKind::AuctionOpenToLimit]
            {
                assert!(std::ptr::eq(side_bar(side, kind, &bar, Some(&s)), &bar), "{kind:?}");
            }
        }
        assert!(std::ptr::eq(side_bar(1, OrderKind::Stop, &bar, Some(&s)), &s.ask));
        assert!(std::ptr::eq(side_bar(-1, OrderKind::Stop, &bar, Some(&s)), &s.bid));
    }

    /// On a window of ONE quote the side law IS the tick tier's quote rule, kind by kind: the paper
    /// book's quoted bars and the simulator's quote ticks agree on what a fill is.
    #[test]
    fn on_one_quote_the_side_law_is_the_tick_tiers_quote_rule() {
        let (bid, ask) = (0.28, 0.30);
        let tick = tick_bar(); // trades 0.30, bid 0.28 / ask 0.30
        let s = sides(&[(bid, ask)]);
        let cases: Vec<OrderResting> = [1, -1]
            .into_iter()
            .flat_map(|side| {
                [
                    ord(OrderKind::Market, side, 10.0, None),
                    ord(OrderKind::MarketClose, side, 10.0, None),
                    ord(OrderKind::MarketToLimit, side, 10.0, None),
                    ord(OrderKind::Limit, side, 10.0, Some(0.29)),
                    ord(OrderKind::Limit, side, 10.0, Some(0.31)),
                    ord(OrderKind::Limit, side, 10.0, Some(0.27)),
                    ord(OrderKind::Stop, side, 10.0, Some(0.29)),
                    ord(OrderKind::Stop, side, 10.0, Some(0.27)),
                    ord(OrderKind::Stop, side, 10.0, Some(0.31)),
                    trigger_ord(OrderKind::StopLimit, side, 10.0, 0.29, Some(0.31)),
                    trigger_ord(OrderKind::StopLimit, side, 10.0, 0.29, Some(0.27)),
                    trigger_ord(OrderKind::TakeProfit, side, 10.0, 0.29, None),
                    trigger_ord(OrderKind::TakeProfit, side, 10.0, 0.31, Some(0.295)),
                    OrderResting {
                        trail: Some(0.01),
                        extreme: Some(0.30),
                        ..ord(OrderKind::Trailing, side, 10.0, None)
                    },
                    OrderResting {
                        trail: Some(0.05),
                        extreme: Some(0.30),
                        ..ord(OrderKind::Trailing, side, 10.0, None)
                    },
                    trailing_limit(side, 10.0, 0.30, 0.01, 0.02),
                    trailing_take_profit(side, 10.0, 0.26, 0.01, None),
                    trailing_take_profit(side, 10.0, 0.32, 0.01, Some(0.005)),
                ]
            })
            .collect();
        for order in cases {
            let (mut by_side, mut by_tick) = (order.clone(), order.clone());
            let bar = side_bar(order.side, order.kind, &tick, Some(&s)).clone();
            let side_fill = FillModelBar.fill_price(&mut by_side, &bar);
            let tick_fill = FillModelTick.fill_price(&mut by_tick, &tick);
            let what = format!("{:?} side {} at {:?}", order.kind, order.side, order.price);
            assert_eq!(side_fill, tick_fill, "{what}");
            assert_eq!(
                (by_side.kind, by_side.price, by_side.limit, by_side.extreme),
                (by_tick.kind, by_tick.price, by_tick.limit, by_tick.extreme),
                "{what}: the order after"
            );
        }
    }

    fn book() -> L2Book {
        let mut b = L2Book::new(0.01);
        // asks 0.30 x100, 0.31 x100, 0.35 x1000 ; bids 0.28 x100, 0.27 x100
        b.apply_snapshot(
            1,
            &[L2BookLevel::new(0.28, 100.0), L2BookLevel::new(0.27, 100.0)],
            &[
                L2BookLevel::new(0.30, 100.0),
                L2BookLevel::new(0.31, 100.0),
                L2BookLevel::new(0.35, 1000.0),
            ],
        );
        b
    }

    fn ord(kind: OrderKind, side: i32, size: f64, price: Option<f64>) -> OrderResting {
        OrderResting { price, ..OrderResting::new(kind, side, size) }
    }

    /// A tick bar whose quoted ask is the TOP of book — the price the L1 tier fills any size at.
    /// The whole point of the L2 tier is that this is not what a real taker pays.
    fn tick_bar() -> Bar {
        Bar {
            timestamp: 1_000,
            open: 0.30,
            high: 0.30,
            low: 0.30,
            close: 0.30,
            volume: 0.0,
            funding: None,
            bid: Some(0.28),
            ask: Some(0.30),
            symbol: None,
        }
    }

    /// THE bug, as a test: the L1 tier fills 250 shares at the top of book; the L2 tier charges
    /// the walk. A 250-share buy must pay the 0.30/0.31/0.35 ladder, not 0.30 flat.
    #[test]
    fn a_taker_pays_the_walk_not_the_top_of_book() {
        let (b, bar) = (book(), tick_bar());
        let mut o = ord(OrderKind::Market, 1, 250.0, None);
        assert_eq!(FillModelTick.fill_price(&mut o, &bar), Some(0.30), "L1: any size at the top");

        let got = FillModelL2Book.fill_price_book(&mut o, &bar, Some(&b)).unwrap();
        let want = (0.30 * 100.0 + 0.31 * 100.0 + 0.35 * 50.0) / 250.0;
        assert!((got - want).abs() < 1e-12, "{got} vs {want}");
        assert!(got > 0.30, "the walk is strictly worse than the top of book");
    }

    /// On an INVERSE contract the walk's price is the HARMONIC average of the levels it takes (each
    /// level costs its coins, `q/p`): 250 dollars across 0.30/0.31/0.35 pay `250 / Σ(q/p)`, below
    /// the arithmetic VWAP the walk charged before. A linear contract at any unit is the walk itself.
    #[test]
    fn an_inverse_taker_pays_the_harmonic_walk() {
        let (b, bar) = (book(), tick_bar());
        let inverse = ContractValue::inverse(1.0);
        let mut o = ord(OrderKind::Market, 1, 250.0, None);
        let got = FillModelL2Book.fill_price_book_valued(&mut o, &bar, Some(&b), inverse).unwrap();
        let want = 250.0 / (100.0 / 0.30 + 100.0 / 0.31 + 50.0 / 0.35);
        assert!((got - want).abs() < 1e-12, "{got} vs {want}");
        let arithmetic = (0.30 * 100.0 + 0.31 * 100.0 + 0.35 * 50.0) / 250.0;
        assert!(arithmetic - got > 1e-4, "the arithmetic VWAP {arithmetic} overcharges {got}");
        let mut l = ord(OrderKind::Market, 1, 250.0, None);
        let linear = FillModelL2Book.fill_price_book_valued(
            &mut l,
            &bar,
            Some(&b),
            ContractValue::linear(10.0),
        );
        assert_eq!(linear, FillModelL2Book.fill_price_book(&mut o, &bar, Some(&b)));
    }

    /// One share still pays the top of book — the tier is not a blanket haircut, it is a walk.
    #[test]
    fn a_single_share_still_fills_at_the_best_level() {
        let (b, bar) = (book(), tick_bar());
        let mut o = ord(OrderKind::Market, 1, 1.0, None);
        assert_eq!(FillModelL2Book.fill_price_book(&mut o, &bar, Some(&b)), Some(0.30));
        // and a sell walks the BID side
        let mut s = ord(OrderKind::Market, -1, 1.0, None);
        assert_eq!(FillModelL2Book.fill_price_book(&mut s, &bar, Some(&b)), Some(0.28));
    }

    /// Depth the book never showed is never filled — the order waits instead.
    #[test]
    fn size_beyond_the_displayed_depth_does_not_fill() {
        let (b, bar) = (book(), tick_bar());
        let mut o = ord(OrderKind::Market, 1, 5_000.0, None);
        assert_eq!(
            FillModelL2Book.fill_price_book(&mut o, &bar, Some(&b)),
            None,
            "1,200 displayed cannot honour 5,000"
        );
        // ...and the L1 tier would happily have filled all 5,000 at 0.30
        assert_eq!(FillModelTick.fill_price(&mut o, &bar), Some(0.30));
    }

    /// A marketable limit crosses only up to its own price; a limit the book has moved away from
    /// does NOT fill and therefore rests. This is the "booked, not filled" outcome.
    #[test]
    fn a_limit_fills_only_from_liquidity_inside_its_own_price_and_rests_otherwise() {
        let (b, bar) = (book(), tick_bar());
        // marketable at 0.31, 150 shares: 100 @ 0.30 + 50 @ 0.31
        let mut o = ord(OrderKind::Limit, 1, 150.0, Some(0.31));
        let got = FillModelL2Book.fill_price_book(&mut o, &bar, Some(&b)).unwrap();
        let want = (0.30 * 100.0 + 0.31 * 50.0) / 150.0;
        assert!((got - want).abs() < 1e-12, "{got} vs {want}");

        // 250 shares at 0.31: only 200 are inside the limit -> no fill, the order rests
        let mut big = ord(OrderKind::Limit, 1, 250.0, Some(0.31));
        assert_eq!(FillModelL2Book.fill_price_book(&mut big, &bar, Some(&b)), None);

        // a limit below the whole ask side never crosses
        let mut away = ord(OrderKind::Limit, 1, 10.0, Some(0.25));
        assert_eq!(FillModelL2Book.fill_price_book(&mut away, &bar, Some(&b)), None);
    }

    /// With no book the tier IS the L1 tier — byte for byte, including the trigger bookkeeping.
    #[test]
    fn bookless_is_exactly_the_tick_tier() {
        let bar = tick_bar();
        for kind in [OrderKind::Market, OrderKind::MarketClose, OrderKind::Limit, OrderKind::Stop] {
            let price = (kind != OrderKind::Market).then_some(0.29);
            let mut a = ord(kind, 1, 10.0, price);
            let mut c = ord(kind, 1, 10.0, price);
            assert_eq!(
                FillModelL2Book.fill_price_book(&mut a, &bar, None),
                FillModelTick.fill_price(&mut c, &bar),
                "{kind:?}"
            );
            assert_eq!(a.extreme, c.extreme, "{kind:?}: trigger bookkeeping must match too");
        }
    }

    /// A triggered stop is priced as the taker it is, but its TRIGGER decision is untouched.
    #[test]
    fn a_stop_keeps_the_l1_trigger_and_gains_the_walk_price() {
        let (b, bar) = (book(), tick_bar());
        // buy-stop at 0.29: ask 0.30 >= 0.29 -> triggered by the L1 law
        let mut hit = ord(OrderKind::Stop, 1, 250.0, Some(0.29));
        let got = FillModelL2Book.fill_price_book(&mut hit, &bar, Some(&b)).unwrap();
        assert!((got - (0.30 * 100.0 + 0.31 * 100.0 + 0.35 * 50.0) / 250.0).abs() < 1e-12);
        // buy-stop at 0.40: ask 0.30 < 0.40 -> NOT triggered, and the book cannot override that
        let mut miss = ord(OrderKind::Stop, 1, 10.0, Some(0.40));
        assert_eq!(FillModelL2Book.fill_price_book(&mut miss, &bar, Some(&b)), None);
    }

    fn trigger_ord(kind: OrderKind, side: i32, size: f64, t: f64, l: Option<f64>) -> OrderResting {
        OrderResting { limit: l, ..ord(kind, side, size, Some(t)) }
    }

    /// The quote law (bid 0.28 / ask 0.30): the stop-limit triggers as a stop, the take-profit as a
    /// limit; the limit part fills at the quote within L and otherwise rests as the `Limit` at L.
    #[test]
    fn a_stop_limit_and_a_take_profit_trigger_and_fill_on_the_quote() {
        let bar = tick_bar();
        let (stop_limit, take_profit) = (OrderKind::StopLimit, OrderKind::TakeProfit);
        for (case, kind, side, t, l, fill, rests) in [
            ("sl buy, ask within L", stop_limit, 1, 0.29, Some(0.31), Some(0.30), false),
            ("sl buy, ask above L: rests", stop_limit, 1, 0.29, Some(0.295), None, true),
            ("sl buy, not triggered", stop_limit, 1, 0.31, Some(0.32), None, false),
            ("sl sell, bid within L", stop_limit, -1, 0.29, Some(0.27), Some(0.28), false),
            ("mit buy, ask at or below T", take_profit, 1, 0.31, None, Some(0.30), false),
            ("mit buy, not touched", take_profit, 1, 0.29, None, None, false),
            ("lit sell, bid below L: rests", take_profit, -1, 0.27, Some(0.285), None, true),
        ] {
            let mut o = trigger_ord(kind, side, 10.0, t, l);
            assert_eq!(FillModelTick.fill_price(&mut o, &bar), fill, "{case}");
            let after = if rests { (OrderKind::Limit, l, None) } else { (kind, Some(t), l) };
            assert_eq!((o.kind, o.price, o.limit), after, "{case}");
        }
    }

    /// The book walk is capped at L: depth the limit cannot cover rests the order as the `Limit` at
    /// L; a take-profit with no limit walks uncapped.
    #[test]
    fn a_stop_limit_walks_the_book_up_to_its_limit_and_rests_beyond_it() {
        let (b, bar) = (book(), tick_bar());
        let mut fits = trigger_ord(OrderKind::StopLimit, 1, 150.0, 0.29, Some(0.31));
        let got = FillModelL2Book.fill_price_book(&mut fits, &bar, Some(&b)).unwrap();
        assert!((got - (0.30 * 100.0 + 0.31 * 50.0) / 150.0).abs() < 1e-12, "{got}");

        let mut deep = trigger_ord(OrderKind::StopLimit, 1, 250.0, 0.29, Some(0.31));
        assert_eq!(FillModelL2Book.fill_price_book(&mut deep, &bar, Some(&b)), None);
        assert_eq!((deep.kind, deep.price, deep.limit), (OrderKind::Limit, Some(0.31), None));

        let mut mit = trigger_ord(OrderKind::TakeProfit, 1, 250.0, 0.31, None);
        let got = FillModelL2Book.fill_price_book(&mut mit, &bar, Some(&b)).unwrap();
        assert!((got - (0.30 * 100.0 + 0.31 * 100.0 + 0.35 * 50.0) / 250.0).abs() < 1e-12);
    }

    fn trailing_limit(side: i32, size: f64, extreme: f64, trail: f64, offset: f64) -> OrderResting {
        OrderResting {
            trail: Some(trail),
            extreme: Some(extreme),
            limit_offset: Some(offset),
            ..ord(OrderKind::TrailingLimit, side, size, None)
        }
    }

    /// The quote law (bid 0.28 / ask 0.30): the trailing-limit fires as the trailing stop does, its
    /// limit is the stop less (a sell) or plus (a buy) its offset, and it fills at the quote within L,
    /// else rests as the `Limit` at L; unfired, the extreme ratchets as the trailing stop's does.
    #[test]
    fn a_trailing_limit_fires_on_the_quote_and_fills_within_its_limit_or_rests() {
        let bar = tick_bar();
        let mut fits = trailing_limit(-1, 10.0, 0.30, 0.01, 0.02);
        assert_eq!(FillModelTick.fill_price(&mut fits, &bar), Some(0.28), "the bid, within L");
        assert_eq!((fits.kind, fits.limit), (OrderKind::TrailingLimit, Some(0.30 - 0.01 - 0.02)));

        let mut rests = trailing_limit(-1, 10.0, 0.30, 0.01, 0.005);
        assert_eq!(FillModelTick.fill_price(&mut rests, &bar), None, "the bid is below L");
        assert_eq!(
            (rests.kind, rests.price, rests.limit),
            (OrderKind::Limit, Some(0.30 - 0.01 - 0.005), None)
        );

        let mut waits = trailing_limit(-1, 10.0, 0.30, 0.03, 0.0);
        let mut plain = OrderResting { kind: OrderKind::Trailing, ..waits.clone() };
        assert_eq!(FillModelTick.fill_price(&mut waits, &bar), None);
        assert_eq!(FillModelTick.fill_price(&mut plain, &bar), None);
        assert_eq!((waits.kind, waits.extreme), (OrderKind::TrailingLimit, plain.extreme));
    }

    /// The book walk is capped at the limit set at the fire: depth inside it fills, depth beyond it
    /// rests the order as the `Limit` at L.
    #[test]
    fn a_trailing_limit_walks_the_book_up_to_its_limit_and_rests_beyond_it() {
        let (b, bar) = (book(), tick_bar()); // asks 0.30 x100, 0.31 x100, 0.35 x1000
        // a buy trailing a low of 0.28 by 0.01 stops at 0.29; its limit is 0.29 + 0.025
        let mut fits = trailing_limit(1, 150.0, 0.28, 0.01, 0.025);
        let got = FillModelL2Book.fill_price_book(&mut fits, &bar, Some(&b)).unwrap();
        assert!((got - (0.30 * 100.0 + 0.31 * 50.0) / 150.0).abs() < 1e-12, "{got}");

        let mut deep = trailing_limit(1, 250.0, 0.28, 0.01, 0.025);
        assert_eq!(FillModelL2Book.fill_price_book(&mut deep, &bar, Some(&b)), None);
        let limit = 0.28 + 0.01 + 0.025;
        assert_eq!((deep.kind, deep.price, deep.limit), (OrderKind::Limit, Some(limit), None));
    }

    fn trailing_take_profit(
        side: i32,
        size: f64,
        extreme: f64,
        trail: f64,
        offset: Option<f64>,
    ) -> OrderResting {
        OrderResting {
            trail: Some(trail),
            extreme: Some(extreme),
            limit_offset: offset,
            ..ord(OrderKind::TrailingTakeProfit, side, size, None)
        }
    }

    /// The trailing take-profit's quote law (bid 0.28 / ask 0.30), the mirror of the trailing stop's:
    /// a sell trailing a low of 0.26 by 0.01 fires on the bid, at the bid; by 0.05 it does not, and its
    /// low stays; a buy trailing a high of 0.32 fires on the ask. On L2 a limit-offset one walks the
    /// book capped at its limit and rests as the `Limit` there when the depth cannot cover it.
    #[test]
    fn a_trailing_take_profit_fires_on_the_quote_from_the_favourable_side() {
        let bar = tick_bar();
        let mut sell = trailing_take_profit(-1, 10.0, 0.26, 0.01, None);
        assert_eq!(FillModelTick.fill_price(&mut sell, &bar), Some(0.28));
        let mut far = trailing_take_profit(-1, 10.0, 0.26, 0.05, None);
        assert_eq!(FillModelTick.fill_price(&mut far, &bar), None);
        assert_eq!(far.extreme, Some(0.26), "the bid is above the low: it stays");
        let mut buy = trailing_take_profit(1, 10.0, 0.32, 0.01, Some(0.005));
        assert_eq!(FillModelTick.fill_price(&mut buy, &bar), Some(0.30), "the ask, within L");
        let mut waits = trailing_take_profit(1, 10.0, 0.32, 0.05, None);
        assert_eq!(FillModelTick.fill_price(&mut waits, &bar), None);
        assert_eq!(waits.extreme, Some(0.32));

        let b = book(); // asks 0.30 x100, 0.31 x100, 0.35 x1000
        let mut fits = trailing_take_profit(1, 150.0, 0.32, 0.01, Some(0.005));
        let got = FillModelL2Book.fill_price_book(&mut fits, &bar, Some(&b)).unwrap();
        assert!((got - (0.30 * 100.0 + 0.31 * 50.0) / 150.0).abs() < 1e-12, "{got}");
        let mut deep = trailing_take_profit(1, 250.0, 0.32, 0.01, Some(0.005));
        assert_eq!(FillModelL2Book.fill_price_book(&mut deep, &bar, Some(&b)), None);
        assert_eq!(deep.kind, OrderKind::Limit, "rests as the Limit at its limit");
    }

    /// On the quote a percent trail of 10 at an extreme of 0.5 is the price trail of 0.05 there.
    #[test]
    fn a_percent_trail_on_the_quote_is_the_price_trail_of_its_distance() {
        let bar = tick_bar();
        for side in [-1, 1] {
            let by = |trail, trail_percent| OrderResting {
                trail,
                trail_percent,
                extreme: Some(0.5),
                ..ord(OrderKind::Trailing, side, 10.0, None)
            };
            let (mut price, mut percent) = (by(Some(0.05), None), by(None, Some(10.0)));
            assert_eq!(
                FillModelTick.fill_price(&mut price, &bar),
                FillModelTick.fill_price(&mut percent, &bar),
                "{side}"
            );
            assert_eq!(price.extreme, percent.extreme, "{side}");
        }
    }

    /// A stop the quote does not fire is adjusted when the quote reaches its level (a sell stop at 0.20
    /// adjusted at 0.25 by the bid 0.28), and a discretionary limit is judged at its discretion on L1
    /// and on L2 (a buy at 0.29 with 0.025 of discretion lifts the 0.30 ask; the plain one does not).
    #[test]
    fn a_stop_adjusts_and_a_discretionary_limit_reaches_on_the_quote_and_the_book() {
        let bar = tick_bar();
        let adjust = Some(Box::new(vike_model::OrderStopAdjust {
            at: 0.25,
            to: vike_model::OrderStopAdjustTo::Stop { stop: 0.22 },
        }));
        let mut stop = OrderResting { adjust, ..ord(OrderKind::Stop, -1, 10.0, Some(0.20)) };
        assert_eq!(FillModelTick.fill_price(&mut stop, &bar), None);
        assert_eq!((stop.kind, stop.price, stop.adjust), (OrderKind::Stop, Some(0.22), None));

        let limit = |d| OrderResting {
            discretionary_amount: d,
            ..ord(OrderKind::Limit, 1, 150.0, Some(0.29))
        };
        assert_eq!(FillModelTick.fill_price(&mut limit(None), &bar), None);
        assert_eq!(FillModelTick.fill_price(&mut limit(Some(0.025)), &bar), Some(0.30));
        let got = FillModelL2Book.fill_price_book(&mut limit(Some(0.025)), &bar, Some(&book()));
        let want = (0.30 * 100.0 + 0.31 * 50.0) / 150.0;
        assert!((got.unwrap() - want).abs() < 1e-12, "{got:?}");
    }

    /// On L1 a market-to-limit is the market order it starts as (it crosses the quote, or takes the
    /// open with no quote), and an auction clears at the traded price, never across the quote.
    #[test]
    fn on_l1_a_market_to_limit_crosses_the_quote_and_an_auction_takes_the_traded_price() {
        let bar = tick_bar(); // trades 0.30, bid 0.28 / ask 0.30
        let mut mtl = ord(OrderKind::MarketToLimit, -1, 10.0, None);
        assert_eq!(FillModelTick.fill_price(&mut mtl, &bar), Some(0.28), "a sell hits the bid");
        let quoteless = Bar { bid: None, ask: None, ..bar.clone() };
        let mut mtl = ord(OrderKind::MarketToLimit, 1, 10.0, None);
        assert_eq!(FillModelTick.fill_price(&mut mtl, &quoteless), Some(0.30), "the open");
        let mut to_limit = ord(OrderKind::AuctionOpenToLimit, 1, 5_000.0, None);
        let to_limit_l2 = FillModelL2Book.fill_price_book(&mut to_limit, &bar, Some(&book()));
        assert_eq!(
            to_limit_l2,
            Some(0.30),
            "the opening auction's market-to-limit: the traded price"
        );
        for kind in [OrderKind::AuctionOpen, OrderKind::AuctionClose] {
            let mut sell = ord(kind, -1, 10.0, None);
            assert_eq!(FillModelTick.fill_price(&mut sell, &bar), Some(0.30), "{kind:?}");
            let mut loo = ord(kind, 1, 10.0, Some(0.29));
            assert_eq!(FillModelTick.fill_price(&mut loo, &bar), None, "{kind:?}: passed");
            // the L2 tier does not walk the book for an auction
            let mut big = ord(kind, 1, 5_000.0, None);
            assert_eq!(FillModelL2Book.fill_price_book(&mut big, &bar, Some(&book())), Some(0.30));
        }
    }

    /// On L2 a market-to-limit takes the best opposite level only, at its price: covered, it fills
    /// there; not covered, the WHOLE order rests as the `Limit` at that price; an empty side leaves
    /// it waiting as it was.
    #[test]
    fn on_l2_a_market_to_limit_takes_the_best_level_or_rests_whole_as_its_limit() {
        let (b, bar) = (book(), tick_bar()); // asks 0.30 x100 …, bids 0.28 x100 …
        let mut fits = ord(OrderKind::MarketToLimit, 1, 100.0, None);
        assert_eq!(FillModelL2Book.fill_price_book(&mut fits, &bar, Some(&b)), Some(0.30));
        let mut deep = ord(OrderKind::MarketToLimit, -1, 150.0, None);
        assert_eq!(FillModelL2Book.fill_price_book(&mut deep, &bar, Some(&b)), None);
        assert_eq!((deep.kind, deep.price, deep.limit), (OrderKind::Limit, Some(0.28), None));
        let mut empty = L2Book::new(0.01);
        empty.apply_snapshot(1, &[L2BookLevel::new(0.28, 100.0)], &[]);
        let mut waits = ord(OrderKind::MarketToLimit, 1, 10.0, None);
        assert_eq!(FillModelL2Book.fill_price_book(&mut waits, &bar, Some(&empty)), None);
        assert_eq!((waits.kind, waits.limit), (OrderKind::MarketToLimit, None));
    }

    /// The path walk, trail 2, a sell waiting for 104 (vike-simulator's `tests/laws/trailing_activation.rs`
    /// and `tests/laws/trailing_limit_law.rs` drive it through every lane): activated at the open
    /// 104.5, up to 106, down through P 104. A trailing stop fills at P; a trailing limit with an
    /// offset of 1 fills there too (L 103 reached). On a bar that opens at 102, past L, and only
    /// gets back to 102.5, the trailing limit rests as the `Limit` at 103.
    #[test]
    fn the_path_walk_activates_ratchets_and_fires_on_one_bar() {
        let walk = |kind: OrderKind| {
            let fresh = || OrderResting {
                trail: Some(2.0),
                activation: Some(104.0),
                limit_offset: (kind == OrderKind::TrailingLimit).then_some(1.0),
                ..OrderResting::new(kind, -1, 1.0)
            };
            let mut one_bar = fresh();
            let fired =
                trailing_path_fill(&mut one_bar, &Bar::ohlcv(0, 104.5, 106.0, 100.0, 101.0, 1.0));
            let mut o = fresh();
            assert_eq!(
                trailing_path_fill(&mut o, &Bar::ohlcv(0, 104.5, 106.0, 104.5, 105.5, 1.0)),
                None
            );
            assert_eq!(o.extreme, Some(106.0), "{kind:?}: activated and ratcheted, not fired");
            let gap = trailing_path_fill(&mut o, &Bar::ohlcv(1, 102.0, 102.5, 100.0, 101.0, 1.0));
            (fired, gap, o.kind, o.price)
        };
        assert_eq!(
            walk(OrderKind::Trailing),
            (Some(104.0), Some(102.0), OrderKind::Trailing, None)
        );
        let rests = (Some(104.0), None, OrderKind::Limit, Some(103.0));
        assert_eq!(walk(OrderKind::TrailingLimit), rests);
    }

    // The shared law's own refusals are pinned beside the law itself:
    // `crates/vike-marketdata/src/orderbook/walk_tests.rs`'s
    // `book_taker_price_refuses_rather_than_inventing_liquidity`.
}
