//! Research codecs: option-chain snapshots, cohort open interest, perp market-context metrics, the
//! option trade log, the text history (news and calendar events as received), the text models' scores
//! of it and the two SEC tables (reported XBRL values and Form 13F lines).

use std::sync::Arc;

use datafusion::arrow::array::{Array, ArrayRef};
use datafusion::arrow::datatypes::{DataType, Field, Schema};
use datafusion::arrow::record_batch::RecordBatch;

use vike_data::chain_log::ChainRow;
use vike_data::cohort_log::CohortRow;
use vike_data::option_trade_log::OptionTradeRow;
use vike_data::perp_metrics_log::PerpMetricRow;
use vike_data::store::hist::HistoryStoreError;
use vike_data::text_log::TextRow;
use vike_data::{FinancialFactRow, InstitutionalHoldingRow, TextScoreRow};
use vike_marketdata::OptionKind;

use super::{
    SeriesCodec, bool_col, f64_col, gather_bool, gather_f64, gather_i64, gather_opt_f64,
    gather_str, i64_col, opt_f64_col, str_values_required,
};

// ---- option-chain snapshots (kind=chain; schema v1) -------------------------------------------
// The PIT options-surface series: `HistoryStore::append_chain_snapshot`/`scan_chain`/`chain_as_of`
// are the callers; the opt-in `ChainRecorder` (`flags.record_chains`) is the producer. Row type
// lives in `vike_data::chain_log` (always compiled, model-only); the codec sits here because
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
        is_call: bool = |&i| rows[i].kind == OptionKind::Call,
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
        kind: if is_call.value(i) { OptionKind::Call } else { OptionKind::Put },
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
// The graded positioning panel: `HistoryStore::append_cohort`/`scan_cohort` are the callers and
// `vike_data::rec::cohort_rec::CohortRecorder` is the producer. Row type lives in `vike_data::cohort_log`
// (always compiled, model-only); the codec sits here because `SeriesCodec` is module-private.
//
// A LONG row — one row per (hour, asset, axis, label) rather than a column per label. The argument
// is `vike_data::store::store_kind::STORE_KINDS`' `cohort` row (36 admitted labels × 2 wire numbers = 73
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
// oversight — `vike_data::perp_metrics_log::PerpMetricRow`'s doc records that the funding RATE is
// deliberately absent (it already lives on `vike_marketdata::Bar::funding` under `interval=funding`, and
// a second stored copy could disagree with the first) and that open interest is absent because
// Hyperliquid serves it only as a current snapshot, never as history.
//
// Identity `(venue, symbol)` is the partition path and is NOT re-stored as a column — the
// `vike_data::funding_log::ExecutionHistoryFundingRow` precedent rather than the `cohort`/`chain` one, because this row
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

// ---- text history (kind=text; schema v1) -------------------------------------------------------
// News items and calendar events as this box received them: `HistoryStore::append_texts`/`scan_texts`
// are the callers and the datahub's text recorder is the producer; the row is `vike_data::text_log`'s.
// The `ts` column IS `TextRow::received_ms`, the time a point-in-time read selects on. Identity
// `(venue=<text kind>, symbol=<collector>)` is the path, and the kind is ALSO a column, so `decode`
// ignores `ctx` and the `compact_roundtrip` re-encode (`ctx=""`) keeps every column. Every string is
// `str` NON-NULL: a field a text kind does not have is stored EMPTY, the feeds' own spelling of
// "none". A later column takes an `_add` flavor; `vike.schema.text` is the escape hatch for a breaking
// revision.

const TEXT_SCHEMA_VERSION: &str = "1";

