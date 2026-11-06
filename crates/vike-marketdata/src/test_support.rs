//! Shared test builders for the market-data vocabulary: ONE spelling of the hand-written
//! [`TradeTick`], [`QuoteTick`], [`Bar`] and [`L2Book`] constructors that several crates' test
//! modules had each re-typed.
//!
//! Behind the `test-support` feature (and `cfg(test)` of this crate), so a default build compiles
//! NONE of it — the workspace's "one owned double behind a `test-support` feature" convention, the
//! same shape as `vike-data`'s and `vike-model`'s. A consumer takes it through a `[dev-dependencies]`
//! edge carrying `features = ["test-support"]`; a NORMAL edge that did would compile it into a
//! shipped build, and `crates/vike-ops/tests/architecture/test_surface_gate/manifests.rs`'s
//! `no_shipped_edge_enables_a_test_feature` refuses that.
//!
//! # The contract: every builder fixes its DEFAULTS, and says which
//!
//! A builder here takes only the fields a test varies and pins every other field to the value its
//! own doc names. That is what makes a migration safe to review — a hand-written copy may be
//! replaced by the builder exactly when its signature AND every pinned field match — and it is why
//! a test that reads a field a builder pins (a symbol, a receive timestamp, a volume, a quote size)
//! must NOT take the builder: it builds the value by hand, so the field it depends on is visible at
//! the call site rather than hidden in a default somewhere else. A test that only RELIES on one
//! (every bar it feeds arrives symbol-less, as a live bar does) may take the builder when it says
//! so beside its import.
//!
//! Where two copies of one shape differ in a single pinned field, each gets its own builder whose
//! NAME says the difference ([`flat_bar_zero_volume`] and [`flat_bar_unit_volume`]) — never one
//! builder with a default that is silently wrong for half its callers.
//!
//! A site whose copy pinned its OWN constants (a flat price, a bid/ask pair, a symbol) keeps a
//! one-line local helper that hands those constants to a builder here, so the values the test
//! depends on stay visible where it reads them and the struct literal is spelled once.
//!
//! # What is deliberately NOT here
//!
//! A builder is added when a migration can name ONE exact set of defaults and land every site that
//! matches it. The `Bar` and `QuoteTick` copies disagree about the fields a builder would pin, so
//! only the shapes below have one, and every other shape stays hand-written at its site:
//!
//! * a `Bar` whose OHLC and volume are a test's own data (each copy a different set of prices),
//!   which no builder could pin without hiding the numbers the test asserts on;
//! * a `QuoteTick` whose sizes are not both `1.0` (a maker test, a strategy test that quotes with
//!   zero sizes); a `BookUpdate` (no two copies pin the same levels).

use crate::{Bar, BookLevel, L2Book, QuoteTick, TradeTick};

/// One executed trade at `ts` for `size` at `price`, with the aggressor flag `is_buyer_maker`.
///
/// DEFAULTS: `local_ts` is `0` ("not stamped", the fixture/backfill convention) and `symbol` is
/// empty (the single-symbol path). A test that reads either builds its `TradeTick` by hand.
pub fn trade(ts: i64, price: f64, size: f64, is_buyer_maker: bool) -> TradeTick {
    TradeTick { ts, local_ts: 0, price, size, is_buyer_maker, symbol: String::new() }
}

/// One L1 quote at `ts`, `bid` over `ask`.
///
/// DEFAULTS: `local_ts` is `0` ("not stamped"), BOTH sizes are `1.0` and `symbol` is empty (the
/// single-symbol path). A test that reads a size or the symbol, or that quotes with zero or unequal
/// sizes, builds its `QuoteTick` by hand.
pub fn quote(ts: i64, bid: f64, ask: f64) -> QuoteTick {
    QuoteTick { ts, local_ts: 0, bid, ask, bid_size: 1.0, ask_size: 1.0, symbol: String::new() }
}

/// A flat bar at `ts`: `open == high == low == close == price`.
///
/// DEFAULTS: `volume` is `0.0`, and `funding`, `bid`, `ask` and `symbol` are all `None` — the
/// symbol-less shape a feed hands the engine before dispatch stamps `"SYMBOL.VENUE"`. A test that
/// reads the volume or a symbol builds its `Bar` by hand; one whose bars carry a volume of `1.0`
/// takes [`flat_bar_unit_volume`].
///
/// `crates/vike-model/src/orders/fill_trigger.rs`'s `one_price_bar` has this same shape and is NOT
/// this item: it is production code (the tick-to-bar adapter the conditional-order and paper-fill
/// checks share), so its `volume` is that code's own decision and a fixture default must not ride
/// on it.
pub fn flat_bar_zero_volume(ts: i64, price: f64) -> Bar {
    flat_bar(ts, price, 0.0)
}

