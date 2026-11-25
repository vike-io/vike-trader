//! The `data.vike.io` Polymarket archive's published Parquet shapes, as read by `vike_backfill`'s
//! `vike_archive` ingest and `events_api`: the column decoders, the `token_id` row-group pruning and
//! the venue id.
//!
//! ⚠ **The read-in-place store that stood here, `ArchiveParquetHistStore`, is DELETED** (2026-11-06):
//! its one caller was `backtest --archive`, which the owner ruled gone the same day, and every
//! history read now goes through a datahub (`docs/decisions/0084-only-the-datahub-touches-the-store.md`).
//! To read an archive day, import it with `vike_archive_backfill` and serve it with a key-less datahub.
//! Three things that store's history measured and worth carrying:
//!
//! * **`Ok(vec![])` means "none in this range", a claim of fact.** It once returned empty from
//!   `scan_quotes` because a `book_events` row has no L1 sizes, and a tick replay ran with its whole
//!   quote lane silently missing: the trailing scalper scored -2.74% where the same markets with quotes
//!   score -27.64% (8,288 trades vs 14,910). In a `HistoryStore`, "I cannot answer this" must never be
//!   spelled the same way as "the answer is none".
//! * **L1 folded from a `book_events` ladder is not the recorder's L1.** It is anchor-gated: until a
//!   `Snapshot` lands the fold only knows the levels that happen to have been touched, and a
//!   `GapStart` or `Stale` marker un-anchors it again until the next one.
//! * **Two real layouts.** The flat one ships three streams per date; the family day-file holds the
//!   whole 16-column `book_events` shape in one file, `token_id`-sorted, with `event_type`/`side` as
//!   plain `Utf8`, which is why the decoders read both columns through one string-or-binary accessor.
//!
//! **The decode is ONE copy.** `decode.rs` holds [`book_updates_from_batch`], [`trades_from_batch`]
//! and [`quotes_from_l1_batch`], and [`RowScope`] says which rows a caller wants. [`parse_levels_json`]
//! is shared the same way, with `vike_backfill`'s `events_api`.

mod decode;
mod prune;

pub use decode::{
    RowScope, book_kind_of, book_updates_from_batch, is_buyer_maker_of, parse_levels_json,
    quotes_from_l1_batch, trades_from_batch,
};
pub use prune::select_row_groups;

/// Vendor prefix for error messages (mirrors `vike_backfill::vike_archive`/`vike_backfill::arrowutil`'s `CTX`
/// convention).
const CTX: &str = "archive-store";

/// The archive's fixed decimal scales, for every stream the decoder reads — the ONE copy since the
/// ingest's own (`vike_backfill`'s `vike_archive`) went with its decoder. `price`/`best_bid`/
/// `best_ask`/`tick_size` and `l1_quotes`' `bid`/`ask` are scale-4; sizes are scale-6.
const PRICE_SCALE_DIVISOR: f64 = 10_000.0;
const SIZE_SCALE_DIVISOR: f64 = 1_000_000.0;

/// The only venue this archive publishes.
pub const VENUE: &str = "polymarket";

#[path = "archive_store_tests.rs"]
#[cfg(test)]
mod archive_store_tests;
