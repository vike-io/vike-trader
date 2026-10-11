//! Market-data codecs: bars, equity-curve samples, quotes and trades.

use std::sync::Arc;

use datafusion::arrow::array::{Array, ArrayRef};
use datafusion::arrow::datatypes::{DataType, Field, Schema};
use datafusion::arrow::record_batch::RecordBatch;

use vike_model::{Bar, EquitySample, QuoteTick, TradeTick};

use crate::store::hist::DataError;

use super::{
    RowSymbolFn, SeriesCodec, bool_col, f64_col, gather_bool, gather_f64, gather_i64,
    gather_opt_f64, gather_str, i64_col, opt_i64_col, opt_str_col,
};

// ---- bars ------------------------------------------------------------------------------------

series_codec! {
    Codec = BarCodec,
    Row = Bar,
    rows = rows,
    schema_fn = bar_schema,
    columns_fn = bar_columns,
    decode_fn = bars_from_batch,
    fields {
        ts: i64 = |&i| rows[i].ts,
        open: f64 = |&i| rows[i].open,
        high: f64 = |&i| rows[i].high,
        low: f64 = |&i| rows[i].low,
        close: f64 = |&i| rows[i].close,
        volume: f64 = |&i| rows[i].volume,
        funding: f64_null = |&i| rows[i].funding,
    },
    decode_row = |i| Bar {
        ts: ts.value(i),
        open: open.value(i),
        high: high.value(i),
        low: low.value(i),
        close: close.value(i),
        volume: volume.value(i),
        funding: (!funding.is_null(i)).then(|| funding.value(i)),
        bid: None,
        ask: None,
        symbol: None,
    },
    sort_key = |row| (row.ts, 0),
}

// ---- equity samples (equity-curve durable store) ----------------------------------------------
// `kind=equity` series: `HistStore::append_equity`/`scan_equity` (portfolio-observer PR-3 Task 2)
// are the callers — the durable twin of the vike-core equity sampler's output. Same shape as
// `kind=properties`: no venue/symbol Parquet column (identity lives in the partition path
// `kind=/venue=/symbol=`); `EquitySample.venue` is re-injected from the caller-supplied `symbol` on
// scan (the recorder passes the per-exchange venue name or "TOTAL" as `symbol`, under the fixed
// `venue="portfolio"` partition namespace at the call site).

const EQUITY_SCHEMA_VERSION: &str = "1";

series_codec! {
    Codec = EquityCodec,
    Row = EquitySample,
    rows = rows,
    ctx = venue,
    schema_fn = equity_schema,
    columns_fn = equities_to_batch,
    decode_fn = batch_to_equities,
    meta = ("vike.schema.equity", EQUITY_SCHEMA_VERSION),
    fields {
        ts: i64 = |&i| rows[i].ts,
        equity: f64 = |&i| rows[i].equity,
        realized: f64 = |&i| rows[i].realized,
        unrealized: f64 = |&i| rows[i].unrealized,
        missing_prices: i64 = |&i| rows[i].missing_prices as i64,
    },
    decode_row = |i| EquitySample {
        ts: ts.value(i),
        venue: venue.to_string(),
        equity: equity.value(i),
        realized: realized.value(i),
        unrealized: unrealized.value(i),
        missing_prices: missing_prices.value(i) as u32,
    },
    sort_key = |row| (row.ts, 0),
}

// ---- quotes ----------------------------------------------------------------------------------

series_codec! {
    Codec = QuoteCodec,
    Row = QuoteTick,
    rows = rows,
    ctx = symbol,
    schema_fn = quote_schema,
    columns_fn = quote_columns,
    decode_fn = quotes_from_batch,
    row_symbol = symbol,
    fields {
        ts: i64 = |&i| rows[i].ts,
        bid: f64 = |&i| rows[i].bid,
        ask: f64 = |&i| rows[i].ask,
        bid_size: f64 = |&i| rows[i].bid_size,
        ask_size: f64 = |&i| rows[i].ask_size,
        // additive (Task 5): appended LAST + nullable so old parts (lacking it) still decode
        local_ts: i64_add = |&i| rows[i].local_ts,
        // additive (series grouping): the row's OWN symbol. Redundant under the default per-symbol
        // layout — the series path already names it and `ctx` supplies it — and LOAD-BEARING under
        // a grouped series, where one part holds many symbols and the path can no longer answer
        // "whose row is this?". Written always, so a store can be regrouped later without a
        // rewrite; read through `opt_str_col`, so parts predating it decode via `ctx` as before.
        symbol_col: str_add = |&i| rows[i].symbol.as_str(),
    },
    decode_row = |i| QuoteTick {
        ts: ts.value(i),
        local_ts: local_ts.map_or(0, |c| c.value(i)), // absent in pre-Task-5 parts → default 0
        bid: bid.value(i),
        ask: ask.value(i),
        bid_size: bid_size.value(i),
        ask_size: ask_size.value(i),
        // Prefer the row's own symbol, falling back to the path-derived `ctx` when the column is
        // ABSENT (a part predating it) or EMPTY. The empty case is not defensive padding — it is
        // this store's documented contract: a caller on a single-symbol path may leave
        // `QuoteTick::symbol` empty ("instrument id — empty for single-symbol paths") and the store
        // tags it from the series path on read. Preferring the column unconditionally silently
        // turned those rows' symbol into "" — caught by `datafusion_ticks_round_trip`. Under a
        // GROUPED series an empty symbol is meaningless (nothing distinguishes the rows), so the
        // grouped write path must REQUIRE a concrete one; that is a writer-side invariant, not
        // something the reader should paper over.
        symbol: symbol_col
            .as_mut()
            .map(|v| std::mem::take(&mut v[i]))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| symbol.to_string()),
    },
    sort_key = |row| (row.ts, 0),
}

// ---- trades ----------------------------------------------------------------------------------

series_codec! {
    Codec = TradeCodec,
    Row = TradeTick,
    rows = rows,
    ctx = symbol,
    schema_fn = trade_schema,
    columns_fn = trade_columns,
    decode_fn = trades_from_batch,
    row_symbol = symbol,
    fields {
        ts: i64 = |&i| rows[i].ts,
        price: f64 = |&i| rows[i].price,
        size: f64 = |&i| rows[i].size,
        is_buyer_maker: bool = |&i| rows[i].is_buyer_maker,
        // additive (Task 5): appended LAST + nullable so old parts (lacking it) still decode
        local_ts: i64_add = |&i| rows[i].local_ts,
        // additive (series grouping) — see the quote codec's note; same reasoning verbatim.
        symbol_col: str_add = |&i| rows[i].symbol.as_str(),
    },
    decode_row = |i| TradeTick {
        ts: ts.value(i),
        local_ts: local_ts.map_or(0, |c| c.value(i)), // absent in pre-Task-5 parts → default 0
        price: price.value(i),
        size: size.value(i),
        is_buyer_maker: is_buyer_maker.value(i),
        // Column-then-`ctx`, absent OR empty → `ctx`; see the quote codec's note for why the empty
        // case is contract rather than padding.
        symbol: symbol_col
            .as_mut()
            .map(|v| std::mem::take(&mut v[i]))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| symbol.to_string()),
    },
    sort_key = |row| (row.ts, 0),
}