series_codec! {
    Codec = TextCodec,
    Row = TextRow,
    rows = rows,
    schema_fn = text_schema,
    columns_fn = text_columns,
    decode_fn = text_from_batch,
    meta = ("vike.schema.text", TEXT_SCHEMA_VERSION),
    fields {
        ts: i64 = |&i| rows[i].received_ms,
        published_ms: i64 = |&i| rows[i].published_ms,
        kind: str = |&i| rows[i].kind.as_str(),
        source: str = |&i| rows[i].source.as_str(),
        source_id: str = |&i| rows[i].source_id.as_str(),
        subject: str = |&i| rows[i].subject.as_str(),
        period: str = |&i| rows[i].period.as_str(),
        title: str = |&i| rows[i].title.as_str(),
        summary: str = |&i| rows[i].summary.as_str(),
        body: str = |&i| rows[i].body.as_str(),
        url: str = |&i| rows[i].url.as_str(),
        market: str = |&i| rows[i].market.as_str(),
        tags: str = |&i| rows[i].tags.as_str(),
        currency: str = |&i| rows[i].currency.as_str(),
        impact: str = |&i| rows[i].impact.as_str(),
        forecast: str = |&i| rows[i].forecast.as_str(),
        previous: str = |&i| rows[i].previous.as_str(),
        actual: str = |&i| rows[i].actual.as_str(),
        amends: str = |&i| rows[i].amends.as_str(),
    },
    decode_row = |i| TextRow {
        received_ms: ts.value(i),
        published_ms: published_ms.value(i),
        kind: std::mem::take(&mut kind[i]),
        source: std::mem::take(&mut source[i]),
        source_id: std::mem::take(&mut source_id[i]),
        subject: std::mem::take(&mut subject[i]),
        period: std::mem::take(&mut period[i]),
        title: std::mem::take(&mut title[i]),
        summary: std::mem::take(&mut summary[i]),
        body: std::mem::take(&mut body[i]),
        url: std::mem::take(&mut url[i]),
        market: std::mem::take(&mut market[i]),
        tags: std::mem::take(&mut tags[i]),
        currency: std::mem::take(&mut currency[i]),
        impact: std::mem::take(&mut impact[i]),
        forecast: std::mem::take(&mut forecast[i]),
        previous: std::mem::take(&mut previous[i]),
        actual: std::mem::take(&mut actual[i]),
        amends: std::mem::take(&mut amends[i]),
    },
    sort_key = |row| (row.received_ms, 0),
}

// ---- option trade log (kind=option_trade; schema v1) ------------------------------------------
// An options venue's public trade log: `HistoryStore::append_option_trades`/`scan_option_trades` are the
// callers and `vike-backfill`'s option-trades lane is the producer. Row type lives in
// `vike_data::option_trade_log` (always compiled, model-only). Identity `(venue, symbol=<currency>)` is
// the partition path; `underlying` and `instrument` are stored columns (one currency's log holds several
// underlyings), so `decode` ignores `ctx` and the `compact_roundtrip` re-encode (`ctx=""`) preserves
// every column verbatim. `iv`/`mark_price` are `f64_null`: absent stays SQL NULL, never 0.0 (Deribit's
// 2017 trades publish `mark_price: null`). `sort_key` is `(ts, 0)`, stable, so the trades of one
// millisecond keep the venue's order.

const OPTION_TRADE_SCHEMA_VERSION: &str = "1";

series_codec! {
    Codec = OptionTradeCodec,
    Row = OptionTradeRow,
    rows = rows,
    schema_fn = option_trade_schema,
    columns_fn = option_trade_columns,
    decode_fn = option_trade_from_batch,
    meta = ("vike.schema.option_trade", OPTION_TRADE_SCHEMA_VERSION),
    fields {
        ts: i64 = |&i| rows[i].ts,
        underlying: str = |&i| rows[i].underlying.as_str(),
        instrument: str = |&i| rows[i].instrument.as_str(),
        expiry_ms: i64 = |&i| rows[i].expiry_ms,
        strike: f64 = |&i| rows[i].strike,
        is_call: bool = |&i| rows[i].kind == OptionKind::Call,
        trade_id: str = |&i| rows[i].trade_id.as_str(),
        price: f64 = |&i| rows[i].price,
        amount: f64 = |&i| rows[i].amount,
        is_buyer_maker: bool = |&i| rows[i].is_buyer_maker,
        iv: f64_null = |&i| rows[i].iv,
        mark_price: f64_null = |&i| rows[i].mark_price,
        index_price: f64 = |&i| rows[i].index_price,
        premium_in_coin: bool = |&i| rows[i].premium_in_coin,
    },
    decode_row = |i| OptionTradeRow {
        ts: ts.value(i),
        underlying: std::mem::take(&mut underlying[i]),
        instrument: std::mem::take(&mut instrument[i]),
        expiry_ms: expiry_ms.value(i),
        strike: strike.value(i),
        kind: if is_call.value(i) { OptionKind::Call } else { OptionKind::Put },
        trade_id: std::mem::take(&mut trade_id[i]),
        price: price.value(i),
        amount: amount.value(i),
        is_buyer_maker: is_buyer_maker.value(i),
        iv: (!iv.is_null(i)).then(|| iv.value(i)),
        mark_price: (!mark_price.is_null(i)).then(|| mark_price.value(i)),
        index_price: index_price.value(i),
        premium_in_coin: premium_in_coin.value(i),
    },
    sort_key = |row| (row.ts, 0),
}

