//! Research codecs: option-chain snapshots, cohort open interest and perp market-context metrics.

use std::sync::Arc;

use datafusion::arrow::array::{Array, ArrayRef};
use datafusion::arrow::datatypes::{DataType, Field, Schema};
use datafusion::arrow::record_batch::RecordBatch;

use crate::chain_log::ChainRow;
use crate::cohort_log::CohortRow;
use crate::perp_metrics_log::PerpMetricRow;
use crate::store::hist::DataError;

use super::{
    SeriesCodec, bool_col, f64_col, gather_bool, gather_f64, gather_i64, gather_opt_f64,
    gather_str, i64_col, opt_f64_col, str_values_required,
};

// ---- option-chain snapshots (kind=chain; schema v1) -------------------------------------------
// The PIT options-surface series: `HistStore::append_chain_snapshot`/`scan_chain`/`chain_as_of`
// are the callers; the opt-in `ChainRecorder` (`VIKE_RECORD_CHAINS=1`) is the producer. Row type
// lives in `crate::chain_log` (always compiled, model-only); the codec sits here because
// `SeriesCodec` is module-private. Identity `(venue, symbol=<underlying>)` is the partition path,
// but `underlying` is ALSO a stored column (like exec_fill's venue/symbol), so `decode` ignores
// `ctx` and the `compact_roundtrip` re-encode (which passes `ctx=""`) preserves every column
// verbatim. Quote/greek fields are `f64_null` (nullable, always PRESENT in v1 parts — absent
// stays SQL NULL, never 0.0/NaN); a future additive column would use the `_add` flavors under the
// same schema-tolerant read discipline as the book kind (the `vike.schema.chain` metadata is the
// escape hatch for a breaking revision).

const CHAIN_SCHEMA_VERSION: &str = "1";

series_codec! {
    Codec = ChainCodec,
    Row = ChainRow,
    rows = rows,
    schema_fn = chain_schema,
    columns_fn = chain_columns,
    decode_fn = chain_from_batch,
    meta = ("vike.schema.chain", CHAIN_SCHEMA_VERSION),
    fields {
        ts: i64 = |&i| rows[i].ts,
        underlying: str = |&i| rows[i].underlying.as_str(),
        instrument: str = |&i| rows[i].instrument.as_str(),
        expiry_ms: i64 = |&i| rows[i].expiry_ms,
        strike: f64 = |&i| rows[i].strike,
        is_call: bool = |&i| rows[i].is_call,
        bid: f64_null = |&i| rows[i].bid,
        ask: f64_null = |&i| rows[i].ask,
        mark: f64_null = |&i| rows[i].mark,
        iv: f64_null = |&i| rows[i].iv,
        open_interest: f64_null = |&i| rows[i].open_interest,
        volume: f64_null = |&i| rows[i].volume,
        delta: f64_null = |&i| rows[i].delta,
        gamma: f64_null = |&i| rows[i].gamma,
        theta: f64_null = |&i| rows[i].theta,
        vega: f64_null = |&i| rows[i].vega,
    },
    decode_row = |i| ChainRow {
        ts: ts.value(i),
        underlying: std::mem::take(&mut underlying[i]),
        instrument: std::mem::take(&mut instrument[i]),
        expiry_ms: expiry_ms.value(i),
        strike: strike.value(i),
        is_call: is_call.value(i),
        bid: (!bid.is_null(i)).then(|| bid.value(i)),
        ask: (!ask.is_null(i)).then(|| ask.value(i)),
        mark: (!mark.is_null(i)).then(|| mark.value(i)),
        iv: (!iv.is_null(i)).then(|| iv.value(i)),
        open_interest: (!open_interest.is_null(i)).then(|| open_interest.value(i)),
        volume: (!volume.is_null(i)).then(|| volume.value(i)),
        delta: (!delta.is_null(i)).then(|| delta.value(i)),
        gamma: (!gamma.is_null(i)).then(|| gamma.value(i)),
        theta: (!theta.is_null(i)).then(|| theta.value(i)),
        vega: (!vega.is_null(i)).then(|| vega.value(i)),
    },
    sort_key = |row| (row.ts, 0),
}

