//! Databento historical HTTP API backfill — intraday bars/trades/quotes/L2-book from
//! `hist.databento.com/v0/timeseries.get_range` (raw CSV encoding) into the `vike-data` hist store.
//! A sibling of `pmxt`/`eod`; a DATA source (not an execution venue), so it lives here, not under
//! `crates/bridges/`. Design: `docs/superpowers/specs/2026-07-15-databento-tardis-adapters-design.md`.

pub mod client;
pub mod ingest;
pub mod parse;

pub use client::{GetRange, HIST_BASE, build_url, fetch_to_file};
pub use ingest::{DbnKind, backfill, ingest_file, ingest_str};
