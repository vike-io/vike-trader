//! The market-data vocabulary: what a FEED PRODUCES, and nothing that interprets it.
//!
//! Three modules, all of them types plus the arithmetic that builds those types from each other:
//! [`bar`] (`Bar` and the two tick value types), [`orderbook`] (the `L2Book` reducer and the
//! `BookUpdate` wire payload a venue task folds into it) and [`consolidator`] (the tick -> bar
//! step). Every one of them lived in `vike-model` until this crate existed, and the SPLIT is the
//! only thing that changed: not a line of the arithmetic moved with them.
//!
//! # Why it is a crate rather than a module of `vike-model`
//!
//! `vike-model` is the DOMAIN vocabulary — orders, positions, fills, fees, venues, the
//! `Broker`/`Strategy` seam, the rate-limit and margin tables — and 56 crates name it. These
//! three files are a different vocabulary: they describe a market, not a trading account, and a
//! crate that only reads a feed has no business linking the crate that also holds the order lane.
//!
//! MEASURED at the split: four crates' ENTIRE use of `vike_model` was these types —
//! `vike-indicators` (`Bar`), `vike-orderflow` (`L2Book`/`BookLevel`/`TradeTick`), `vike-chart`
//! (`Bar`/`L2Book`/`TradeTick`) and `vike-ai` (`Bar`, and only in a test module). Each of them
//! linked 28 600 lines and six transitive dependencies (serde, serde_json, indexmap, ustr,
//! compact_str, libm) to reach 882 lines and one.
//!
//! # The property this crate exists to hold, and the way to check it
//!
//! **One normal dependency, `serde`, and no `vike-*` dependency at all.** It is the bottom of this
//! workspace's own graph (`layer = 5`, the lowest declared rank), which is what lets `vike-model`
//! depend on it rather than the other way round. `crates/vike-ops/tests/layer_gate.rs` holds the
//! direction; the dependency COUNT is held by the manifest's own comment and by nothing else, so
//! `cargo tree -p vike-marketdata -e normal` is the check — it must print this crate and `serde`.
//!
//! # What deliberately did NOT move
//!
//! `impact.rs` reads an `L2Book` and stays in `vike-model`: it is a market-impact MODEL over the
//! book, not a description of one, and the line this crate draws is exactly there. The same
//! reasoning keeps `spread_quote.rs` (executable spread pricing) and `tick_scheme.rs` (a venue's
//! price-tier metadata, which is instrument vocabulary rather than feed output) where they are.
//!
//! # Re-exports
//!
//! `vike-model` re-exports every name below at ITS crate root, so `vike_model::Bar` and
//! `vike_model::L2Book` still resolve and no consumer that wants the domain vocabulary changed a
//! line. That is the crate-root-vocabulary exception to this workspace's no-shim-on-a-move rule
//! (root `CLAUDE.md`, *Conventions that will bite you if ignored*) and not a compatibility shim:
//! the domain vocabulary genuinely names these types, and a `Broker` method taking an `&L2Book`
//! would be unreadable if its own crate could not spell the argument.

pub mod bar;
pub mod consolidator;
pub mod orderbook;

pub use bar::{Bar, QuoteTick, TradeTick};
pub use consolidator::{
    BarConsolidator, consolidate_quotes, consolidate_trades, quote_tick_to_bar, trade_tick_to_bar,
};
pub use orderbook::{
    BookLevel, BookUpdate, BookUpdateKind, DeltaDecision, FillSim, L2Book, SeqPolicy,
    book_taker_price,
};
