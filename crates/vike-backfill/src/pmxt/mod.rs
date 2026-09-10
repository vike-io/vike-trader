//! pmxt Polymarket-L2 historical archive — backfill collector.
//!
//! Data source: the [pmxt](https://archive.pmxt.dev) Polymarket order-book archive, licensed
//! [CC BY 4.0](https://creativecommons.org/licenses/by/4.0/). Attribution per the license terms:
//! this module ingests Parquet files published by **pmxt** (`archive.pmxt.dev`); pmxt is not
//! affiliated with this project.
//!
//! [`map`] is the pure row→domain mapper: decoded pmxt Parquet row columns →
//! `vike_model::{BookUpdate, TradeTick}`, no I/O. [`ingest`] is the Parquet streaming reader
//! (bounded memory — one row group at a time) plus the streaming HTTP download that drives the
//! mapper into the `vike-data` hist store; the `pmxt_backfill` bin
//! (`crates/vike-backfill/src/bin/pmxt_backfill.rs`) is the runnable entrypoint over it.
//!
//! ⚠ **THE CLICKHOUSE WRITE PATH IS GONE, removed 2026-09-09.** This module also carried
//! `ingest_file_clickhouse` — the same download/decode/map pipeline with the rows INSERTed into the
//! `polymarket` ClickHouse tables the live L2 recorder writes — plus a `ch_rows` JSONEachRow
//! serializer for it. It went for two reasons, and the second is the one that matters:
//!
//!   * **Nothing scheduled it.** `git grep` finds no cron, no timer and no systemd unit invoking
//!     `pmxt_backfill --clickhouse` anywhere in this repository. It was a hand-run tool, and the
//!     recorder that fills those tables lives out of tree.
//!   * **It was NOT IDEMPOTENT and said so.** Its own doc warned that re-running an hour duplicates
//!     rows, because the insert had no per-row dedup. That is not a hypothetical: it put 33.5M
//!     duplicate rows into ClickHouse once. A tool whose safe use depends on the operator
//!     remembering which hours already ran is a tool that will eventually be run twice.
//!
//! The hist-store path ([`ingest::ingest_file`]) is untouched and is idempotent by the store's own
//! commit key. ⚠ The rest of this crate's ClickHouse surface — `clickhouse_poly`, `clickhouse_spot`,
//! `backtest_bridge`'s `ClickHousePolyHistStore` and the `poly_ch_backtest`/`poly_mm_batch` bins —
//! is DELIBERATELY untouched: those READ tables the live recorder still fills, and read paths carry
//! none of the duplication hazard that retired this one.

pub mod ingest;
pub mod map;

pub use ingest::{download_hour, hour_url, ingest_file};
pub use map::{MapState, Mapped, PmxtRow, l1_from_row, map_row};
