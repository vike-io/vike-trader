//! ClickHouse Polymarket backfill collector — pull L1 top-of-book (`l1_quotes`, the live L2
//! recorder's table, or `polymarket_snapshots`, the retired Python poller's table — see
//! [`ingest::QuoteSource`]/[`ingest::resolve_quote_source`] and `ingest`'s module doc for the split),
//! the trade tape (`polymarket_trades`), and (the BOOK lane) L2 book updates (`book_events`, the live
//! L2 recorder's table) out of the latency box's local `polymarket` ClickHouse DB into the `vike-data`
//! DataFusion hist store, keyed `venue=polymarket`/`symbol=token_id`, the same series shape the
//! live Polymarket feed and the pmxt archive backfill write. This is the one-time-export step of
//! the `poly_mm_batch --store DIR` fast path (see that bin's module doc): once a token universe's
//! days are exported here, repeated `poly_mm_batch` runs read the local Parquet store instead of
//! shelling `clickhouse-client` per token per run.
//!
//! This is the local-data twin of `pmxt` (which pulls the same Polymarket L2 from a remote R2
//! archive over HTTP): here the source is already on the box, so [`ingest`] shells the
//! already-authenticated `clickhouse-client` for a `FORMAT Parquet` export per UTC day and drives
//! it through the same arrow decode → store-append path. [`map`] is the pure row→domain decode
//! (fixture-tested, no I/O). The BOOK lane is the odd one out: `book_events` carries no `slug`
//! column, so it resolves its token universe up front (`book_tokens_query`/`fetch_book_tokens`)
//! then reuses `crate::backtest_bridge::ClickHousePolyHistStore::scan_book_updates` verbatim per
//! token per day (`ingest_book_day`) rather than a second decoder. The `clickhouse_poly_backfill`
//! bin (`crates/vike-backfill/src/bin/clickhouse_poly_backfill.rs`) is the runnable entrypoint.

pub mod ingest;
pub mod map;

pub use ingest::{
    DB, L1_QUOTES_TABLE, QuoteSource, ResolvedQuoteSource, SNAPSHOTS_TABLE, TradeSymbolKey, VENUE,
    book_tokens_query, fetch_book_tokens, ingest_book_day, ingest_quotes_file, ingest_trades_file,
    l1_quotes_query, quotes_count_query_l1, quotes_count_query_snapshots, quotes_query,
    resolve_quote_source, run_count_query, run_export, slug_filter, token_universe_count_query,
    trades_query,
};
pub use map::{quotes_from_batch, tokens_from_batch, trades_from_batch};
