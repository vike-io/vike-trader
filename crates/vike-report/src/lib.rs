//! `vike-report` — the live-journal READER that feeds the tearsheet. It reads the command journal
//! and nothing else: it names no store, so it links no `vike-data`. A library of readers and
//! nothing else.
//!
//! The parity contract: every closed-trade round-trip is reconstructed through
//! [`vike_model::compute_fill`] — the ONE cost-basis primitive `vike_exec::Account::fold` and
//! `vike_sim`'s `SimBroker::apply_fill` both delegate to — and every performance number is
//! composed by `vike_analytics` and keyed on `vike_analytics::metric_catalog::METRICS`. NO metric
//! is reimplemented here, so a live tearsheet and a backtest tearsheet over the same fill sequence
//! are computed bit-for-bit identically across the whole catalog.
//!
//! - [`journal_read`] — the fill stream out of a command journal ([`fills_from_journal`]), and
//!   [`tearsheet_from_journal`], the journal door onto the document; [`tearsheet_from_fills`], the
//!   same document over fills a caller already holds (the desktop converts the `kind=exec_fill` rows
//!   it read from the datahub with `vike_data::ExecFillRow::to_fill_event` first).
//!
//! The renderer half (the `LiveTearsheet` document, its HTML renderer, the trade reconstruction
//! and the pure equity/mark-to-market folds) lives in `vike-analytics`. The `tearsheet` CLI that
//! composes these readers lives in `crates/vike`, and so do the tearsheet's STORE reads
//! (`equity_curve_from_store`, `mtm_curve_from_store`, `backfill_excursions`): they take a
//! `HistStore`, which is the `--datahub` path's business and not the journal reader's.
//!
//! The analytics edge is deliberately vike-**analytics**, not vike-**backtest**: its only vike
//! dependency is vike-model, so the catalog arrives without the simulator that produced it.

pub mod journal_read;

pub use journal_read::{
    JournalReadError, fills_from_journal, tearsheet_from_fills, tearsheet_from_journal,
};
