//! `vike-report` — the tearsheet RENDERER, plus the live-journal reader that feeds it.
//!
//! PR-1 made the vike-core command journal durable for paper/live fills (see
//! `vike-core/tests/journal/journal_wiring.rs::paper_synthesized_fill_is_journaled`); this crate READS
//! that fill stream and turns it into the same tearsheet a backtest produces.
//!
//! The parity contract: every closed-trade round-trip is reconstructed through
//! [`vike_model::compute_fill`] — the ONE cost-basis primitive `vike_exec::Account::fold` and
//! `vike_backtest`'s `SimBroker::apply_fill` both delegate to — and every performance number is
//! composed by `vike_analytics` and keyed on `vike_analytics::metric_catalog::METRICS` (NO metric
//! is reimplemented here, and no roster of them is written down here). So a live tearsheet and a
//! backtest tearsheet over the same fill sequence are computed bit-for-bit identically, across the
//! WHOLE catalog rather than across whichever fields two hand-kept lists happened to share.
//!
//! # ⚠ The crate is TWO HALVES, split by the `journal` feature
//!
//! The RENDERER half — [`report::LiveTearsheet`] (the document), [`html`] (the self-contained HTML
//! tearsheet), [`trades`], [`mtm`]'s folds and [`equity`]'s pure builders — needs vike-model,
//! vike-analytics and serde. Nothing else.
//!
//! The READER half — `journal_read`, `excursions`, `tearsheet_cli`,
//! `LiveTearsheet::from_journal`, `equity::equity_curve_from_store` and `mtm::mtm_curve_from_store`
//! — needs vike-core, vike-exec and vike-data, and sits behind the DEFAULT-ON `journal` feature.
//! (Named in plain code spans, not intra-doc links: they do not exist in every build of this crate,
//! and a link that resolves conditionally is worse than none.)
//! The manifest carries the argument for the split (and for why it is one feature rather than
//! three); the short version is that two doc comments in this tree — `vike-tradehub-client`'s
//! `Response::Tearsheet` and `crates/vike-cli/src/cmd/report.rs`'s module doc — refused to name
//! this crate's types BECAUSE of that closure, while `crates/vike-cli/src/cmd/runs/show.rs`'s
//! `refuse_an_unbuilt_renderer` told operators the renderer did not exist. It does; only its
//! dependency graph was in the way.
//!
//! Layering: leaf crate (nothing depends on it downward), reusing vike-model
//! (compute_fill/Trade/FillEvent), **vike-analytics** (the `metric_catalog` roster + the
//! `metrics`/`excursions`/`periods` catalog + `BacktestResult`/`BacktestReport` + the argv glue),
//! and — under `journal` — vike-core (the journal), vike-exec (the ingest-lane `Ingest`/`Event` the
//! journal records wrap) and vike-data (the `HistStore` seam).
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
#[cfg(feature = "journal")]
pub mod excursions;
pub mod html;
#[cfg(feature = "journal")]
pub mod journal_read;
pub mod mtm;
pub mod report;
pub mod trades;
// The `tearsheet` CLI as a library function, so the bin and the `vike` multicall dispatcher
// reach one copy. Behind `journal` because the tool's whole input is a journal directory.
#[cfg(feature = "journal")]
pub mod tearsheet_cli;

#[cfg(feature = "journal")]
pub use equity::equity_curve_from_store;
pub use equity::{equity_curve_from_samples, equity_curve_from_trades};
#[cfg(feature = "journal")]
pub use excursions::backfill_excursions;
pub use html::{render_html, render_html_with_stats};
#[cfg(feature = "journal")]
pub use journal_read::{JournalReadError, fills_from_journal};
#[cfg(feature = "journal")]
pub use mtm::mtm_curve_from_store;
pub use mtm::{MtmPoint, RuntimeStats, mtm_equity_curve, reconstruct_mtm};
pub use report::{
    LiveTearsheet, MetricValues, NOT_RECORDED, TEARSHEET_SCHEMA, TEARSHEET_SCHEMA_UNVERSIONED,
};
pub use trades::reconstruct_trades;
