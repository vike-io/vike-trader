//! Market-data value types for trading software: what a feed produces, as plain `serde` data.
//! Pure Rust, one dependency (`serde`), no `unsafe`.
//!
//! - [`Bar`] is one OHLCV candle (`Bar::ohlcv` builds the plain case); [`QuoteTick`] and
//!   [`TradeTick`] are a best-bid/ask update and a trade.
//! - [`BarConsolidator`], [`consolidate_trades`] and [`consolidate_quotes`] fold ticks into bars,
//!   and bars into coarser bars. A window closes only when a strictly later one starts, so a jittery
//!   live feed never steps backwards in time.
//! - [`L2Book`] and [`BookUpdate`] are a level-2 order book and the delta that updates it, with
//!   sequence-gap handling and walk-the-book price estimators.
//! - [`feed`] holds the derivatives values (mark and index price, funding rate, open interest,
//!   liquidation, volatility-index tick); [`option`] the option and option-chain snapshot types.
//! - [`aggressor`] says which side of a trade crossed the spread; [`interval`] parses bar
//!   intervals (`interval_ms("5m")`, `KLINE_MENU`).
//!
//! # Example
//!
//! ```
//! use vike_marketdata::{Bar, TradeTick, consolidate_trades};
//!
//! let trade = |ts: i64, price: f64, size: f64| TradeTick {
//!     ts,
//!     local_ts: 0,
//!     price,
//!     size,
//!     is_buyer_maker: false,
//!     symbol: String::new(),
//! };
//! // Three trades, folded into 60-second candles.
//! let ticks = [trade(1_000, 100.0, 1.0), trade(31_000, 101.0, 2.0), trade(61_000, 99.5, 1.0)];
//! let bars: Vec<Bar> = consolidate_trades(&ticks, 60_000);
//! assert!(!bars.is_empty());
//!
//! // Or build a candle by hand.
//! let bar = Bar::ohlcv(0, 100.0, 101.5, 99.0, 100.8, 1250.0);
//! assert_eq!(bar.close, 100.8);
//! ```
//!
//! # Compatibility
//!
//! The types derive `Serialize` and `Deserialize`, and fields added after the first release carry
//! `#[serde(default)]`, so older payloads keep deserializing. This is a `0.x` crate: the API can
//! change between minor versions.
//
// ---- Engineering notes: not rendered on docs.rs. They record why the crate exists and what
// ---- deliberately did not move into it; the public contract is the documentation above.
// The market-data vocabulary: what a FEED PRODUCES, and nothing that interprets it.
//
// Five modules, all of them types plus the arithmetic that builds those types from each other:
// [`bar`] (`Bar` and the two tick value types), [`orderbook`] (the `L2Book` reducer and the
// `BookUpdate` wire payload a venue task folds into it) and [`consolidator`] (the tick -> bar
// step) lived in `vike-model` until this crate existed, and the SPLIT is the only thing that
// changed: not a line of the arithmetic moved with them. [`feed`] (mark price, index price,
// funding rate, open interest, liquidation, volatility-index tick) and [`option`] (`OptionKind`,
// the live `OptionTicker` row and the chain snapshot types `OptionChain`/`Expiry`/`StrikeRow`/
// `OptionQuote`/`UnderlyingKind`, bundled per underlying as `UnderlyingChains`) are the values
// each venue used to define for itself; they are
// one venue-free shape here (`docs/decisions/0126-every-instrument-class-is-a-typed-contract-in-vike-model-and-feed-values-live-in-vike-marketdata.md`).
// The chain's chrono-using constructors (`make_expiry`, `expiry_ms`) and `limit_strikes` stay in
// `vike-options` with the option maths: this crate takes no `chrono`.
//
// Two more modules are the words a feed is READ and keyed by, moved down out of `vike-model`
// whole: [`aggressor`] (`Side`, `classify`, `signed` — which side of a `TradeTick` crossed the
// spread, the one reading of `is_buyer_maker`) and [`interval`] (`interval_ms`,
// `measures_bar_step`, `KLINE_MENU` — the bar-interval vocabulary and its one parser). Both are
// pure `std`; neither name is re-exported at this root, so each is spelled by its module path.
//
// `orderbook` is a folder module: its root holds `L2Book` and the reducer, `orderbook/wire.rs` the
// journal payloads (`BookLevel`, `BookUpdate`), `orderbook/seq.rs` the delta-apply law and
// `orderbook/walk.rs` the walk-the-book estimators and `book_taker_price`.
//
// # Why it is a crate rather than a module of `vike-model`
//
// `vike-model` is the DOMAIN vocabulary — orders, positions, fills, fees, venues, the
// `Broker`/`Strategy` seam, the rate-limit and margin tables — and 56 crates name it. These
// three files are a different vocabulary: they describe a market, not a trading account, and a
// crate that only reads a feed has no business linking the crate that also holds the order lane.
//
// MEASURED at the split: four crates' ENTIRE use of `vike_model` was these types —
// `vike-indicators` (`Bar`), `vike-orderflow` (`L2Book`/`BookLevel`/`TradeTick`), `vike-chart`
// (`Bar`/`L2Book`/`TradeTick`) and `vike-ai` (`Bar`, and only in a test module). Each of them
// linked 28 600 lines and six transitive dependencies (serde, serde_json, indexmap, ustr,
// compact_str, libm) to reach 882 lines and one.
//
// # The property this crate exists to hold, and the way to check it
//
// **One normal dependency, `serde`, and no `vike-*` dependency at all.** It sits one rank over the
// bottom of this workspace's own graph (`layer = 5`; the helper floor `vike-util` is 3, and this
// crate does not name it), which is what lets `vike-model` depend on it rather than the other way
// round. `crates/vike-ops/tests/architecture/layer_gate.rs` holds the
// direction; the dependency COUNT is held by the manifest's own comment and by nothing else, so
// `cargo tree -p vike-marketdata -e normal` is the check — it must print this crate and `serde`.
//
// # What deliberately did NOT move
//
// `impact.rs` reads an `L2Book` and did not come here: it is a market-impact MODEL over the
// book, not a description of one, and the line this crate draws is exactly there. It stayed in
// `vike-model` until 2026-10-12 and now lives beside its one consumer, the risk gate
// (`crates/vike-exec/src/risk/impact.rs`). The same reasoning kept `spread_quote.rs` (executable
// spread pricing, now the pairs strategy's own child,
// `crates/vike-strategy/src/strategies/pairs/spread_quote.rs`) and `tick_scheme.rs` (a venue's
// price-tier metadata, which is instrument vocabulary rather than feed output) out of this crate.
//
// # Re-exports
//
// None elsewhere: every name here has ONE public path, this crate's. `vike-model` names them in
// its own signatures (a `Broker` method takes an `&L2Book`) without re-exporting them, so a
// consumer spells `vike_marketdata::Bar` and takes a direct edge to this crate (decision 0067's
// amendment; the root `CLAUDE.md`'s one-name-per-symbol rule).
//
// # The `test-support` feature
//
// One cargo feature, EMPTY (no `dep:` entry, so the one-dependency property above is untouched):
// it compiles `test_support`, the shared test builders for the tick, bar and book types. A default
// build compiles none of it, and a consumer enables it through a dev-dependency edge only.

pub mod aggressor;
pub mod bar;
pub mod consolidator;
pub mod feed;
pub mod interval;
pub mod option;
pub mod orderbook;

// The shared market-data test builders. Behind `test-support` so a shipped build compiles NONE of
// it — the `vike-data`/`vike-model`/`vike-indicators` model. `cfg(test)` too, so this crate's own
// unit tests reach it without the feature being on.
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;

pub use bar::{Bar, QuoteTick, TradeTick};
pub use consolidator::{
    BarConsolidator, consolidate_quotes, consolidate_trades, quote_tick_to_bar, trade_tick_to_bar,
};
pub use feed::{FundingRate, IndexPrice, Liquidation, MarkPrice, OpenInterest, VolIndexTick};
pub use option::{
    Expiry, OptionChain, OptionKind, OptionQuote, OptionTicker, StrikeRow, UnderlyingChains,
    UnderlyingKind,
};
pub use orderbook::{
    BookLevel, BookUpdate, BookUpdateKind, DeltaDecision, FillSim, L2Book, SeqPolicy,
    book_taker_price,
};
