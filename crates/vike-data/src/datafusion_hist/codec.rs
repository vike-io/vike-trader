//! Arrow `RecordBatch` <-> domain-type codecs for the series kinds this store holds (bars, quotes,
//! trades, L2 book events, point-in-time symbol properties, and equity-curve samples): schema
//! definitions, column builders (encode), and row decoders (decode).
//!
//! ONE decode path per row type, shared by the `Vec` scans AND the streaming twins, so a streamed
//! row is bit-identical to the same row from `scan_*` (same `col.value(i)` reads, same field order).
//! The `Vec` scans `extend` with these then sort; the streams yield one decoded batch's rows at a time.
//!
//! Each kind declares its columns ONCE, via the [`series_codec`] field-spec macro, which generates
//! the schema, the column builders and the decoder from that one list.
//!
//! `quotes`/`trades` carry a NULLABLE `local_ts` column appended LAST (book-recording plan Task 5),
//! read through [`opt_i64_col`] so parts written before it existed decode with `local_ts = 0`. The
//! `book` kind explodes each [`BookUpdate`] to one row per level (schema v1); see that section.
//! `exec_fill` similarly carries a NULLABLE `mark_price` column appended LAST, read through
//! [`opt_f64_col`] so parts written before it existed decode with `mark_price = None`, plus two more
//! ADDITIVE string columns appended after it (`liquidity_side`, `commission_asset`), read through
//! [`opt_str_col`] so parts written before THEY existed decode with `""` (venue-not-surfaced) rather
//! than erroring.

use std::sync::Arc;

use datafusion::arrow::array::{
    Array, ArrayRef, BooleanArray, Float64Array, Int64Array, Int8Array, StringArray,
    StringViewArray,
};
use datafusion::arrow::datatypes::{DataType, Field, Schema};
use datafusion::arrow::record_batch::RecordBatch;

use vike_model::{
    Bar, BookUpdate, BookUpdateKind, EquitySample, QuoteTick, SymbolProperties, TickScheme,
    TradeTick,
};

use crate::chain_log::ChainRow;
use crate::cohort_log::CohortRow;
use crate::exec_log::{ExecFillRow, ExecOrderRow};
use crate::funding_log::FundingRow;
use crate::hist::DataError;
use crate::perp_metrics_log::PerpMetricRow;

use super::q;

// ---- RecordBatch column readers ------------------------------------------------------------

pub(super) fn f64_col<'a>(b: &'a RecordBatch, name: &str) -> Result<&'a Float64Array, DataError> {
    b.column_by_name(name)
        .and_then(|c| c.as_any().downcast_ref::<Float64Array>())
        .ok_or_else(|| q(format!("missing/!f64 column {name}")))
}
pub(super) fn i64_col<'a>(b: &'a RecordBatch, name: &str) -> Result<&'a Int64Array, DataError> {
    b.column_by_name(name)
        .and_then(|c| c.as_any().downcast_ref::<Int64Array>())
        .ok_or_else(|| q(format!("missing/!i64 column {name}")))
}
/// The [`i64_col`] twin for the `book` kind's Int8 `kind`-code column — hoisted up here beside its
/// siblings (it was a decoder-local closure) so every typed downcast reader lives at ONE site.
pub(super) fn i8_col<'a>(b: &'a RecordBatch, name: &str) -> Result<&'a Int8Array, DataError> {
    b.column_by_name(name)
        .and_then(|c| c.as_any().downcast_ref::<Int8Array>())
        .ok_or_else(|| q(format!("missing/!i8 column {name}")))
}
/// Optional Int64 column reader — the decode-time schema-tolerance primitive (book-recording plan
/// Task 4). Returns `Ok(None)` when the column is ABSENT (a part written under an older schema
/// revision that predates a later-added nullable column), and `Err` ONLY when it is present but not
/// Int64. A decoder reads a nullable additive column through this so old parts still decode.
///
/// Threaded by the quote/trade decoders for the additive `local_ts` column (Task 5): a part written
/// before that column existed decodes with `local_ts = 0` instead of erroring.
pub(super) fn opt_i64_col<'a>(
    b: &'a RecordBatch,
    name: &str,
) -> Result<Option<&'a Int64Array>, DataError> {
    match b.column_by_name(name) {
        None => Ok(None), // column absent → old part; NOT an error
        Some(c) => c
            .as_any()
            .downcast_ref::<Int64Array>()
            .map(Some)
            .ok_or_else(|| q(format!("column {name} present but !i64"))),
    }
}
/// Optional Float64 column reader — the [`opt_i64_col`] twin for additive nullable f64 columns.
/// Returns `Ok(None)` when the column is ABSENT (a part written before it existed), and `Err` ONLY
/// when it is present but not Float64. Threaded by the exec-fill decoder for the additive
/// `mark_price` column: a part written before that column existed decodes with `mark_price = None`
/// instead of erroring, exactly like [`opt_i64_col`]'s `local_ts` contract.
pub(super) fn opt_f64_col<'a>(
    b: &'a RecordBatch,
    name: &str,
) -> Result<Option<&'a Float64Array>, DataError> {
    match b.column_by_name(name) {
        None => Ok(None), // column absent → old part; NOT an error
        Some(c) => c
            .as_any()
            .downcast_ref::<Float64Array>()
            .map(Some)
            .ok_or_else(|| q(format!("column {name} present but !f64"))),
    }
}
/// Optional (ABSENT-tolerant) Utf8 column reader — the [`opt_f64_col`]/[`opt_i64_col`] twin for
/// additive STRING columns. Unlike `mark_price`'s `Option<f64>` domain type, these additive string
/// columns (`liquidity_side`/`commission_asset`) are always written as concrete (possibly empty)
/// strings — never a SQL NULL — so tolerance is purely about the COLUMN being absent, not individual
/// nulls: `Ok(None)` when the column is ABSENT (a part written before it existed), `Err` only when
/// present but not Utf8/Utf8View. Threaded by the exec-fill decoder for `liquidity_side`/
/// `commission_asset`: a part written before these existed decodes with `""` per row instead of
/// erroring, exactly like `opt_f64_col`'s `mark_price = None` contract.
pub(super) fn opt_str_col(b: &RecordBatch, name: &str) -> Result<Option<Vec<String>>, DataError> {
    if b.column_by_name(name).is_none() {
        return Ok(None); // column absent → old part; NOT an error
    }
    Ok(Some(str_values_required(b, name)?))
}
/// Optional (ABSENT-tolerant) Utf8 reader that PRESERVES per-cell NULLs — the [`str_values`] twin of
/// [`opt_str_col`]. Where `opt_str_col` flattens a NULL cell to `""` (right for the always-concrete
/// `liquidity_side`/`commission_asset`), this keeps the `Option` PER ROW, for an additive nullable
/// string column where a NULL cell is a MEANINGFUL absent value distinct from a present empty string:
/// properties' `tick_scheme`, where a NULL cell means "no scheme" and a string is a JSON-encoded
/// scheme. Returns `Ok(None)` when the COLUMN is absent (a part written before it existed → every row
/// decodes to the absent default), `Err` only when present but not Utf8/Utf8View. The `str_add_null`
/// flavor reads through this — the string twin of `i64_add_null`'s [`opt_i64_col`].
pub(super) fn opt_str_nullable_col(
    b: &RecordBatch,
    name: &str,
) -> Result<Option<Vec<Option<String>>>, DataError> {
    if b.column_by_name(name).is_none() {
        return Ok(None); // column absent → old part; NOT an error
    }
    Ok(Some(str_values(b, name)?))
}
pub(super) fn bool_col<'a>(b: &'a RecordBatch, name: &str) -> Result<&'a BooleanArray, DataError> {
    b.column_by_name(name)
        .and_then(|c| c.as_any().downcast_ref::<BooleanArray>())
        .ok_or_else(|| q(format!("missing/!bool column {name}")))
}
/// Materialize a Utf8 column to `Vec<Option<String>>`, tolerant of BOTH the `Utf8`
/// ([`StringArray`], what we WRITE) and `Utf8View` ([`StringViewArray`]) Arrow layouts. DataFusion's
/// Parquet reader defaults to `schema_force_view_types = true`, so a string column WRITTEN as `Utf8`
/// comes BACK as a `Utf8View` array — a plain `StringArray` downcast would spuriously fail. Reading
/// through this decouples the decode from that read-time representation choice. `None` element = SQL
/// NULL (a nullable column's absent value).
pub(super) fn str_values(b: &RecordBatch, name: &str) -> Result<Vec<Option<String>>, DataError> {
    let col = b.column_by_name(name).ok_or_else(|| q(format!("missing column {name}")))?;
    let read = |is_null: &dyn Fn(usize) -> bool, val: &dyn Fn(usize) -> String, len: usize| {
        (0..len).map(|i| (!is_null(i)).then(|| val(i))).collect::<Vec<_>>()
    };
    if let Some(a) = col.as_any().downcast_ref::<StringArray>() {
        Ok(read(&|i| a.is_null(i), &|i| a.value(i).to_string(), a.len()))
    } else if let Some(a) = col.as_any().downcast_ref::<StringViewArray>() {
        Ok(read(&|i| a.is_null(i), &|i| a.value(i).to_string(), a.len()))
    } else {
        Err(q(format!("column {name} present but not a Utf8/Utf8View string")))
    }
}
/// A non-nullable Utf8 column as owned `String`s (unwraps the NULL slot to `""` — the schema marks
/// these columns non-null, so a NULL should never occur; the default is defensive, never load-bearing).
pub(super) fn str_values_required(b: &RecordBatch, name: &str) -> Result<Vec<String>, DataError> {
    Ok(str_values(b, name)?.into_iter().map(Option::unwrap_or_default).collect())
}

