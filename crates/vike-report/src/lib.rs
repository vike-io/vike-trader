//! `vike-report` — the LIVE tearsheet reader (PR-2 of the live-tearsheet arc).
//!
//! PR-1 made the vike-core command journal durable for paper/live fills (see
//! `vike-core/tests/journal/journal_wiring.rs::paper_synthesized_fill_is_journaled`); this crate READS
//! that fill stream and turns it into the same tearsheet a backtest produces.
//!
//! The parity contract: every closed-trade round-trip is reconstructed through
//! [`vike_model::compute_fill`] — the ONE cost-basis primitive `vike_exec::Account::fold` and
//! `vike_backtest`'s `SimBroker::apply_fill` both delegate to — and every performance number is a
//! call into `vike_analytics::metrics` (NO metric is reimplemented here). So a live tearsheet and a
//! backtest tearsheet over the same fill sequence are computed bit-for-bit identically.
//!
//! Layering: leaf crate (nothing depends on it), reusing vike-model (compute_fill/Trade/FillEvent),
//! **vike-analytics** (the `metrics`/`excursions`/`periods` catalog + `BacktestResult`/
//! `BacktestReport` + the argv glue), vike-core (the journal), vike-exec (the ingest-lane
//! `Ingest`/`Event` the journal records wrap), and vike-data (the optional
//! `HistStore::scan_equity` equity-curve source).
//!
//! NOTE the analytics edge is deliberately vike-**analytics**, not vike-**backtest**. Those
//! modules used to live in vike-backtest, so reusing them meant compiling 32,259 lines to reach
//! ~3,900 — the simulator, the paper exchange and the whole gated `harness/` tree came along, and
//! 89% of what this crate built it could never call. vike-analytics depends on vike-model alone,
//! so the catalog now arrives without the machine that produced it. The numbers are the same
//! functions; only their home changed.
//!
//! The [`mtm`] module adds a REPORT-SIDE alternative to the persisted equity sampler: a
//! positions-over-time fold from the journal fills, marked to market against `HistStore` bar
//! prices (with the sampler's missing-price convention), plus the [`mtm::RuntimeStats`]
//! turnover/exposure/margin summary the HTML tearsheet renders. The live `kind=equity` sampler
//! stays the primary source; nothing here is persisted.

pub mod equity;
pub mod excursions;
pub mod html;
pub mod journal_read;
pub mod mtm;
pub mod report;
pub mod trades;
// The `tearsheet` CLI as a library function, so the bin and the `vike` multicall dispatcher
// reach one copy. Not feature-gated: this crate is DataFusion-free and `binutil` is feature-free,
// so the module costs nothing to compile unconditionally.
pub mod tearsheet_cli;

pub use equity::{equity_curve_from_samples, equity_curve_from_store, equity_curve_from_trades};
pub use excursions::backfill_excursions;
pub use html::{render_html, render_html_with_stats};
pub use journal_read::{fills_from_journal, JournalReadError};
pub use mtm::{mtm_curve_from_store, mtm_equity_curve, reconstruct_mtm, MtmPoint, RuntimeStats};
pub use report::LiveTearsheet;
pub use trades::reconstruct_trades;
