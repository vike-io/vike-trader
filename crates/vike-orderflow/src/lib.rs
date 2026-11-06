//! vike-orderflow — pure trade-aggregation core (chart Phase 5, SP1).
//! Folds `vike_marketdata::TradeTick` streams into orderflow primitives. No I/O, no
//! rendering, no threading. See docs/superpowers/specs/2026-07-08-vike-orderflow-design.md.
//!
//! Every builder routes a trade by `vike_marketdata::aggressor::{Side, classify, signed}` — the
//! load-bearing semantic, `is_buyer_maker=false`→Buy/`true`→Sell, `delta = buy_vol − sell_vol`
//! everywhere below (the vocabulary moved to `vike-marketdata`; this crate re-exports none of it).
//!
//! One file per primitive: `bars` (`OrderflowBar` + `TickBarBuilder`/`VolumeBarBuilder`, the
//! latter splitting a trade proportionally across a volume threshold boundary); `profile`
//! (`VolumeProfile` — price-bucketed `PriceBin`s, POC, 70% value area, merged from footprints);
//! `footprint` (`FootprintBar` — a per-bar `PriceBin` grid); `bar_agg` (the chart's per-bar
//! footprint aggregator) and `tickvol` (tick and volume interval bars as chart bars).
//!
//! Gate discipline per builder: hand-computed expected-value tests (the real oracle — no
//! CPython twin here) + invariant/zero-skip tests + streaming↔batch bit-parity
//! (`f64::to_bits`; batch is a genuinely separate slice computation, not `push` in disguise).
//!
//! SP2 renders this, SP3 backfills it — both out of scope here.
/// Per-bar footprint aggregation a chart renders from: the drained trade tape bucketed into the
/// chart's bars by open time (moved here from vike-app-core, 2026-09-27).
pub mod bar_agg;
pub mod bars;
pub mod footprint;
pub mod profile;
/// Tick/volume interval bars from a trade tape, converted to `vike_marketdata::Bar` for a chart
/// (moved here from vike-app-core, 2026-09-27).
pub mod tickvol;
pub use bars::{OrderflowBar, TickBarBuilder, VolumeBarBuilder};
pub use footprint::FootprintBar;
pub use profile::{PriceBin, VolumeProfile};