// ---- SEC tables (kind=financial_fact, kind=institutional_holding; schema v1) ----------------------
// Reported XBRL values and Form 13F lines, written by the SEC import (`vike-cli data history import`);
// the rows are `vike_data::sec_log`'s. The `ts` column IS the row's `filed_ms`, the instant it became
// knowable, so a point-in-time read is a `ts` range. Identity `(venue=sec, symbol=<CIK>)` is the path,
// and the CIK is ALSO a column, so `decode` ignores `ctx` and the `compact_roundtrip` re-encode
// (`ctx=""`) keeps every column. Every string is `str` NON-NULL, EMPTY where the source gives nothing.

const FINANCIAL_FACT_SCHEMA_VERSION: &str = "1";

series_codec! {
    Codec = FinancialFactCodec,
    Row = FinancialFactRow,
    rows = rows,
    schema_fn = financial_fact_schema,
    columns_fn = financial_fact_columns,
    decode_fn = financial_fact_from_batch,
    meta = ("vike.schema.financial_fact", FINANCIAL_FACT_SCHEMA_VERSION),
    fields {
        ts: i64 = |&i| rows[i].filed_ms,
        cik: str = |&i| rows[i].cik.as_str(),
        entity: str = |&i| rows[i].entity.as_str(),
        taxonomy: str = |&i| rows[i].taxonomy.as_str(),
        concept: str = |&i| rows[i].concept.as_str(),
        unit: str = |&i| rows[i].unit.as_str(),
        period_start: str = |&i| rows[i].period_start.as_str(),
        period_end: str = |&i| rows[i].period_end.as_str(),
        value: f64 = |&i| rows[i].value,
        accession: str = |&i| rows[i].accession.as_str(),
        fiscal_year: str = |&i| rows[i].fiscal_year.as_str(),
        fiscal_period: str = |&i| rows[i].fiscal_period.as_str(),
        form: str = |&i| rows[i].form.as_str(),
        frame: str = |&i| rows[i].frame.as_str(),
    },
    decode_row = |i| FinancialFactRow {
        filed_ms: ts.value(i),
        cik: std::mem::take(&mut cik[i]),
        entity: std::mem::take(&mut entity[i]),
        taxonomy: std::mem::take(&mut taxonomy[i]),
        concept: std::mem::take(&mut concept[i]),
        unit: std::mem::take(&mut unit[i]),
        period_start: std::mem::take(&mut period_start[i]),
        period_end: std::mem::take(&mut period_end[i]),
        value: value.value(i),
        accession: std::mem::take(&mut accession[i]),
        fiscal_year: std::mem::take(&mut fiscal_year[i]),
        fiscal_period: std::mem::take(&mut fiscal_period[i]),
        form: std::mem::take(&mut form[i]),
        frame: std::mem::take(&mut frame[i]),
    },
    sort_key = |row| (row.filed_ms, 0),
}

const INSTITUTIONAL_HOLDING_SCHEMA_VERSION: &str = "1";

