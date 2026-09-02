//! ClickHouse **spot 1-second** backfill collector — pull `data_history.spot_1s` out of the latency box's
//! ClickHouse into the `vike-data` DataFusion hist store as a `kind=quote` tick series.
//!
//! The sibling of [`crate::clickhouse_poly`] (same `clickhouse-client --query "... FORMAT Parquet"`
//! export → arrow row-group decode → store-append shape, same SELECT-only / per-day-commit-key
//! discipline); the difference is the source table and the series shape. This is the reference
//! spot series `vike_backtest::CheapNp` samples for σ and `s_now` — the backlog's G8.
//!
//! # The two load-bearing source traps
//!
//! 1. **`spot_1s` is a `ReplacingMergeTree(_v)`.** A non-`FINAL` `SELECT` can return superseded
//!    duplicate rows whenever parts are unmerged. [`ingest::spot_query`] therefore reads
//!    `FROM data_history.spot_1s FINAL`, and a unit test pins the keyword — dropping it would
//!    silently double-count seconds and change every downstream σ.
//! 2. **~11 % of the seconds are MISSING** (≈76,845 of 86,400 on a typical day). The series is
//!    stored **RAW and SPARSE** here, deliberately: `vike_backtest::fair_value::trailing_sigma`
//!    builds its own contiguous 1-second grid and LINEARLY INTERPOLATES the missing seconds
//!    itself (see that function's step 3 — it is the port of the backtest SQL's
//!    `WITH FILL STEP 1 INTERPOLATE (px)`). Materializing an interpolated grid HERE would feed
//!    the estimator already-filled seconds and double-interpolate; storing raw keeps the store a
//!    faithful record of what the venue actually published and leaves the gap policy where the
//!    strategy can see it.
//!
//! # Series shape
//!
//! One `kind=quote` tick per observed second, `bid == ask == px` and zero sizes — a scalar price
//! series has no book. `QuoteTick::mid()` (and `CheapNp::on_quote_tick`'s own
//! `0.5 * (bid + ask)`) then returns `px` EXACTLY: `0.5 * (p + p) == p` is exact in binary f64 for
//! every finite `p`, so the round trip through the quote shape is lossless. `quote` rather than
//! `bar` is also what makes the replay ordering right — `hist_replay`'s equal-`ts` tie-break is
//! Book → Quote → Trade, so a spot sample stamped the same millisecond as a Polymarket print is
//! delivered to the strategy BEFORE that print, which is the causally honest order.

pub mod ingest;
pub mod map;

pub use ingest::{ingest_spot_file, spot_query, DB, MARKET, TABLE, VENUE};
pub use map::spot_from_batch;