// ---- cohort open interest (kind=cohort; schema v1) --------------------------------------------
// The graded positioning panel: `HistStore::append_cohort`/`scan_cohort` are the callers and
// `crate::rec::cohort_rec::CohortRecorder` is the producer. Row type lives in `crate::cohort_log`
// (always compiled, model-only); the codec sits here because `SeriesCodec` is module-private.
//
// A LONG row — one row per (hour, asset, axis, label) rather than a column per label. The argument
// is `crate::store::store_kind::STORE_KINDS`' `cohort` row (36 admitted labels × 2 wire numbers = 73
// columns at one grading, 129 across three, against a widest kind of 16), and the consequence for
// THIS file is that the field list below is stable against a taxonomy revision: a new rung writes
// rows, not columns.
//
// Identity `(venue, symbol=<asset>)` is the partition path, but `asset` is ALSO a stored column
// (like chain's `underlying` and exec_fill's venue/symbol), so `decode` ignores `ctx` and the
// `compact_roundtrip` re-encode (which passes `ctx=""`) preserves every column verbatim.
//
// Every column is `str`/`f64`/`i64` NON-NULL, deliberately, and that is a statement about the
// source rather than a default: the loader upstream REFUSES a row whose `total_position_value` or
// `_long` is absent rather than inventing a zero, so a NULL notional cannot reach this codec and a
// nullable column would document a state that does not exist. `grading`/`label_basis` are non-null
// for the stronger reason — a NULL there is precisely the aliasing this kind stores them to prevent.
// A future additive column would use the `_add` flavors under the same schema-tolerant read
// discipline as the book kind (the `vike.schema.cohort` metadata is the escape hatch for a breaking
// revision).

const COHORT_SCHEMA_VERSION: &str = "1";

series_codec! {
    Codec = CohortCodec,
    Row = CohortRow,
    rows = rows,
    schema_fn = cohort_schema,
    columns_fn = cohort_columns,
    decode_fn = cohort_from_batch,
    meta = ("vike.schema.cohort", COHORT_SCHEMA_VERSION),
    fields {
        ts: i64 = |&i| rows[i].ts,
        asset: str = |&i| rows[i].asset.as_str(),
        axis: str = |&i| rows[i].axis.as_str(),
        cohort: str = |&i| rows[i].cohort.as_str(),
        grading: str = |&i| rows[i].grading.as_str(),
        label_basis: str = |&i| rows[i].label_basis.as_str(),
        long_usd: f64 = |&i| rows[i].long_usd,
        total_usd: f64 = |&i| rows[i].total_usd,
    },
    decode_row = |i| CohortRow {
        ts: ts.value(i),
        asset: std::mem::take(&mut asset[i]),
        axis: std::mem::take(&mut axis[i]),
        cohort: std::mem::take(&mut cohort[i]),
        grading: std::mem::take(&mut grading[i]),
        label_basis: std::mem::take(&mut label_basis[i]),
        long_usd: long_usd.value(i),
        total_usd: total_usd.value(i),
    },
    sort_key = |row| (row.ts, 0),
}

// --- perp market-context metrics (kind=perp_metrics) --------------------------------------------
//
// The NARROWEST kind in the store: a timestamp and one venue-reported number. That is not an
// oversight — `crate::perp_metrics_log::PerpMetricRow`'s doc records that the funding RATE is
// deliberately absent (it already lives on `vike_model::Bar::funding` under `interval=funding`, and
// a second stored copy could disagree with the first) and that open interest is absent because
// Hyperliquid serves it only as a current snapshot, never as history.
//
// Identity `(venue, symbol)` is the partition path and is NOT re-stored as a column — the
// `crate::funding_log::FundingRow` precedent rather than the `cohort`/`chain` one, because this row
// carries no further dimension that would have to be stored anyway. `decode` therefore ignores
// `ctx`, and the `compact_roundtrip` re-encode (which passes `ctx=""`) preserves every column
// verbatim.
//
// `premium` is `f64` NON-NULL, and that is a statement about the producer: a row exists only for an
// interval whose premium the venue actually reported, so a NULL cannot reach this codec. A future
// additive column — `open_interest` is the one this kind was shaped to accept — uses the `_add`
// flavors, whose absent-column tolerance is what lets it join without breaking the parts written
// today; the `vike.schema.perp_metrics` metadata is the escape hatch for a breaking revision.

const PERP_METRICS_SCHEMA_VERSION: &str = "1";

series_codec! {
    Codec = PerpMetricsCodec,
    Row = PerpMetricRow,
    rows = rows,
    schema_fn = perp_metrics_schema,
    columns_fn = perp_metrics_columns,
    decode_fn = perp_metrics_from_batch,
    meta = ("vike.schema.perp_metrics", PERP_METRICS_SCHEMA_VERSION),
    fields {
        ts: i64 = |&i| rows[i].ts,
        premium: f64 = |&i| rows[i].premium,
        // additive: appended LAST + nullable so parts written before it existed still decode —
        // exec_fill's `mark_price` contract. The source is data.vike.io's hourly panel; the
        // funding-rate collector has no reading and writes None.
        open_interest: f64_add = |&i| rows[i].open_interest,
    },
    decode_row = |i| PerpMetricRow {
        ts: ts.value(i),
        premium: premium.value(i),
        // absent in pre-open_interest parts → None
        open_interest: open_interest.and_then(|c| (!c.is_null(i)).then(|| c.value(i))),
    },
    sort_key = |row| (row.ts, 0),
}