/// A flat bar at `ts`: `open == high == low == close == price`.
///
/// DEFAULTS: `volume` is `1.0`, and `funding`, `bid`, `ask` and `symbol` are all `None`. The
/// twin of [`flat_bar_zero_volume`], for the tests whose bars carry a volume of exactly `1.0`; a
/// test that reads the volume or a symbol builds its `Bar` by hand.
pub fn flat_bar_unit_volume(ts: i64, price: f64) -> Bar {
    flat_bar(ts, price, 1.0)
}

/// [`flat_bar_unit_volume`] stamped with `symbol`: the shape a study's series hands its runner once
/// dispatch has named the instrument.
///
/// DEFAULTS: those of [`flat_bar_unit_volume`], with `symbol` set. vike-studio-core's study suites and
/// vike-user-research's pipeline each carried it.
pub fn flat_bar_unit_volume_on(symbol: &str, ts: i64, price: f64) -> Bar {
    Bar { symbol: Some(symbol.to_string()), ..flat_bar_unit_volume(ts, price) }
}

/// A bar at `ts` whose four prices all differ — `open 1.0`, `high 2.0`, `low 0.5`, `close 1.5` — so
/// a field swapped or dropped on a round trip (a wire, a store, a queue) shows, with `volume`.
///
/// DEFAULTS: those four prices, and `funding`, `bid`, `ask` and `symbol` all `None`.
pub fn distinct_ohlc_bar(ts: i64, volume: f64) -> Bar {
    Bar {
        ts,
        open: 1.0,
        high: 2.0,
        low: 0.5,
        close: 1.5,
        volume,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}

/// A bar with the given prices, for a test about one bar's SHAPE (a barrier touch, a range, a
/// trigger), not about when it happened.
///
/// DEFAULTS: `ts` is `0`, `volume` is `0.0`, and `funding`, `bid`, `ask` and `symbol` are all `None`.
pub fn shape_bar(open: f64, high: f64, low: f64, close: f64) -> Bar {
    Bar {
        ts: 0,
        open,
        high,
        low,
        close,
        volume: 0.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}

/// A bar at `ts` spanning `low..=high` that opens and closes at the midpoint, for a test about a
/// bar's excursion (how far a position ran each way), not its direction.
///
/// DEFAULTS: `open == close == (high + low) / 2`, `volume` is `0.0`, and `funding`, `bid`, `ask` and
/// `symbol` are all `None`.
pub fn mid_bar(ts: i64, high: f64, low: f64) -> Bar {
    let mid = (high + low) / 2.0;
    Bar {
        ts,
        open: mid,
        high,
        low,
        close: mid,
        volume: 0.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}

/// A bar for `symbol` priced off its quote, carrying `bid` and `ask`: `open == low == bid`,
/// `high == ask`, `close` the mid — what a combo test feeds, since a combo prices each leg off the side
/// it would actually trade.
///
/// DEFAULTS: `volume` is `0.0` and `funding` is `None`.
pub fn quoted_bar(ts: i64, symbol: &str, bid: f64, ask: f64) -> Bar {
    Bar {
        ts,
        open: bid,
        high: ask,
        low: bid,
        close: (bid + ask) / 2.0,
        volume: 0.0,
        funding: None,
        bid: Some(bid),
        ask: Some(ask),
        symbol: Some(symbol.to_string()),
    }
}

/// The one spelling of the flat, symbol-less bar; [`flat_bar_zero_volume`] and [`flat_bar_unit_volume`]
/// differ only in `volume`.
fn flat_bar(ts: i64, price: f64, volume: f64) -> Bar {
    Bar {
        ts,
        open: price,
        high: price,
        low: price,
        close: price,
        volume,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}

/// An [`L2Book`] holding exactly `bids` and `asks`, applied as one snapshot.
///
/// DEFAULTS: tick size `0.5` and snapshot sequence `1`. Tick `0.5` keeps every price/tick
/// round-trip binary-exact (an `n/2` grid), so an assertion over this book can be bit-exact rather
/// than tolerance-based; a test that needs another grid or another sequence builds its book by
/// hand.
pub fn book(bids: &[BookLevel], asks: &[BookLevel]) -> L2Book {
    let mut b = L2Book::new(0.5);
    b.apply_snapshot(1, bids, asks);
    b
}
