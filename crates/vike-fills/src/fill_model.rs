//! Pluggable fill-price models, selected by available data (tiered fill engine).
//! Exact port of `core/fill_model.py`, plus the net-new L2 tier below.
//!
//! - [`BarFillModel`] (no ticks): delegate to `order_fill_price` — the pre-tick behavior.
//! - [`TickFillModel`] (L1 quote bars): market orders cross the real spread (buy@ask /
//!   sell@bid) when the bar carries bid/ask; single-quote-tick bars (high == low) trigger
//!   resting orders off the quote side; consolidated bars delegate.
//! - [`L2BookFillModel`] (L1 + L2 book): a taker is priced by CONSUMING the resting book, not
//!   by reading one tick price. This is the R8 tier the `SimBroker` docs have named as the
//!   follow-up since the queue model landed.
//!
//! All tiers return a RAW price; the engine still applies `adverse_fill_price` slippage and
//! routes the fill through `compute_fill` (parity preserved).
//!
//! # Why the L2 tier exists (and why it is not one strategy's bug)
//!
//! Every tier above it prices a taker at a price NOBODY WAS OFFERING. [`TickFillModel`] fills a
//! market buy at `bar.ask` — the top of book, at any size — and on a trade-tick tape with no
//! quotes it falls through to `order_fill_price`, which fills at the PRINT: the price at which
//! somebody else has already taken the liquidity. A taker cannot obtain either. The error is
//! systematically one-directional (always in the strategy's favour) and it scales with size,
//! because top-of-book is exactly the cheapest share on the ladder.
//!
//! Measured on the Polymarket `btc-updown-5m` tape over 835 entries (`cheap_np_depth`, PR #650):
//! the tape print averaged **0.2683** while the resting best ask at the same instant averaged
//! **0.3063**, and in 738 of 835 cases there was NO resting ask at the printed price at all. That
//! is a ~4 cent per-share error on a strategy whose whole edge bar is 5.5 cents — and it is not
//! specific to that strategy. Any strategy that takes liquidity on the tick path is mispriced the
//! same way. Hence the fix lives HERE, in the engine, and not in a strategy.
//!
//! [`L2BookFillModel`] is ADDITIVE: it is reached only via `FillModelKind::L2Book`, which nothing
//! selects by default, so every existing model, test and published number is byte-identical.

// `order_fill_price` is the fill-trigger oracle, which lives in vike-model (it is shared with the
// vike-core ConditionalBook emulator); every call site spells `vike_model::` — the old
// `vike_backtest::orders` re-export shim over these names is gone (no pub-use shims on a move).
use vike_model::{Bar, L2Book, OrderKind, WorkingOrder, order_fill_price};

pub trait FillModel {
    fn fill_price(&self, order: &mut WorkingOrder, bar: &Bar) -> Option<f64>;

    /// The BOOK-AWARE fill price. Defaults to [`FillModel::fill_price`], so every tier that has
    /// no use for depth is unchanged and no existing implementation needs touching — only
    /// [`L2BookFillModel`] overrides it.
    fn fill_price_book(
        &self,
        order: &mut WorkingOrder,
        bar: &Bar,
        book: Option<&L2Book>,
    ) -> Option<f64> {
        let _ = book;
        self.fill_price(order, bar)
    }
}

/// THE taker-price law — **hoisted to [`vike_model::book_taker_price`]**.
///
/// It is a property of an order book, not of a simulation, and the live path needs the SAME
/// rule (pre-trade sizing, a paper executor on a live feed, comparing realised fills against
/// the model). `vike-backtest` is a SIBLING of the live path, not below it, so live cannot
/// depend on this crate — the shared home in `vike-model` is what lets both sides share ONE
/// definition instead of drifting apart silently.
pub use vike_model::book_taker_price;

/// Bar-level fills (no tick data) — identical to the pre-tick engine.
#[derive(Debug, Default, Clone, Copy)]
pub struct BarFillModel;

impl FillModel for BarFillModel {
    fn fill_price(&self, order: &mut WorkingOrder, bar: &Bar) -> Option<f64> {
        order_fill_price(order, bar)
    }
}

/// L1 spread-crossing fill model.
#[derive(Debug, Default, Clone, Copy)]
pub struct TickFillModel;

