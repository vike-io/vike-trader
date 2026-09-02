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
//! mapper into the `vike-data` hist store OR (via [`ingest::ingest_file_clickhouse`]) the
//! `polymarket` ClickHouse tables the live L2 recorder writes; the `pmxt_backfill` bin
//! (`crates/vike-backfill/src/bin/pmxt_backfill.rs`) is the runnable entrypoint over it. [`ch_rows`]
//! is the pure JSONEachRow serializer for that ClickHouse write path, no I/O.

pub mod ch_rows;
pub mod ingest;
pub mod map;

pub use ch_rows::{book_event_json, l1_quote_json, trade_event_json};
pub use ingest::{download_hour, hour_url, ingest_file, ingest_file_clickhouse};
pub use map::{l1_from_row, map_row, MapState, Mapped, PmxtRow};
