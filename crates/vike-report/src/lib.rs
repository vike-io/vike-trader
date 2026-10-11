//! `vike-report` — the live-journal READER that feeds the tearsheet: the journal and store doors,
//! and the `tearsheet` CLI over them.
//!
//! PR-1 made the vike-core command journal durable for paper/live fills (see
//! `vike-core/tests/journal/journal_wiring.rs::paper_synthesized_fill_is_journaled`); this crate READS
//! that fill stream and turns it into the same tearsheet a backtest produces.
//!
//! The parity contract: every closed-trade round-trip is reconstructed through
//! [`vike_model::compute_fill`] — the ONE cost-basis primitive `vike_exec::Account::fold` and
//! `vike_sim`'s `SimBroker::apply_fill` both delegate to — and every performance number is
//! composed by `vike_analytics` and keyed on `vike_analytics::metric_catalog::METRICS` (NO metric
//! is reimplemented here, and no roster of them is written down here). So a live tearsheet and a
//! backtest tearsheet over the same fill sequence are computed bit-for-bit identically, across the
//! WHOLE catalog rather than across whichever fields two hand-kept lists happened to share.
//!
//! # ⚠ The renderer half LEFT this crate on 2026-09-28
//!
//! This crate used to be two halves split by a default-on `journal` feature: a RENDERER half (the
//! `LiveTearsheet` document, the self-contained HTML tearsheet, the trade reconstruction and the
//! pure equity/mark-to-market folds) that needed vike-model + vike-analytics + serde, and a
//! READER half that needed the journal, the exec lane and the data layer. The renderer half named
//! nothing but `vike_model` and `vike_analytics`, so it moved INTO `vike-analytics`, where a
//! consumer that only renders (`crates/vike-cli/src/cmd/runs/show.rs`'s `html_document`) takes it
//! without this crate at all. Every consumer names it at that crate's root —
//! `vike_analytics::LiveTearsheet`, `vike_analytics::render_html`,
//! `vike_analytics::reconstruct_trades` and the rest.
//!
//! What stayed is everything that READS, so nothing here is optional any more and the `journal`
//! feature went with the split (and with it the CI lane whose only job was to build this crate
//! feature-off):
//!
//! - [`journal_read`] — the fill stream out of a command journal ([`fills_from_journal`]), and
//!   [`tearsheet_from_journal`], the journal door onto the
//!   document. It is a FREE function where it used to be the inherent
//!   `LiveTearsheet::from_journal`, because an inherent `impl` may not be written on a type another
//!   crate owns.
//! - [`store`] — [`equity_curve_from_store`] and [`mtm_curve_from_store`], the two builders that
//!   read a `HistStore` series before handing it to a pure fold in `vike-analytics`.
//! - [`excursions`] — [`backfill_excursions`], the per-trade `mae`/`mfe` enrichment off stored
//!   bars.
//! - [`tearsheet_cli`] — the `tearsheet` tool as a library function, so the bin and the
//!   `vike-backend` multicall dispatcher reach one copy.
//!
//! Layering: leaf crate (nothing depends on it downward), reusing vike-model
//! (compute_fill/Trade/FillEvent), **vike-analytics** (the tearsheet document, its renderer, the
//! reconstruction and the whole metric catalog), vike-journal (the journal), vike-exec (the
//! ingest-lane `Ingest`/`Event` the journal records wrap) and vike-data (the `HistStore` seam).
//!
//! NOTE the analytics edge is deliberately vike-**analytics**, not vike-**backtest**. Those
//! modules used to live in vike-backtest, so reusing them meant compiling 32,259 lines to reach
//! ~3,900 — the simulator, the paper exchange and the whole gated `harness/` tree came along, and
//! 89% of what this crate built it could never call. vike-analytics depends on vike-model alone,
//! so the catalog now arrives without the machine that produced it. The numbers are the same
//! functions; only their home changed.

pub mod excursions;
pub mod journal_read;
pub mod store;
// The `tearsheet` CLI as a library function, so the bin and the `vike-backend` multicall dispatcher
// reach one copy.
pub mod tearsheet_cli;

pub use excursions::backfill_excursions;
pub use journal_read::{JournalReadError, fills_from_journal, tearsheet_from_journal};
pub use store::{equity_curve_from_store, mtm_curve_from_store};
