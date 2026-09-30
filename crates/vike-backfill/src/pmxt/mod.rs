//! pmxt Polymarket-L2 historical archive — backfill collector.
//!
//! # ⚠⚠ OBSOLETE — THE ARCHIVE STOPPED PUBLISHING ON 2026-08-10, AND THIS COLLECTOR SAYS NOTHING
//!
//! **Ruled obsolete by the owner on 2026-09-21.** The measurement below is the evidence, taken
//! from the CI box the same day against the real bucket:
//!
//! * the LAST object published is **`polymarket_orderbook_2026-08-10T00.parquet`**. Every hour
//!   from `2026-08-10T01` onward answers **404** — 42 days at the time of writing, and counting;
//! * that is a STOP, not a naming misunderstanding: `2026-08-01` answers 200 at `T01`, `T06` and
//!   `T12`, so the archive really was hourly and really did end mid-day;
//! * the bucket still serves the OLD objects (`2026-06-01T00` and `2026-08-01T00` both 200), so
//!   this is the publisher stopping rather than the data being withdrawn;
//! * `archive.pmxt.dev` — the human-facing site this module attributes — refuses TCP connections
//!   outright.
//!
//! ⚠ **The failure is SILENT BY CONSTRUCTION, and that is the part worth carrying.**
//! [`ingest::download_hour`] maps a 404 to `Ok(None)` with the reason *"the hour hasn't been
//! published / doesn't exist — not an error, callers skip it"*, which is correct for an archive
//! that publishes on a lag and WRONG for one that has stopped. The two states are
//! indistinguishable at the call site, so a dead vendor and a quiet hour read identically: the
//! collector runs, finds nothing, and reports success. Nothing in this repository has said
//! otherwise since 2026-08-10.
//!
//! **Do not build on this module.** It is left in place rather than deleted because the ingested
//! history is real and the mapper is the reference for pmxt's Parquet schema; what is dead is the
//! SOURCE. If a replacement vendor appears, [`map`] is the part worth keeping.
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
//! commit key. ⚠ This paragraph used to end "the rest of this crate's ClickHouse surface —
//! `clickhouse_poly`, `backtest_bridge`'s `ClickHousePolyHistStore` and the
//! `poly_ch_backtest`/`poly_mm_batch` bins — is untouched BY THIS CHANGE", on the argument that a
//! READ path carries none of the duplication hazard that retired this WRITE one.
//!
//! **That argument held and the surface is gone anyway, on a different one.** `clickhouse_spot`
//! went first (2026-09-19), then `clickhouse_poly` and `backtest_bridge` (2026-09-20), because the
//! owner ruled that data is fetched by API or from the venue directly, never by reaching
//! ClickHouse — which is a rule about the ROUTE, not about idempotence, so a read path is no more
//! exempt from it than a write one. The two bins survive on the archive and local-store paths;
//! `crates/vike-data/src/backtest_store.rs`'s module doc carries what the deletion cost.
//! **No module in this workspace reaches a ClickHouse server by any route now.**

pub mod ingest;
pub mod map;

pub use ingest::{download_hour, hour_url, ingest_file};
pub use map::{MapState, Mapped, PmxtRow, l1_from_row, map_row};