series_codec! {
    Codec = InstitutionalHoldingCodec,
    Row = InstitutionalHoldingRow,
    rows = rows,
    schema_fn = institutional_holding_schema,
    columns_fn = institutional_holding_columns,
    decode_fn = institutional_holding_from_batch,
    meta = ("vike.schema.institutional_holding", INSTITUTIONAL_HOLDING_SCHEMA_VERSION),
    fields {
        ts: i64 = |&i| rows[i].filed_ms,
        accession: str = |&i| rows[i].accession.as_str(),
        filer_cik: str = |&i| rows[i].filer_cik.as_str(),
        filer_name: str = |&i| rows[i].filer_name.as_str(),
        period: str = |&i| rows[i].period.as_str(),
        submission_type: str = |&i| rows[i].submission_type.as_str(),
        amendment_type: str = |&i| rows[i].amendment_type.as_str(),
        issuer: str = |&i| rows[i].issuer.as_str(),
        class_title: str = |&i| rows[i].class_title.as_str(),
        cusip: str = |&i| rows[i].cusip.as_str(),
        value_usd: i64 = |&i| rows[i].value_usd,
        shares: i64 = |&i| rows[i].shares,
        shares_type: str = |&i| rows[i].shares_type.as_str(),
        put_call: str = |&i| rows[i].put_call.as_str(),
    },
    decode_row = |i| InstitutionalHoldingRow {
        filed_ms: ts.value(i),
        accession: std::mem::take(&mut accession[i]),
        filer_cik: std::mem::take(&mut filer_cik[i]),
        filer_name: std::mem::take(&mut filer_name[i]),
        period: std::mem::take(&mut period[i]),
        submission_type: std::mem::take(&mut submission_type[i]),
        amendment_type: std::mem::take(&mut amendment_type[i]),
        issuer: std::mem::take(&mut issuer[i]),
        class_title: std::mem::take(&mut class_title[i]),
        cusip: std::mem::take(&mut cusip[i]),
        value_usd: value_usd.value(i),
        shares: shares.value(i),
        shares_type: std::mem::take(&mut shares_type[i]),
        put_call: std::mem::take(&mut put_call[i]),
    },
    sort_key = |row| (row.filed_ms, 0),
}

// ---- text scores (kind=text_score; schema v1) --------------------------------------------------
// A host-side model's numbers for the stored texts: `HistoryStore::append_text_scores`/
// `scan_text_scores` are the callers; the row is `vike_data::text_score_log`'s. The `ts` column IS the
// SCORED TEXT's `received_ms`, so a point-in-time read selects on the clock `kind=text` does. Identity
// `(venue=<text kind>, symbol=<model name>)` is the path and every field is a stored column, so
// `decode` ignores `ctx` and the `compact_roundtrip` re-encode (`ctx=""`) keeps every column. `values`
// is the model's numbers as ONE string (the row type's `TEXT_SCORE_SEPARATOR`): the macro has no list
// column, and a vector's length is the model's.

const TEXT_SCORE_SCHEMA_VERSION: &str = "1";

series_codec! {
    Codec = TextScoreCodec,
    Row = TextScoreRow,
    rows = rows,
    schema_fn = text_score_schema,
    columns_fn = text_score_columns,
    decode_fn = text_score_from_batch,
    meta = ("vike.schema.text_score", TEXT_SCORE_SCHEMA_VERSION),
    fields {
        ts: i64 = |&i| rows[i].received_ms,
        text_id: str = |&i| rows[i].text_id.as_str(),
        chunk: i64 = |&i| i64::from(rows[i].chunk),
        scored_ms: i64 = |&i| rows[i].scored_ms,
        model: str = |&i| rows[i].model.as_str(),
        values: str = |&i| rows[i].values.as_str(),
    },
    decode_row = |i| TextScoreRow {
        received_ms: ts.value(i),
        text_id: std::mem::take(&mut text_id[i]),
        chunk: chunk.value(i) as u32,
        scored_ms: scored_ms.value(i),
        model: std::mem::take(&mut model[i]),
        values: std::mem::take(&mut values[i]),
    },
    sort_key = |row| (row.received_ms, 0),
}