// ---- gather: the write-side column builders ---------------------------------------------------
//
// Each collapses one `Arc::new(XxxArray::from(idxs.iter().map(get).collect::<Vec<_>>()))` gather —
// the expression that otherwise repeated ~60x across the per-kind column builders — into one typed
// call. `get` maps a SELECTED row index (`idxs` is the caller's row selection, so the array holds
// exactly those rows in that order) to that column's value; the helper name IS the Arrow array type.
// Byte-identical to the inline form: same array type, same values, same order.

pub(super) fn gather_i64(idxs: &[usize], get: impl Fn(&usize) -> i64) -> ArrayRef {
    Arc::new(Int64Array::from(idxs.iter().map(get).collect::<Vec<i64>>()))
}
pub(super) fn gather_i8(idxs: &[usize], get: impl Fn(&usize) -> i8) -> ArrayRef {
    Arc::new(Int8Array::from(idxs.iter().map(get).collect::<Vec<i8>>()))
}
pub(super) fn gather_f64(idxs: &[usize], get: impl Fn(&usize) -> f64) -> ArrayRef {
    Arc::new(Float64Array::from(idxs.iter().map(get).collect::<Vec<f64>>()))
}
/// The nullable-f64 gather: a `None` becomes a SQL NULL slot. Used for BOTH stored-nullable columns
/// (bar `funding`, exec_order `price`/`trigger_price`) and the additive one (exec_fill `mark_price`)
/// — the WRITE side is identical for the two; only the READ side differs (see the `f64_null` vs
/// `f64_add` flavors in [`series_codec`]).
pub(super) fn gather_opt_f64(idxs: &[usize], get: impl Fn(&usize) -> Option<f64>) -> ArrayRef {
    Arc::new(Float64Array::from(idxs.iter().map(get).collect::<Vec<Option<f64>>>()))
}
/// The nullable-i64 gather — the integer twin of [`gather_opt_f64`]: a `None` becomes a SQL NULL
/// slot. Used by the `i64_add_null` flavor (properties' `taker_hold_ms`), where an ABSENT value
/// must stay distinguishable in the tape from an explicit `0` (`i64_add`, in contrast, always
/// writes a value and only tolerates the column being absent from OLD parts).
pub(super) fn gather_opt_i64(idxs: &[usize], get: impl Fn(&usize) -> Option<i64>) -> ArrayRef {
    Arc::new(Int64Array::from(idxs.iter().map(get).collect::<Vec<Option<i64>>>()))
}
pub(super) fn gather_bool(idxs: &[usize], get: impl Fn(&usize) -> bool) -> ArrayRef {
    Arc::new(BooleanArray::from(idxs.iter().map(get).collect::<Vec<bool>>()))
}
pub(super) fn gather_str<'a>(idxs: &[usize], get: impl Fn(&usize) -> &'a str) -> ArrayRef {
    Arc::new(StringArray::from(idxs.iter().map(get).collect::<Vec<&'a str>>()))
}
pub(super) fn gather_opt_str<'a>(
    idxs: &[usize],
    get: impl Fn(&usize) -> Option<&'a str>,
) -> ArrayRef {
    Arc::new(StringArray::from(idxs.iter().map(get).collect::<Vec<Option<&'a str>>>()))
}
/// The owned-`String` twin of [`gather_opt_str`]: a `None` becomes a SQL NULL slot. Used where the
/// cell is COMPUTED per row (properties' `tick_scheme`, a JSON encoding built on the fly) and so
/// cannot be returned as a borrow the way `gather_opt_str`'s `&str` accessors are. Materializes the
/// owned strings, then feeds the SAME `StringArray::from(Vec<Option<&str>>)` builder the borrowed
/// twin uses — byte-identical array, only the input ownership differs.
pub(super) fn gather_opt_string(
    idxs: &[usize],
    get: impl Fn(&usize) -> Option<String>,
) -> ArrayRef {
    let owned: Vec<Option<String>> = idxs.iter().map(get).collect();
    let borrowed: Vec<Option<&str>> = owned.iter().map(Option::as_deref).collect();
    Arc::new(StringArray::from(borrowed))
}

// ---- SeriesCodec: the schema/columns/decode/sort-key contract, one impl per series kind --------
//
// `decode`'s `ctx: &str` is the caller-supplied re-injection argument (`symbol` for quotes/trades;
// the scan's `symbol` argument standing in for `venue` on equity — see `batch_to_equities`); kinds
// that don't need it (bar / properties / exec / funding / the book per-level row) simply ignore it.
//
// `sort_key` returns `(ts, tiebreak)`: `tiebreak` is always `0` except the book row codec (`seq`, so
// multi-level rows of one event stay adjacent after a merge-sort — mirrors the original
// `|r| (r.ts, r.seq)`). `[T]::sort_by_key` is a STABLE sort, so a constant `0` tiebreak produces
// EXACTLY the order sorting by `ts` alone would (the comparator never observes the constant) — not a
// behavior change versus the other kinds' original `|r| r.ts` / `|r| r.0` key functions.
//
// The control flow AROUND this trait is what it exists to dedup: `append_*`/`scan_*`
// (`super::DataFusionHist::append_series`/`scan_series`) and `compact_roundtrip`'s
// decode→sort→re-encode rewrite (`compact_roundtrip_generic`) are each written ONCE, generic over
// `C: SeriesCodec`, instead of once per kind.
pub(super) trait SeriesCodec {
    /// The decoded domain row type this kind scans/appends.
    type Row;

    fn schema() -> Arc<Schema>;
    fn columns(rows: &[Self::Row], idxs: &[usize]) -> Vec<ArrayRef>;
    fn decode(b: &RecordBatch, ctx: &str) -> Result<Vec<Self::Row>, DataError>;
    fn sort_key(row: &Self::Row) -> (i64, i64);
}

// ---- series_codec!: each kind declares its columns ONCE ---------------------------------------
//
// A kind's schema, column builders and decoder are three views of ONE field list. Hand-writing that
// trio enumerated the list THREE times per kind, and the three had to stay in sync — the proven
// drift surface: the additive `local_ts` and `mark_price` columns each needed coordinated edits at
// all three sites. This macro takes the list ONCE and generates all three (plus the `SeriesCodec`
// adapter), so adding a column is a one-line edit to `fields` + its line in `decode_row`.
//
// Per field: `name: flavor = |&i| rows[i].accessor`. The NAME is the Arrow column name
// (`stringify!`d) AND the decode binding that `decode_row` reads. The FLAVOR picks all three
// representations at once — schema type + nullability, `gather_*` builder, and `*_col` reader:
//
//   flavor     schema              write            read binding          (drift-relevant note)
//   i64        Int64,   non-null   gather_i64       i64_col
//   i8         Int8,    non-null   gather_i8        i8_col
//   f64        Float64, non-null   gather_f64       f64_col
//   bool       Boolean, non-null   gather_bool      bool_col
//   f64_null   Float64, NULLABLE   gather_opt_f64   f64_col              column always PRESENT
//   f64_add    Float64, NULLABLE   gather_opt_f64   opt_f64_col          ADDITIVE: absent in old parts
//   i64_add    Int64,   NULLABLE   gather_i64       opt_i64_col          ADDITIVE: written non-null
//   i64_add_null Int64, NULLABLE   gather_opt_i64   opt_i64_col          ADDITIVE: absent in old parts
//   str        Utf8,    non-null   gather_str       str_values_required
//   str_null   Utf8,    NULLABLE   gather_opt_str   str_values
//   str_add    Utf8,    NULLABLE   gather_str       opt_str_col          ADDITIVE: written non-null
//   str_add_null Utf8,  NULLABLE   gather_opt_string opt_str_nullable_col ADDITIVE: absent in old parts, NULL cells kept
//
// The `_add` flavors are the schema-TOLERANCE ones: they read through `opt_*_col`, so a part written
// before the column existed still decodes (that kind's `decode_row` supplies the default —
// `local_ts = 0` / `mark_price = None` / `liquidity_side = ""`). Picking `f64_null`/`str_null` where
// `f64_add`/`str_add` is meant would break old parts: that distinction is the one judgement this
// macro asks of a caller.
//
// Hand-written by design: `decode_row` (the row constructor — tuple vs struct, `ctx` re-injection,
// i32/u32 casts, and non-column fields like bar's `bid`/`ask`/`symbol` are all genuinely per-kind),
// and `book`'s explode (`book_rows`) / regroup (`book_updates_from_rows`) — only book's plain
// per-level-row trio is generated here.
//
// Byte-identity: schema AND columns come from the same list in the same order, so field order/type/
// nullability and the built arrays are what the hand-written trio produced; the generated reads are
// the same `col.value(i)` calls the hand-written decoders made. No f64 ordering is touched.
macro_rules! series_codec {
    // -- flavor → Arrow schema field --
    (@field $col:ident : i64) => { Field::new(stringify!($col), DataType::Int64, false) };
    (@field $col:ident : i8) => { Field::new(stringify!($col), DataType::Int8, false) };
    (@field $col:ident : f64) => { Field::new(stringify!($col), DataType::Float64, false) };
    (@field $col:ident : bool) => { Field::new(stringify!($col), DataType::Boolean, false) };
    (@field $col:ident : f64_null) => { Field::new(stringify!($col), DataType::Float64, true) };
    (@field $col:ident : f64_add) => { Field::new(stringify!($col), DataType::Float64, true) };
    (@field $col:ident : i64_add) => { Field::new(stringify!($col), DataType::Int64, true) };
    (@field $col:ident : i64_add_null) => { Field::new(stringify!($col), DataType::Int64, true) };
    (@field $col:ident : str) => { Field::new(stringify!($col), DataType::Utf8, false) };
    (@field $col:ident : str_null) => { Field::new(stringify!($col), DataType::Utf8, true) };
    (@field $col:ident : str_add) => { Field::new(stringify!($col), DataType::Utf8, true) };
    (@field $col:ident : str_add_null) => { Field::new(stringify!($col), DataType::Utf8, true) };

    // -- flavor → write-side column builder --
    (@col $idxs:ident : i64 = $get:expr) => { gather_i64($idxs, $get) };
    (@col $idxs:ident : i8 = $get:expr) => { gather_i8($idxs, $get) };
    (@col $idxs:ident : f64 = $get:expr) => { gather_f64($idxs, $get) };
    (@col $idxs:ident : bool = $get:expr) => { gather_bool($idxs, $get) };
    (@col $idxs:ident : str_add = $get:expr) => { gather_str($idxs, $get) };
    (@col $idxs:ident : f64_null = $get:expr) => { gather_opt_f64($idxs, $get) };
    (@col $idxs:ident : f64_add = $get:expr) => { gather_opt_f64($idxs, $get) };
    (@col $idxs:ident : i64_add = $get:expr) => { gather_i64($idxs, $get) };
    (@col $idxs:ident : i64_add_null = $get:expr) => { gather_opt_i64($idxs, $get) };
    (@col $idxs:ident : str = $get:expr) => { gather_str($idxs, $get) };
    (@col $idxs:ident : str_null = $get:expr) => { gather_opt_str($idxs, $get) };
    (@col $idxs:ident : str_add_null = $get:expr) => { gather_opt_string($idxs, $get) };

    // -- flavor → decode column binding (consumed by `decode_row` under this same name) --
    (@read $b:ident $col:ident : i64) => { let $col = i64_col($b, stringify!($col))?; };
    (@read $b:ident $col:ident : i8) => { let $col = i8_col($b, stringify!($col))?; };
    (@read $b:ident $col:ident : f64) => { let $col = f64_col($b, stringify!($col))?; };
    (@read $b:ident $col:ident : bool) => { let $col = bool_col($b, stringify!($col))?; };
    (@read $b:ident $col:ident : f64_null) => { let $col = f64_col($b, stringify!($col))?; };
    (@read $b:ident $col:ident : f64_add) => { let $col = opt_f64_col($b, stringify!($col))?; };
    (@read $b:ident $col:ident : i64_add) => { let $col = opt_i64_col($b, stringify!($col))?; };
    (@read $b:ident $col:ident : i64_add_null) => { let $col = opt_i64_col($b, stringify!($col))?; };
    (@read $b:ident $col:ident : str) => {
        let mut $col = str_values_required($b, stringify!($col))?;
    };
    (@read $b:ident $col:ident : str_null) => { let mut $col = str_values($b, stringify!($col))?; };
    (@read $b:ident $col:ident : str_add) => {
        let mut $col = opt_str_col($b, stringify!($col))?;
    };
    (@read $b:ident $col:ident : str_add_null) => {
        let $col = opt_str_nullable_col($b, stringify!($col))?;
    };

    // -- schema assembly, with or without the kind's schema-version metadata --
    (@schema $fields:ident) => { Arc::new(Schema::new($fields)) };
    (@schema $fields:ident meta ($mkey:literal, $mver:expr)) => {{
        let meta = [($mkey.to_string(), ($mver).to_string())].into_iter().collect();
        Arc::new(Schema::new_with_metadata($fields, meta))
    }};

    (
        Codec = $codec:ident,
        Row = $row:ty,
        rows = $rows:ident,
        $(ctx = $ctx:ident,)?
        schema_fn = $schema_fn:ident,
        columns_fn = $columns_fn:ident,
        decode_fn = $decode_fn:ident,
        $(meta = ($mkey:literal, $mver:expr),)?
        fields { $($col:ident : $flavor:tt = $get:expr),+ $(,)? },
        decode_row = |$i:ident| $rowexpr:expr,
        sort_key = |$skrow:ident| $sk:expr $(,)?
    ) => {
        pub(super) fn $schema_fn() -> Arc<Schema> {
            let fields = vec![$(series_codec!(@field $col : $flavor)),+];
            series_codec!(@schema fields $(meta ($mkey, $mver))?)
        }

        pub(super) fn $columns_fn($rows: &[$row], idxs: &[usize]) -> Vec<ArrayRef> {
            vec![$(series_codec!(@col idxs : $flavor = $get)),+]
        }

        // `needless_range_loop`: the index drives several columns at once (and, for the string
        // flavors, a `mem::take` out of the materialized vec) — there is no single collection to
        // iterate instead. Allowed so every kind decodes through the SAME generated loop shape.
        #[allow(clippy::needless_range_loop)]
        pub(super) fn $decode_fn(b: &RecordBatch $(, $ctx: &str)?) -> Result<Vec<$row>, DataError> {
            $(series_codec!{@read b $col : $flavor})+
            let mut out = Vec::with_capacity(b.num_rows());
            for $i in 0..b.num_rows() {
                out.push($rowexpr);
            }
            Ok(out)
        }

        /// Type-level tag only — never instantiated (an uninhabited enum, not a unit struct, so it's
        /// exempt from the "never constructed" dead-code lint by construction: it literally cannot be).
        pub(super) enum $codec {}

        impl SeriesCodec for $codec {
            type Row = $row;
            fn schema() -> Arc<Schema> {
                $schema_fn()
            }
            fn columns(rows: &[$row], idxs: &[usize]) -> Vec<ArrayRef> {
                $columns_fn(rows, idxs)
            }
            fn decode(b: &RecordBatch, _ctx: &str) -> Result<Vec<$row>, DataError> {
                $(let $ctx: &str = _ctx;)?
                $decode_fn(b $(, $ctx)?)
            }
            fn sort_key($skrow: &$row) -> (i64, i64) {
                $sk
            }
        }
    };
}

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

// ---- symbol properties (PIT instrument grid) --------------------------------------------------
// `kind=properties` series (was kind=filters; renamed with SymbolFilters→SymbolProperties):
// `HistStore::append_symbol_properties`/`scan_symbol_properties` (PIT-properties plan Task 2) are the
// callers; the recorder that populates it lands in a later task.

const PROPERTIES_SCHEMA_VERSION: &str = "1";

/// Decode one properties row's `tick_scheme` cell. `None` (an absent COLUMN — an old part — OR a
/// NULL cell) → no scheme; a present JSON string → the scheme, re-validated through `TickScheme`'s
/// own `Deserialize`, so a malformed row is an ERROR rather than a silently dropped or zero-tick
/// grid. The JSON is the venue-shaped `{base_tick, tiers:[…]}` the write side emits. Sole caller is
/// the generated `properties_from_batch`; it lives here (not inline in the macro) because the parse
/// can fail and the macro's `decode_row` is a single expression.
fn decode_tick_scheme(cell: Option<&str>) -> Result<Option<TickScheme>, DataError> {
    match cell {
        None => Ok(None),
        Some(s) => serde_json::from_str::<TickScheme>(s)
            .map(Some)
            .map_err(|e| q(format!("bad tick_scheme JSON in a properties row: {e}"))),
    }
}

series_codec! {
    Codec = PropertiesCodec,
    Row = (i64, SymbolProperties),
    rows = rows,
    schema_fn = properties_schema,
    columns_fn = properties_columns,
    decode_fn = properties_from_batch,
    // (was vike.schema.filters; renamed with SymbolFilters→SymbolProperties)
    meta = ("vike.schema.properties", PROPERTIES_SCHEMA_VERSION),
    fields {
        ts: i64 = |&i| rows[i].0,
        tick_size: f64 = |&i| rows[i].1.tick_size,
        step_size: f64 = |&i| rows[i].1.step_size,
        min_qty: f64 = |&i| rows[i].1.min_qty,
        max_qty: f64 = |&i| rows[i].1.max_qty,
        min_notional: f64 = |&i| rows[i].1.min_notional,
        // additive, appended LAST + nullable so parts written before this column existed still
        // decode (same contract as exec_fill's `mark_price`). The contract size / notional
        // multiplier: only Deribit options+futures populate it today; every other venue leaves it
        // absent. An absent (0.0) grid is written as NULL rather than 0.0 so "the venue never told
        // us" stays distinguishable in the tape from a venue that explicitly reported 0.
        contract_size: f64_add = |&i| (rows[i].1.contract_size != 0.0).then_some(rows[i].1.contract_size),
        // additive, appended LAST + nullable — same contract as `contract_size` above. The
        // VENUE-DECLARED order hold in ms (`SymbolProperties::taker_hold_ms`): Polymarket's
        // `itode` → 250 on the crypto up/down markets and `seconds_delay` → 3000 on sports GAME
        // markets; every other venue leaves it absent. Absent (0) is written as NULL rather than 0
        // so "the venue never told us" stays distinguishable in the tape from a venue that
        // explicitly reported no hold.
        //
        // ⚠ THIS COLUMN IS THE POINT. A `SymbolProperties` field with no column here is SILENTLY
        // DROPPED on a store round-trip. `taker_hold_ms` IS populated, so the column ships with the
        // field — as does `tick_scheme` below, whose column closed exactly this trap once the type
        // gained an optional tiered grid a venue parser can populate.
        taker_hold_ms: i64_add_null = |&i| (rows[i].1.taker_hold_ms != 0).then_some(i64::from(rows[i].1.taker_hold_ms)),
        // additive, appended LAST + nullable — the COLUMNAR twin of the serde `tick_scheme` field,
        // and the whole point of this change (the hard prerequisite the field doc on
        // `vike_model::SymbolProperties::tick_scheme` documents). The OPTIONAL tiered price grid,
        // JSON-encoded into a nullable Utf8 column as the same venue-shaped `{base_tick, tiers:[…]}`
        // the type's own `Serialize` emits. `None` — every venue but a Deribit option today — is
        // written as a SQL NULL so "no scheme" stays distinguishable in the tape, and a part written
        // before this column existed decodes to `None` (the `str_add_null` flavor reads through
        // `opt_str_nullable_col`, preserving a NULL cell as absent rather than flattening it to "").
        // Serialization is infallible for this validated `Copy` value, so `.expect` can never fire.
        // Without this column a populated `TickScheme` is SILENTLY DROPPED on a store round-trip.
        tick_scheme: str_add_null = |&i| rows[i]
            .1
            .tick_scheme
            .as_ref()
            .map(|s| serde_json::to_string(s).expect("TickScheme JSON-serializes infallibly")),
    },
    decode_row = |i| (
        ts.value(i),
        SymbolProperties {
            tick_size: tick_size.value(i),
            step_size: step_size.value(i),
            min_qty: min_qty.value(i),
            max_qty: max_qty.value(i),
            min_notional: min_notional.value(i),
            // absent column (old part) OR NULL cell → 0.0, the struct's absent convention, which
            // `SymbolProperties::multiplier` folds to the inert 1.0.
            contract_size: contract_size
                .and_then(|c| (!c.is_null(i)).then(|| c.value(i)))
                .unwrap_or(0.0),
            // absent column (a part written before this column existed) OR a NULL cell → `None`
            // (a flat grid); a present JSON string is re-parsed through `TickScheme`'s validating
            // `Deserialize` (a malformed row ERRORS, never a silently dropped or zero-tick grid).
            // This CLOSES the round-trip hole the field doc on `SymbolProperties::tick_scheme`
            // calls out — a populated scheme now survives a store round-trip.
            tick_scheme: decode_tick_scheme(tick_scheme.as_ref().and_then(|c| c[i].as_deref()))?,
            // absent column (a part written before this column existed) OR a NULL cell → 0, the
            // struct's absent convention = "this venue declares no hold". A negative or
            // out-of-u32-range cell is impossible from this codec's own writer and is likewise
            // folded to 0 rather than wrapping.
            taker_hold_ms: taker_hold_ms
                .and_then(|c| (!c.is_null(i)).then(|| c.value(i)))
                .and_then(|v| u32::try_from(v).ok())
                .unwrap_or(0),
        },
    ),
    sort_key = |row| (row.0, 0),
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

// ---- book updates (kind=book; schema v1) -----------------------------------------------------
// One row PER LEVEL; rows of one event share (ts, seq, kind). An event with zero levels (status
// kinds; degenerate empty snapshot) writes ONE placeholder row is_bid=true, price=0.0, size=0.0,
// decoded back to empty level vecs (price==0&&size==0 is not a real level on any venue — Polymarket
// prices are in (0,1) and no venue quotes at exactly 0; the schema-version metadata is the escape
// hatch if that ever changes).
//
// The per-level ROW trio is generated like every other kind; the explode (`book_rows`) and regroup
// (`book_updates_from_rows`) around it are this kind's own and stay hand-written.

pub(super) const BOOK_SCHEMA_VERSION: &str = "1";

pub(super) fn book_kind_code(k: BookUpdateKind) -> i8 {
    match k {
        BookUpdateKind::Delta => 0,
        BookUpdateKind::Snapshot => 1,
        BookUpdateKind::GapStart => 2,
        BookUpdateKind::Stale => 3,
        BookUpdateKind::LiveResume => 4,
    }
}

fn book_kind_from(code: i8) -> Result<BookUpdateKind, DataError> {
    Ok(match code {
        0 => BookUpdateKind::Delta,
        1 => BookUpdateKind::Snapshot,
        2 => BookUpdateKind::GapStart,
        3 => BookUpdateKind::Stale,
        4 => BookUpdateKind::LiveResume,
        other => return Err(q(format!("unknown book kind code {other}"))),
    })
}

/// One exploded per-level row — the write-side flattening of a [`BookUpdate`].
#[derive(Clone)] // grouped writes sort symbol-major, which reorders rows
pub(super) struct BookRow {
    pub ts: i64,
    pub local_ts: i64,
    pub seq: i64,
    pub kind: i8,
    pub is_bid: bool,
    pub price: f64,
    pub size: f64,
    pub tick_size: f64,
    /// The event's own symbol. Redundant under the per-symbol layout — the series path names it and
    /// `book_updates_from_rows` re-injects it from `ctx` — and REQUIRED under a grouped series,
    /// where one part holds many symbols and the path can no longer say whose row this is.
    ///
    /// Book is why this matters at all: one Polymarket token's market is ~352,935 book rows against
    /// ~17,364 quotes and ~1,087 trades, so grouping quotes and trades alone would leave ~95% of the
    /// volume un-grouped.
    pub symbol: String,
}

/// Explode events into per-level rows (placeholder row for zero-level events).
pub(super) fn book_rows(updates: &[BookUpdate]) -> Vec<BookRow> {
    let mut out = Vec::new();
    for u in updates {
        let base = |is_bid: bool, price: f64, size: f64| BookRow {
            ts: u.ts,
            local_ts: u.local_ts,
            seq: u.seq as i64,
            kind: book_kind_code(u.kind),
            is_bid,
            price,
            size,
            tick_size: u.tick_size,
            symbol: u.symbol.clone(),
        };
        if u.bids.is_empty() && u.asks.is_empty() {
            out.push(base(true, 0.0, 0.0)); // placeholder — decodes to empty levels
            continue;
        }
        for &(p, s) in &u.bids {
            out.push(base(true, p, s));
        }
        for &(p, s) in &u.asks {
            out.push(base(false, p, s));
        }
    }
    out
}

series_codec! {
    Codec = BookCodec,
    Row = BookRow,
    rows = rows,
    ctx = symbol,
    schema_fn = book_schema,
    columns_fn = book_columns,
    decode_fn = book_rows_from_batch,
    meta = ("vike.schema.book", BOOK_SCHEMA_VERSION),
    fields {
        ts: i64 = |&i| rows[i].ts,
        local_ts: i64 = |&i| rows[i].local_ts,
        seq: i64 = |&i| rows[i].seq,
        kind: i8 = |&i| rows[i].kind,
        is_bid: bool = |&i| rows[i].is_bid,
        price: f64 = |&i| rows[i].price,
        size: f64 = |&i| rows[i].size,
        tick_size: f64 = |&i| rows[i].tick_size,
        // additive (series grouping) — same reasoning as the quote/trade codecs, and the one that
        // matters most by volume: book is ~95% of a Polymarket token's rows.
        symbol_col: str_add = |&i| rows[i].symbol.as_str(),
    },
    decode_row = |i| BookRow {
        ts: ts.value(i),
        local_ts: local_ts.value(i),
        seq: seq.value(i),
        kind: kind.value(i),
        is_bid: is_bid.value(i),
        price: price.value(i),
        size: size.value(i),
        tick_size: tick_size.value(i),
        // Absent OR empty falls back to `ctx`, exactly like quotes/trades: a per-symbol caller may
        // legally leave `BookUpdate::symbol` empty and be tagged from the path.
        symbol: symbol_col
            .as_mut()
            .map(|v| std::mem::take(&mut v[i]))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| symbol.to_string()),
    },
    sort_key = |row| (row.ts, row.seq),
}

/// Regroup sorted per-level rows into events. Boundary rule: a status-kind row is ALWAYS its own
/// single-row event; two consecutive Delta/Snapshot rows sharing `(seq, kind)` are treated as ONE
/// event's levels BY DESIGN.
///
/// This relies on Delta/Snapshot seqs being monotonic-DISTINCT across events (status kinds are
/// force-split by the `status` flag and per `vike-model/orderbook.rs` carry seq 0) — so two DISTINCT
/// events can never share `(seq, kind)`. The `debug_assert!` below guards that invariant: the rows of
/// one event share a frame timestamp, so if a feed bug ever emitted two distinct Delta/Snapshot events
/// with the same seq their rows would differ in `ts` and the assert fires instead of silently merging
/// them. It cannot false-positive on the legitimate multi-level case (those rows share `ts`/`local_ts`).
pub(super) fn book_updates_from_rows(
    rows: Vec<BookRow>,
    symbol: &str,
) -> Result<Vec<BookUpdate>, DataError> {
    let mut out: Vec<BookUpdate> = Vec::new();
    for r in rows {
        let kind = book_kind_from(r.kind)?;
        // Resolve the row's symbol ONCE, and compare like with like below. Comparing the row's RAW
        // symbol against the event's RESOLVED one splits every multi-level event whose rows carry an
        // empty symbol (the legal per-symbol case): the event takes `ctx`, the next row is still "",
        // they differ, and each level becomes its own event.
        let row_symbol: &str = if r.symbol.is_empty() { symbol } else { &r.symbol };
        let status = !matches!(kind, BookUpdateKind::Delta | BookUpdateKind::Snapshot);
        let placeholder = r.price == 0.0 && r.size == 0.0;
        // A GROUPED part interleaves symbols, so a symbol CHANGE always starts a new event even when
        // (seq, kind) would otherwise fold — without this, two tokens sharing a seq would merge
        // into one BookUpdate carrying the other symbol's levels.
        let start_new = status
            || out.last().is_none_or(|u| {
                u.seq as i64 != r.seq
                    || u.symbol != row_symbol
                    || book_kind_code(u.kind) != book_kind_code(kind)
                    || !matches!(u.kind, BookUpdateKind::Delta | BookUpdateKind::Snapshot)
            });
        if start_new {
            out.push(BookUpdate {
                ts: r.ts,
                local_ts: r.local_ts,
                seq: r.seq as u64,
                kind,
                tick_size: r.tick_size,
                bids: Vec::new(),
                asks: Vec::new(),
                // The ROW's symbol when it has one (a grouped part holds many), else `ctx`. The
                // fallback is NOT redundant with decode's: `book_rows` copies `BookUpdate::symbol`
                // verbatim, and a per-symbol caller may legally leave that empty — so the
                // `book_rows` -> `book_updates_from_rows` round trip compaction performs would
                // otherwise lose the symbol entirely.
                symbol: row_symbol.to_string(),
            });
        } else {
            // Folding into the previous event because `(seq, kind)` matched — only sound if this is
            // genuinely the same frame (same ts/local_ts). Guards the monotonic-distinct-seq invariant.
            debug_assert!(
                out.last().is_some_and(|u| u.ts == r.ts && u.local_ts == r.local_ts),
                "book regroup: consecutive Delta/Snapshot rows share (seq, kind) but differ in \
                 timestamp — Delta/Snapshot seqs must be monotonic-distinct (would wrongly merge \
                 two events into one)"
            );
        }
        if !placeholder {
            let u = out.last_mut().expect("just pushed or existing");
            if r.is_bid {
                u.bids.push((r.price, r.size));
            } else {
                u.asks.push((r.price, r.size));
            }
        }
    }
    Ok(out)
}

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

// ---- realized perp funding (kind=funding) -----------------------------------------------------
// The ACCOUNT realized-funding series (Tier-2) — a DISTINCT kind from the market `kind=trade` series
// so account funding never collides with a symbol's public prints. Row type lives in
// `crate::funding_log` (always compiled, model-only); the codec sits here because `SeriesCodec` is
// module-private. Identity `(venue, symbol=<coin>)` is the partition path only — every field
// (`ts`/`usdc`/`szi`/`funding_rate`/`hash`) is intrinsic to the row, so `decode` ignores `ctx`
// (exactly like `BarCodec`/`ExecFillCodec`), and the `compact_roundtrip` re-encode (which passes
// `ctx=""`) preserves every column verbatim.

series_codec! {
    Codec = FundingCodec,
    Row = FundingRow,
    rows = rows,
    schema_fn = funding_schema,
    columns_fn = funding_columns,
    decode_fn = funding_from_batch,
    fields {
        ts: i64 = |&i| rows[i].ts,
        usdc: f64 = |&i| rows[i].usdc,
        szi: f64 = |&i| rows[i].szi,
        funding_rate: f64 = |&i| rows[i].funding_rate,
        hash: str = |&i| rows[i].hash.as_str(),
    },
    decode_row = |i| FundingRow {
        ts: ts.value(i),
        usdc: usdc.value(i),
        szi: szi.value(i),
        funding_rate: funding_rate.value(i),
        hash: std::mem::take(&mut hash[i]),
    },
    sort_key = |row| (row.ts, 0),
}

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
// `crate::cohort_rec::CohortRecorder` is the producer. Row type lives in `crate::cohort_log`
// (always compiled, model-only); the codec sits here because `SeriesCodec` is module-private.
//
// A LONG row — one row per (hour, asset, axis, label) rather than a column per label. The argument
// is `crate::store_kind::STORE_KINDS`' `cohort` row (36 admitted labels × 2 wire numbers = 73
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_from_batch_defaults_missing_local_ts() {
        // A batch built with the PRE-plan schema (no local_ts column) must decode with
        // local_ts = 0 — the additive-column contract for already-recorded data.
        let schema = Arc::new(Schema::new(vec![
            Field::new("ts", DataType::Int64, false),
            Field::new("bid", DataType::Float64, false),
            Field::new("ask", DataType::Float64, false),
            Field::new("bid_size", DataType::Float64, false),
            Field::new("ask_size", DataType::Float64, false),
        ]));
        let batch = RecordBatch::try_new(
            schema,
            vec![
                Arc::new(Int64Array::from(vec![5i64])),
                Arc::new(Float64Array::from(vec![1.0])),
                Arc::new(Float64Array::from(vec![1.1])),
                Arc::new(Float64Array::from(vec![2.0])),
                Arc::new(Float64Array::from(vec![3.0])),
            ],
        )
        .unwrap();
        let rows = quotes_from_batch(&batch, "S").unwrap();
        assert_eq!(rows[0].local_ts, 0);
    }

    #[test]
    fn exec_fills_from_batch_defaults_missing_mark_price() {
        // A batch built with the PRE-mark_price schema (no mark_price column at all — the exact
        // shape of an exec_fill part written before this field existed) must decode with
        // mark_price = None rather than erroring — the additive-column contract, mirroring
        // `quotes_from_batch_defaults_missing_local_ts` above for `local_ts`.
        let schema = Arc::new(Schema::new(vec![
            Field::new("ts", DataType::Int64, false),
            Field::new("trade_id", DataType::Utf8, false),
            Field::new("client_order_id", DataType::Utf8, false),
            Field::new("venue", DataType::Utf8, false),
            Field::new("symbol", DataType::Utf8, false),
            Field::new("side", DataType::Int64, false),
            Field::new("qty", DataType::Float64, false),
            Field::new("px", DataType::Float64, false),
            Field::new("commission", DataType::Float64, false),
        ]));
        let batch = RecordBatch::try_new(
            schema,
            vec![
                Arc::new(Int64Array::from(vec![5i64])),
                Arc::new(StringArray::from(vec!["t1"])),
                Arc::new(StringArray::from(vec!["c1"])),
                Arc::new(StringArray::from(vec!["binance"])),
                Arc::new(StringArray::from(vec!["BTCUSDT"])),
                Arc::new(Int64Array::from(vec![1i64])),
                Arc::new(Float64Array::from(vec![0.5])),
                Arc::new(Float64Array::from(vec![65_000.0])),
                Arc::new(Float64Array::from(vec![0.13])),
            ],
        )
        .unwrap();
        let rows = exec_fills_from_batch(&batch).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].mark_price, None);
        // this same oldest-shape batch also predates liquidity_side/commission_asset — both must
        // default to "" rather than erroring.
        assert_eq!(rows[0].liquidity_side, "");
        assert_eq!(rows[0].commission_asset, "");
    }

    #[test]
    fn exec_fills_from_batch_defaults_missing_liquidity_and_commission() {
        // A batch built with the schema as it stood right after mark_price shipped (#374) — i.e. it
        // HAS mark_price but predates liquidity_side/commission_asset — must decode those two with
        // "" rather than erroring. This is the realistic upgrade path: parts already on disk carry
        // mark_price but not the two fields added in this change.
        let schema = Arc::new(Schema::new(vec![
            Field::new("ts", DataType::Int64, false),
            Field::new("trade_id", DataType::Utf8, false),
            Field::new("client_order_id", DataType::Utf8, false),
            Field::new("venue", DataType::Utf8, false),
            Field::new("symbol", DataType::Utf8, false),
            Field::new("side", DataType::Int64, false),
            Field::new("qty", DataType::Float64, false),
            Field::new("px", DataType::Float64, false),
            Field::new("commission", DataType::Float64, false),
            Field::new("mark_price", DataType::Float64, true),
        ]));
        let batch = RecordBatch::try_new(
            schema,
            vec![
                Arc::new(Int64Array::from(vec![5i64])),
                Arc::new(StringArray::from(vec!["t1"])),
                Arc::new(StringArray::from(vec!["c1"])),
                Arc::new(StringArray::from(vec!["binance"])),
                Arc::new(StringArray::from(vec!["BTCUSDT"])),
                Arc::new(Int64Array::from(vec![1i64])),
                Arc::new(Float64Array::from(vec![0.5])),
                Arc::new(Float64Array::from(vec![65_000.0])),
                Arc::new(Float64Array::from(vec![0.13])),
                Arc::new(Float64Array::from(vec![65_001.0])),
            ],
        )
        .unwrap();
        let rows = exec_fills_from_batch(&batch).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].mark_price, Some(65_001.0)); // still decodes fine
        assert_eq!(rows[0].liquidity_side, "");
        assert_eq!(rows[0].commission_asset, "");
    }

    #[test]
    fn filter_codec_round_trips_all_fields() {
        use vike_model::SymbolProperties;
        let rows = vec![
            (
                1_000i64,
                SymbolProperties {
                    tick_size: 0.01,
                    step_size: 0.001,
                    min_qty: 0.001,
                    max_qty: 9000.0,
                    min_notional: 5.0,
                    // a deribit-shaped contract size: the additive column must survive round-trip
                    contract_size: 10.0,
                    tick_scheme: None,
                    taker_hold_ms: 0,
                },
            ),
            (
                2_000i64,
                SymbolProperties {
                    tick_size: 0.5,
                    step_size: 1.0,
                    min_qty: 1.0,
                    max_qty: 0.0,
                    min_notional: 0.0,
                    // …and the absent case round-trips as absent (written NULL, decoded 0.0)
                    contract_size: 0.0,
                    tick_scheme: None,
                    taker_hold_ms: 0,
                },
            ),
        ];
        let idxs: Vec<usize> = (0..rows.len()).collect();
        let cols = properties_columns(&rows, &idxs);
        let batch = RecordBatch::try_new(properties_schema(), cols).unwrap();
        let got = properties_from_batch(&batch).unwrap();
        assert_eq!(got, rows);
        assert_eq!(
            properties_schema().metadata().get("vike.schema.properties").map(String::as_str), // (was vike.schema.properties)
            Some("1")
        );
    }

    /// THE schema-compat pin for the PIT properties series: a part written BEFORE `contract_size`
    /// existed has no such column at all. It must still decode (to the absent `0.0`), exactly like
    /// exec_fill's `mark_price` — this is why the column is `f64_add` (nullable, appended LAST) and
    /// why the schema version stays "1". A non-additive column here would break every recorded
    /// properties part on disk.
    #[test]
    fn properties_codec_decodes_parts_written_before_contract_size() {
        use vike_model::SymbolProperties;
        // rebuild the OLD schema/batch: every column except the appended `contract_size`
        let old_schema = Arc::new(Schema::new(vec![
            Field::new("ts", DataType::Int64, false),
            Field::new("tick_size", DataType::Float64, false),
            Field::new("step_size", DataType::Float64, false),
            Field::new("min_qty", DataType::Float64, false),
            Field::new("max_qty", DataType::Float64, false),
            Field::new("min_notional", DataType::Float64, false),
        ]));
        let batch = RecordBatch::try_new(
            old_schema,
            vec![
                Arc::new(Int64Array::from(vec![1_000i64])),
                Arc::new(Float64Array::from(vec![0.01])),
                Arc::new(Float64Array::from(vec![0.001])),
                Arc::new(Float64Array::from(vec![0.001])),
                Arc::new(Float64Array::from(vec![9000.0])),
                Arc::new(Float64Array::from(vec![5.0])),
            ],
        )
        .unwrap();
        let got =
            properties_from_batch(&batch).expect("a pre-contract_size part must still decode");
        assert_eq!(
            got,
            vec![(
                1_000i64,
                SymbolProperties {
                    tick_size: 0.01,
                    step_size: 0.001,
                    min_qty: 0.001,
                    max_qty: 9000.0,
                    min_notional: 5.0,
                    contract_size: 0.0,
                    tick_scheme: None,
                    taker_hold_ms: 0,
                },
            )]
        );
        assert_eq!(got[0].1.multiplier(), 1.0, "an old part is inert, never a 0.0 multiplier");
    }

    /// The `taker_hold_ms` twin of the pin above, and the reason the column exists at all: a
    /// `SymbolProperties` field with NO column is silently dropped on a store round-trip. Both
    /// live Polymarket values survive — `itode` → 250 (crypto up/down) and `seconds_delay` → 3000
    /// (sports game markets) — and absent stays absent.
    #[test]
    fn properties_codec_round_trips_the_venue_taker_hold() {
        use vike_model::SymbolProperties;
        let rows: Vec<(i64, SymbolProperties)> = [0u32, 250, 3000]
            .iter()
            .enumerate()
            .map(|(i, &hold)| {
                (
                    1_000i64 + i as i64,
                    SymbolProperties { tick_size: 0.01, taker_hold_ms: hold, ..Default::default() },
                )
            })
            .collect();
        let idxs: Vec<usize> = (0..rows.len()).collect();
        let batch =
            RecordBatch::try_new(properties_schema(), properties_columns(&rows, &idxs)).unwrap();
        // the ABSENT hold is written as a SQL NULL, not a 0 — "the venue never told us" stays
        // distinguishable in the tape from an explicit no-hold.
        let col = batch.column_by_name("taker_hold_ms").expect("the column must exist");
        assert!(col.is_null(0), "hold 0 is written NULL");
        assert!(!col.is_null(1) && !col.is_null(2));
        assert_eq!(properties_from_batch(&batch).unwrap(), rows);
    }

    /// A part written BEFORE `taker_hold_ms` existed — i.e. EVERY properties part on every store
    /// today — has no such column. It must still decode, to the absent `0` (= "this venue declares
    /// no hold"), which is why the column is additive+nullable and appended LAST and why the
    /// schema version stays "1".
    #[test]
    fn properties_codec_decodes_parts_written_before_taker_hold_ms() {
        use vike_model::SymbolProperties;
        // the PREVIOUS schema: everything through the `contract_size` column, nothing after it
        let old_schema = Arc::new(Schema::new(vec![
            Field::new("ts", DataType::Int64, false),
            Field::new("tick_size", DataType::Float64, false),
            Field::new("step_size", DataType::Float64, false),
            Field::new("min_qty", DataType::Float64, false),
            Field::new("max_qty", DataType::Float64, false),
            Field::new("min_notional", DataType::Float64, false),
            Field::new("contract_size", DataType::Float64, true),
        ]));
        let batch = RecordBatch::try_new(
            old_schema,
            vec![
                Arc::new(Int64Array::from(vec![1_000i64])),
                Arc::new(Float64Array::from(vec![0.01])),
                Arc::new(Float64Array::from(vec![0.001])),
                Arc::new(Float64Array::from(vec![0.001])),
                Arc::new(Float64Array::from(vec![9000.0])),
                Arc::new(Float64Array::from(vec![5.0])),
                Arc::new(Float64Array::from(vec![10.0])),
            ],
        )
        .unwrap();
        let got =
            properties_from_batch(&batch).expect("a pre-taker_hold_ms part must still decode");
        assert_eq!(
            got,
            vec![(
                1_000i64,
                SymbolProperties {
                    tick_size: 0.01,
                    step_size: 0.001,
                    min_qty: 0.001,
                    max_qty: 9000.0,
                    min_notional: 5.0,
                    contract_size: 10.0,
                    tick_scheme: None,
                    taker_hold_ms: 0,
                },
            )]
        );
    }

    /// The `tick_scheme` twin of `properties_codec_round_trips_the_venue_taker_hold`, and the whole
    /// point of this change: a `SymbolProperties` field with NO codec column is silently dropped on a
    /// store round-trip. A scheme-BEARING row (a real Deribit option grid) survives encode→decode
    /// with its scheme intact and still resolves by price; a scheme-LESS row stays `None` — written
    /// as a SQL NULL, distinguishable in the tape from a present value.
    #[test]
    fn properties_codec_round_trips_the_tick_scheme() {
        use vike_model::{SymbolProperties, TickScheme, TickTier};
        // the real Deribit BTC-option grid: base 0.0001, 0.0005 above 0.005.
        let scheme = TickScheme::new(0.0001, &[TickTier { above_price: 0.005, tick_size: 0.0005 }])
            .expect("valid deribit grid");
        let rows: Vec<(i64, SymbolProperties)> = vec![
            // scheme-LESS — the state every venue but a Deribit option is in
            (1_000, SymbolProperties { tick_size: 0.01, ..Default::default() }),
            // scheme-BEARING — base_tick equals tick_size (the scalar non-price consumers still read)
            (
                2_000,
                SymbolProperties {
                    tick_size: 0.0001,
                    tick_scheme: Some(scheme),
                    ..Default::default()
                },
            ),
        ];
        let idxs: Vec<usize> = (0..rows.len()).collect();
        let batch =
            RecordBatch::try_new(properties_schema(), properties_columns(&rows, &idxs)).unwrap();
        // the ABSENT scheme is written as a SQL NULL; the present one as a JSON string.
        let col = batch.column_by_name("tick_scheme").expect("the column must exist");
        assert!(col.is_null(0), "a None scheme is written NULL");
        assert!(!col.is_null(1), "a Some scheme is written as a JSON string");
        // and the whole grid rides back through intact.
        let got = properties_from_batch(&batch).unwrap();
        assert_eq!(got, rows);
        assert_eq!(got[1].1.tick_scheme, Some(scheme), "the scheme survives the round-trip");
        // the recovered scheme still resolves by price (not flattened to a scalar tick).
        assert_eq!(got[1].1.effective_tick(0.05), 0.0005);
        assert_eq!(got[1].1.effective_tick(0.004), 0.0001);
    }

    /// A part written BEFORE `tick_scheme` existed — i.e. EVERY properties part on every store today
    /// — has no such column. It must still decode, to the absent `None` (a flat grid), which is why
    /// the column is additive+nullable, appended LAST, and the schema version stays "1". Mirrors the
    /// contract_size / taker_hold_ms pins above.
    #[test]
    fn properties_codec_decodes_parts_written_before_tick_scheme() {
        use vike_model::SymbolProperties;
        // the PREVIOUS schema: everything through `taker_hold_ms`, nothing after it.
        let old_schema = Arc::new(Schema::new(vec![
            Field::new("ts", DataType::Int64, false),
            Field::new("tick_size", DataType::Float64, false),
            Field::new("step_size", DataType::Float64, false),
            Field::new("min_qty", DataType::Float64, false),
            Field::new("max_qty", DataType::Float64, false),
            Field::new("min_notional", DataType::Float64, false),
            Field::new("contract_size", DataType::Float64, true),
            Field::new("taker_hold_ms", DataType::Int64, true),
        ]));
        let batch = RecordBatch::try_new(
            old_schema,
            vec![
                Arc::new(Int64Array::from(vec![1_000i64])),
                Arc::new(Float64Array::from(vec![0.01])),
                Arc::new(Float64Array::from(vec![0.001])),
                Arc::new(Float64Array::from(vec![0.001])),
                Arc::new(Float64Array::from(vec![9000.0])),
                Arc::new(Float64Array::from(vec![5.0])),
                Arc::new(Float64Array::from(vec![10.0])),
                Arc::new(Int64Array::from(vec![250i64])),
            ],
        )
        .unwrap();
        let got = properties_from_batch(&batch).expect("a pre-tick_scheme part must still decode");
        assert_eq!(
            got,
            vec![(
                1_000i64,
                SymbolProperties {
                    tick_size: 0.01,
                    step_size: 0.001,
                    min_qty: 0.001,
                    max_qty: 9000.0,
                    min_notional: 5.0,
                    contract_size: 10.0,
                    tick_scheme: None,
                    taker_hold_ms: 250,
                },
            )]
        );
        assert!(got[0].1.tick_scheme.is_none(), "an old part decodes to a flat grid");
    }

    #[test]
    fn chain_codec_round_trips_all_fields() {
        // A fully-populated call and a sparse put: every stored column (incl. the string identity
        // pair and each nullable quote/greek) survives encode→decode; absent stays absent.
        let full = ChainRow {
            ts: 1_780_387_200_000,
            underlying: "BTC".into(),
            instrument: "BTC-27JUN26-100000-C".into(),
            expiry_ms: 1_782_547_200_000,
            strike: 100_000.0,
            is_call: true,
            bid: Some(5_200.0),
            ask: Some(6_240.0),
            mark: Some(5_720.0),
            iv: Some(0.625),
            open_interest: Some(120.0),
            volume: Some(8.0),
            delta: Some(0.55),
            gamma: Some(0.000_01),
            theta: Some(-45.2),
            vega: Some(210.0),
        };
        let sparse = ChainRow {
            instrument: "BTC-27JUN26-100000-P".into(),
            is_call: false,
            bid: None,
            ask: None,
            mark: None,
            iv: None,
            open_interest: None,
            volume: None,
            delta: None,
            gamma: None,
            theta: None,
            vega: None,
            ..full.clone()
        };
        let rows = vec![full, sparse];
        let idxs: Vec<usize> = (0..rows.len()).collect();
        let batch = RecordBatch::try_new(chain_schema(), chain_columns(&rows, &idxs)).unwrap();
        assert_eq!(chain_from_batch(&batch).unwrap(), rows);
        assert_eq!(
            chain_schema().metadata().get("vike.schema.chain").map(String::as_str),
            Some("1")
        );
    }

    #[test]
    fn funding_codec_round_trips_all_fields() {
        // A PAID row (negative usdc, long szi) and a RECEIVED row (positive usdc, short szi, negative
        // rate) — every column (incl. the signed f64s and the string hash) survives encode→decode.
        let rows = vec![
            FundingRow {
                ts: 1_681_222_254_710,
                usdc: -1.25,
                szi: 0.5,
                funding_rate: 0.000_012_5,
                hash: "0xabc".into(),
            },
            FundingRow {
                ts: 1_681_222_254_720,
                usdc: 0.75,
                szi: -2.0,
                funding_rate: -0.000_008_8,
                hash: "0xdef".into(),
            },
        ];
        let idxs: Vec<usize> = (0..rows.len()).collect();
        let batch = RecordBatch::try_new(funding_schema(), funding_columns(&rows, &idxs)).unwrap();
        assert_eq!(funding_from_batch(&batch).unwrap(), rows);
    }
}
