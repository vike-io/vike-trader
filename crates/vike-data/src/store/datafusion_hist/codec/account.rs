//! Account codecs: execution fills and orders, and realized perp funding.

use std::sync::Arc;

use datafusion::arrow::array::{Array, ArrayRef};
use datafusion::arrow::datatypes::{DataType, Field, Schema};
use datafusion::arrow::record_batch::RecordBatch;

use crate::exec_log::{ExecFillRow, ExecOrderRow};
use crate::funding_log::FundingRow;
use crate::store::hist::DataError;

use super::{
    SeriesCodec, f64_col, gather_f64, gather_i64, gather_opt_f64, gather_opt_str, gather_str,
    i64_col, opt_f64_col, opt_str_col, str_values, str_values_required,
};

// ---- execution trade-log (kind=exec_fill / kind=exec_order) -----------------------------------
// The ACCOUNT fill/order series (Tier-2) — DISTINCT kinds from the market `kind=trade` series so
// account fills never collide with a symbol's public prints. Row types live in `crate::exec_log`
// (always compiled, model-only); the codecs sit here because `SeriesCodec` is module-private.
//
// Unlike the equity/quote/trade codecs (which strip venue/symbol and re-inject one of them from the
// scan's `ctx`), these rows carry BOTH `venue` and `symbol` as fields — a single `ctx: &str` can't
// re-inject two values — so every field is stored as a real column and `decode` ignores `ctx`
// (exactly like `BarCodec`/`BookCodec`). This also makes the `compact_roundtrip` re-encode (which
// passes `ctx=""`) preserve venue/symbol verbatim.

series_codec! {
    Codec = ExecFillCodec,
    Row = ExecFillRow,
    rows = rows,
    schema_fn = exec_fill_schema,
    columns_fn = exec_fill_columns,
    decode_fn = exec_fills_from_batch,
    fields {
        ts: i64 = |&i| rows[i].ts,
        trade_id: str = |&i| rows[i].trade_id.as_str(),
        client_order_id: str = |&i| rows[i].client_order_id.as_str(),
        venue: str = |&i| rows[i].venue.as_str(),
        symbol: str = |&i| rows[i].symbol.as_str(),
        side: i64 = |&i| rows[i].side as i64,
        qty: f64 = |&i| rows[i].qty,
        px: f64 = |&i| rows[i].px,
        commission: f64 = |&i| rows[i].commission,
        // additive: appended LAST + nullable so old parts (lacking it) still decode, exactly like
        // quote/trade's `local_ts` (the `f64_add` flavor reads through `opt_f64_col`, not `f64_col`).
        // Perp venues (bybit/okx) surface it at fill time; not every venue does.
        mark_price: f64_add = |&i| rows[i].mark_price,
        // additive, appended AFTER mark_price (MM fill-rate/adverse-selection analytics): "maker" |
        // "taker" | "" (not surfaced). `str_add` reads through `opt_str_col`, so parts written before
        // this column existed decode with `""` per row.
        liquidity_side: str_add = |&i| rows[i].liquidity_side.as_str(),
        // additive, appended LAST: fee currency of `commission` (e.g. "USDT", "BNB"); "" if not
        // surfaced. Same `str_add`/`opt_str_col` schema-tolerance contract as `liquidity_side`.
        commission_asset: str_add = |&i| rows[i].commission_asset.as_str(),
    },
    decode_row = |i| ExecFillRow {
        ts: ts.value(i),
        trade_id: std::mem::take(&mut trade_id[i]),
        client_order_id: std::mem::take(&mut client_order_id[i]),
        venue: std::mem::take(&mut venue[i]),
        symbol: std::mem::take(&mut symbol[i]),
        side: side.value(i) as i32,
        qty: qty.value(i),
        px: px.value(i),
        commission: commission.value(i),
        // absent in pre-mark_price parts → None
        mark_price: mark_price.and_then(|c| (!c.is_null(i)).then(|| c.value(i))),
        // absent in pre-liquidity_side/commission_asset parts → ""
        liquidity_side: liquidity_side
            .as_mut()
            .map(|v| std::mem::take(&mut v[i]))
            .unwrap_or_default(),
        commission_asset: commission_asset
            .as_mut()
            .map(|v| std::mem::take(&mut v[i]))
            .unwrap_or_default(),
    },
    sort_key = |row| (row.ts, 0),
}