impl FillModel for TickFillModel {
    fn fill_price(&self, order: &mut WorkingOrder, bar: &Bar) -> Option<f64> {
        let has_quote = bar.bid.is_some() && bar.ask.is_some();
        let buy = order.side > 0;
        let kind = order.kind;
        // Slice-1 preserved: a MARKET order crosses the real spread whenever quotes are
        // present, for ANY bar shape (consolidated multi-tick bar or single tick).
        if kind == OrderKind::Market && has_quote {
            return Some(if buy { bar.ask.unwrap() } else { bar.bid.unwrap() });
        }
        // Slice-2: quote-side triggering for resting orders + market_close, ONLY for a SINGLE
        // quote tick (high == low). A consolidated bar (high != low) delegates.
        if has_quote && bar.high == bar.low {
            let (bid, ask) = (bar.bid.unwrap(), bar.ask.unwrap());
            let quote = if buy { ask } else { bid };
            match kind {
                OrderKind::MarketClose => return Some(quote),
                OrderKind::Limit | OrderKind::LimitClose => {
                    let price = order.price.expect("limit requires price");
                    return if buy {
                        if ask <= price { Some(quote) } else { None }
                    } else if bid >= price {
                        Some(quote)
                    } else {
                        None
                    };
                }
                OrderKind::Stop => {
                    let price = order.price.expect("stop requires price");
                    return if buy {
                        if ask >= price { Some(quote) } else { None }
                    } else if bid <= price {
                        Some(quote)
                    } else {
                        None
                    };
                }
                OrderKind::Trailing => {
                    // side<0 protects a long (sell-stop trailing the bid);
                    // side>0 protects a short (buy-stop trailing the ask).
                    let trail = order.trail.expect("trailing requires trail");
                    let extreme = order.extreme.expect("trailing requires extreme");
                    if order.side < 0 {
                        let trigger = extreme - trail;
                        if bid <= trigger {
                            return Some(bid);
                        }
                        order.extreme = Some(extreme.max(bid));
                        return None;
                    }
                    let trigger = extreme + trail;
                    if ask >= trigger {
                        return Some(ask);
                    }
                    order.extreme = Some(extreme.min(ask));
                    return None;
                }
                OrderKind::Market => {} // unreachable: handled above when has_quote
            }
        }
        order_fill_price(order, bar)
    }
}

/// L1 + **L2** fills: the trigger law of [`TickFillModel`], the PRICE from walking the book.
///
/// The split is deliberate and is what keeps this tier reviewable:
///
/// * **WHEN** an order fills is unchanged — [`TickFillModel`] decides it, including the stop /
///   trailing trigger arithmetic and its `extreme` bookkeeping. Nothing about triggering moves.
/// * **WHAT PRICE** it fills at is re-derived by [`book_taker_price`]: the VWAP of consuming the
///   resting book for the order's OWN SIZE, best level outward. A resting LIMIT additionally caps
///   the walk at its own limit price, so it can only ever be filled by liquidity it would
///   genuinely have crossed.
///
/// **No book, no change.** With `book == None` this tier IS [`TickFillModel`], byte for byte. The
/// tier upgrade only bites where L2 data actually exists, so mixing a book-carrying symbol and a
/// bookless one in a single run is well-defined rather than silently lossy.
///
/// **Insufficient depth does not fill.** When the displayed book cannot cover the order's size
/// (within its limit, if any) the order does not fill on this event and stays resting — it waits
/// for liquidity. That is the one behavioural difference a strategy will notice, and it is the
/// honest one: the alternative is to fill size the market never showed. It also gives the venue
/// semantics this tier was built for: an order that is marketable when the strategy decides, and
/// no longer marketable when the matching engine actually looks at it, **rests on the book**
/// rather than filling or vanishing. Compose that with `vike_backtest::latency::LatencyModelKind`
/// and the "decide at `T`, match at `T + Δ`" rule falls out of machinery that already exists —
/// see `vike_backtest::latency::VENUE_HOLD_POLYMARKET_UPDOWN_MS`.
#[derive(Debug, Default, Clone, Copy)]
pub struct L2BookFillModel;

impl FillModel for L2BookFillModel {
    /// Bookless: exactly [`TickFillModel`]. See the type doc — the tier degrades, it does not
    /// invent.
    fn fill_price(&self, order: &mut WorkingOrder, bar: &Bar) -> Option<f64> {
        TickFillModel.fill_price(order, bar)
    }

