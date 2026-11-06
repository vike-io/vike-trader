//! Fixtures two or more vike_archive test files share: the FLAT-layout `book_events` Parquet bytes
//! (their scratch store is the crate's `crate::test_support::store`). The row shape and the batch
//! builder are the archive reader's own fixtures (`vike_store::test_support`), shared with its decode
//! tests.

use bytes::Bytes;
use vike_store::test_support::{BookEventRow, TagColumns, book_events_parquet};

/// `rows` as a `book_events`-shaped Parquet file in the FLAT layout (`Binary` tags), forced to split into
/// row groups of at most `max_rows_per_group` rows each.
pub(super) fn write_book_events_parquet(
    rows: &[BookEventRow<'_>],
    max_rows_per_group: usize,
) -> Bytes {
    Bytes::from(book_events_parquet(TagColumns::Binary, rows, Some(max_rows_per_group)))
}
