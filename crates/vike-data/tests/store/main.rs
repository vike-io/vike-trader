//! Gate for the DataFusion+Parquet HistStore: bars round-trip bit-for-bit, ts-range filtering,
//! ingest idempotency + the manifest file-index, `resample_*_to_bars` == the parity-tested
//! consolidator, and (slice 4) `date=` partitioning + compaction + retention. Only compiled/run
//! with `--features hist-datafusion`.
#![cfg(feature = "hist-datafusion")]

mod bulk;
mod common;
mod compaction;
mod depth_and_budget;
mod grouped_coverage_recorder;
mod grouped_series;
mod inventory_and_delete;
mod manifest_and_rebuild;
mod repair;
mod round_trip;
mod source_dimension;
mod superseding_commits;
mod supersession_policy;
mod wal_and_maintenance;

mod grouped_compaction;
mod manifest_delta;
mod parquet_export;
mod supersede_refusal;
mod write_granularity_ab;