series_codec! {
    Codec = ExecOrderCodec,
    Row = ExecOrderRow,
    rows = rows,
    schema_fn = exec_order_schema,
    columns_fn = exec_order_columns,
    decode_fn = exec_orders_from_batch,
    fields {
        ts: i64 = |&i| rows[i].ts,
        client_order_id: str = |&i| rows[i].client_order_id.as_str(),
        venue: str = |&i| rows[i].venue.as_str(),
        symbol: str = |&i| rows[i].symbol.as_str(),
        side: i64 = |&i| rows[i].side as i64,
        qty: f64 = |&i| rows[i].qty,
        order_type: str = |&i| rows[i].order_type.as_str(),
        status: str = |&i| rows[i].status.as_str(),
        price: f64_null = |&i| rows[i].price,
        trigger_price: f64_null = |&i| rows[i].trigger_price,
        venue_order_id: str_null = |&i| rows[i].venue_order_id.as_deref(),
        filled_qty: f64 = |&i| rows[i].filled_qty,
        avg_fill_px: f64 = |&i| rows[i].avg_fill_px,
    },
    decode_row = |i| ExecOrderRow {
        ts: ts.value(i),
        client_order_id: std::mem::take(&mut client_order_id[i]),
        venue: std::mem::take(&mut venue[i]),
        symbol: std::mem::take(&mut symbol[i]),
        side: side.value(i) as i32,
        qty: qty.value(i),
        order_type: std::mem::take(&mut order_type[i]),
        status: std::mem::take(&mut status[i]),
        price: (!price.is_null(i)).then(|| price.value(i)),
        trigger_price: (!trigger_price.is_null(i)).then(|| trigger_price.value(i)),
        venue_order_id: venue_order_id[i].take(),
        filled_qty: filled_qty.value(i),
        avg_fill_px: avg_fill_px.value(i),
    },
    sort_key = |row| (row.ts, 0),
}

// ---- realized perp funding (kind=exec_funding) -------------------------------------------------
// The ACCOUNT realized-funding series (Tier-2) — a DISTINCT kind from the market `kind=trade` series
// so account funding never collides with a symbol's public prints. Row type lives in
// `crate::funding_log` (always compiled, model-only); the codec sits here because `SeriesCodec` is
// module-private. Identity `(venue, symbol=<coin>)` is the partition path only — every field
// (`ts`/`account`/`usdc`/`szi`/`funding_rate`/`hash`) is intrinsic to the row, so `decode` ignores
// `ctx` (exactly like `BarCodec`/`ExecFillCodec`), and the `compact_roundtrip` re-encode (which
// passes `ctx=""`) preserves every column verbatim.
//
// ⚠ `account` is a STORED COLUMN rather than a partition level, per `docs/decisions/0080-the-account-funding-kind-takes-the-qualified-name.md` — the
// partition is `(venue, coin)` and the commit key carries `{account}`, so without this column two
// accounts' payments for one coin were indistinguishable once written (`hash` is the venue
// TRANSACTION hash and names no account). It is added with NO schema-version metadata deliberately:
// that record MEASURED zero partitions of this kind (under its old name `funding`) in every store root, so there are no v0 parts
// to read tolerantly. If that measurement is ever falsified, this becomes a schema revision and
// needs the `meta = (…)` treatment `ChainCodec` carries.

series_codec! {
    Codec = FundingCodec,
    Row = FundingRow,
    rows = rows,
    schema_fn = funding_schema,
    columns_fn = funding_columns,
    decode_fn = funding_from_batch,
    fields {
        ts: i64 = |&i| rows[i].ts,
        account: str = |&i| rows[i].account.as_str(),
        usdc: f64 = |&i| rows[i].usdc,
        szi: f64 = |&i| rows[i].szi,
        funding_rate: f64 = |&i| rows[i].funding_rate,
        hash: str = |&i| rows[i].hash.as_str(),
    },
    decode_row = |i| FundingRow {
        ts: ts.value(i),
        account: std::mem::take(&mut account[i]),
        usdc: usdc.value(i),
        szi: szi.value(i),
        funding_rate: funding_rate.value(i),
        hash: std::mem::take(&mut hash[i]),
    },
    sort_key = |row| (row.ts, 0),
}