    fn fill_price_book(
        &self,
        order: &mut WorkingOrder,
        bar: &Bar,
        book: Option<&L2Book>,
    ) -> Option<f64> {
        let Some(book) = book else { return TickFillModel.fill_price(order, bar) };
        match order.kind {
            // A market taker crosses whatever is resting, uncapped, for its own size.
            OrderKind::Market | OrderKind::MarketClose => {
                book_taker_price(book, order.side, order.size, None)
            }
            // A marketable limit crosses only up to its own price; a limit the book has moved away
            // from does not fill at all (the within-limit depth cannot cover it, so
            // `book_taker_price` is `None`) and therefore RESTS — the venue outcome this tier
            // exists to be able to express.
            OrderKind::Limit | OrderKind::LimitClose => {
                let price = order.price?;
                book_taker_price(book, order.side, order.size, Some(price))
            }
            // Stops and trailing stops keep the L1 TRIGGER law verbatim (it owns the `extreme`
            // mutation), and are then priced as the takers they are. If the book cannot cover the
            // size the stop does not fill on this event — it stays armed for the next one, the
            // same discipline as every other kind here.
            OrderKind::Stop | OrderKind::Trailing => {
                TickFillModel.fill_price(order, bar)?;
                book_taker_price(book, order.side, order.size, None)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn book() -> L2Book {
        let mut b = L2Book::new(0.01);
        // asks 0.30 x100, 0.31 x100, 0.35 x1000 ; bids 0.28 x100, 0.27 x100
        b.apply_snapshot(
            1,
            &[(0.28, 100.0), (0.27, 100.0)],
            &[(0.30, 100.0), (0.31, 100.0), (0.35, 1000.0)],
        );
        b
    }

    fn ord(kind: OrderKind, side: i32, size: f64, price: Option<f64>) -> WorkingOrder {
        WorkingOrder { price, ..WorkingOrder::new(kind, side, size) }
    }

    /// A tick bar whose quoted ask is the TOP of book — the price the L1 tier fills any size at.
    /// The whole point of the L2 tier is that this is not what a real taker pays.
    fn tick_bar() -> Bar {
        Bar {
            ts: 1_000,
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
        assert_eq!(TickFillModel.fill_price(&mut o, &bar), Some(0.30), "L1: any size at the top");

        let got = L2BookFillModel.fill_price_book(&mut o, &bar, Some(&b)).unwrap();
        let want = (0.30 * 100.0 + 0.31 * 100.0 + 0.35 * 50.0) / 250.0;
        assert!((got - want).abs() < 1e-12, "{got} vs {want}");
        assert!(got > 0.30, "the walk is strictly worse than the top of book");
    }

    /// One share still pays the top of book — the tier is not a blanket haircut, it is a walk.
    #[test]
    fn a_single_share_still_fills_at_the_best_level() {
        let (b, bar) = (book(), tick_bar());
        let mut o = ord(OrderKind::Market, 1, 1.0, None);
        assert_eq!(L2BookFillModel.fill_price_book(&mut o, &bar, Some(&b)), Some(0.30));
        // and a sell walks the BID side
        let mut s = ord(OrderKind::Market, -1, 1.0, None);
        assert_eq!(L2BookFillModel.fill_price_book(&mut s, &bar, Some(&b)), Some(0.28));
    }

    /// Depth the book never showed is never filled — the order waits instead.
    #[test]
    fn size_beyond_the_displayed_depth_does_not_fill() {
        let (b, bar) = (book(), tick_bar());
        let mut o = ord(OrderKind::Market, 1, 5_000.0, None);
        assert_eq!(
            L2BookFillModel.fill_price_book(&mut o, &bar, Some(&b)),
            None,
            "1,200 displayed cannot honour 5,000"
        );
        // ...and the L1 tier would happily have filled all 5,000 at 0.30
        assert_eq!(TickFillModel.fill_price(&mut o, &bar), Some(0.30));
    }

    /// A marketable limit crosses only up to its own price; a limit the book has moved away from
    /// does NOT fill and therefore rests. This is the "booked, not filled" outcome.
    #[test]
    fn a_limit_fills_only_from_liquidity_inside_its_own_price_and_rests_otherwise() {
        let (b, bar) = (book(), tick_bar());
        // marketable at 0.31, 150 shares: 100 @ 0.30 + 50 @ 0.31
        let mut o = ord(OrderKind::Limit, 1, 150.0, Some(0.31));
        let got = L2BookFillModel.fill_price_book(&mut o, &bar, Some(&b)).unwrap();
        let want = (0.30 * 100.0 + 0.31 * 50.0) / 150.0;
        assert!((got - want).abs() < 1e-12, "{got} vs {want}");

        // 250 shares at 0.31: only 200 are inside the limit -> no fill, the order rests
        let mut big = ord(OrderKind::Limit, 1, 250.0, Some(0.31));
        assert_eq!(L2BookFillModel.fill_price_book(&mut big, &bar, Some(&b)), None);

        // a limit below the whole ask side never crosses
        let mut away = ord(OrderKind::Limit, 1, 10.0, Some(0.25));
        assert_eq!(L2BookFillModel.fill_price_book(&mut away, &bar, Some(&b)), None);
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
                L2BookFillModel.fill_price_book(&mut a, &bar, None),
                TickFillModel.fill_price(&mut c, &bar),
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
        let got = L2BookFillModel.fill_price_book(&mut hit, &bar, Some(&b)).unwrap();
        assert!((got - (0.30 * 100.0 + 0.31 * 100.0 + 0.35 * 50.0) / 250.0).abs() < 1e-12);
        // buy-stop at 0.40: ask 0.30 < 0.40 -> NOT triggered, and the book cannot override that
        let mut miss = ord(OrderKind::Stop, 1, 10.0, Some(0.40));
        assert_eq!(L2BookFillModel.fill_price_book(&mut miss, &bar, Some(&b)), None);
    }

    // The shared law's own refusals are pinned beside the law itself, in
    // `vike_model::orderbook`'s tests — see `book_taker_price_refuses_rather_than_inventing_liquidity`.
}
