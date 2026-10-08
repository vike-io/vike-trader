//! Arrow `RecordBatch` <-> domain-type codecs for the series kinds this store holds (bars, quotes,
//! trades, L2 book events, point-in-time symbol properties, and equity-curve samples): schema
//! definitions, column builders (encode), and row decoders (decode).
//!
//! ONE decode path per row type, shared by every `Vec` scan (same `col.value(i)` reads, same field
//! order), so two scans of one row can never decode it two ways. The scans `extend` with these, then
//! sort.
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
    Array, ArrayRef, BooleanArray, Float64Array, Int8Array, Int64Array, StringArray,
    StringViewArray,
};
use datafusion::arrow::datatypes::Schema;
use datafusion::arrow::record_batch::RecordBatch;

use crate::store::hist::DataError;

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

    /// The row's OWN instrument, for a kind with a GROUPED form — `None` for every other kind.
    ///
    /// `sort_key` is a row's whole natural key only when the PATH names the instrument. A grouped
    /// part (`group=…`) holds many symbols told apart by `symbol_col`, so there the key is
    /// `(symbol, sort_key)`, and a merge that forgets the symbol treats two instruments' rows at
    /// one ts as one observation. A CONST rather than a per-row method so a grouped merge of a kind
    /// without one is refused before a row is touched (`super::grouped_symbol_of`).
    ///
    /// ⚠ Never consult it for a PER-SYMBOL series: there a row's symbol cell may be empty (the
    /// tag-from-path contract) or stamped, so it does not identify anything the path does not.
    const ROW_SYMBOL: Option<RowSymbolFn<Self::Row>> = None;
}

/// Reads a row's OWN instrument out of a row of a kind with a grouped form — the type of
/// [`SeriesCodec::ROW_SYMBOL`], and of what a grouped merge keys on.
pub(super) type RowSymbolFn<R> = fn(&R) -> &str;

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
        $(row_symbol = $rsym:ident,)?
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
        pub(crate) enum $codec {}

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
            $(
                const ROW_SYMBOL: Option<RowSymbolFn<$row>> = {
                    fn row_symbol(row: &$row) -> &str {
                        row.$rsym.as_str()
                    }
                    Some(row_symbol)
                };
            )?
        }
    };
}

mod account;
mod book;
mod market;
mod properties;
mod research;

pub(super) use account::{ExecFillCodec, ExecOrderCodec, FundingCodec};
pub(super) use book::{BookCodec, BookRow, book_rows, book_updates_from_rows};
pub(super) use market::{BarCodec, EquityCodec, QuoteCodec, TradeCodec};
pub(super) use properties::PropertiesCodec;
pub(super) use research::{ChainCodec, CohortCodec, PerpMetricsCodec};

#[cfg(test)]
use crate::chain_log::ChainRow;
#[cfg(test)]
use crate::funding_log::FundingRow;
#[cfg(test)]
use account::{exec_fills_from_batch, funding_columns, funding_from_batch, funding_schema};
#[cfg(test)]
use datafusion::arrow::datatypes::{DataType, Field};
#[cfg(test)]
use market::quotes_from_batch;
#[cfg(test)]
use properties::{properties_columns, properties_from_batch, properties_schema};
#[cfg(test)]
use research::{chain_columns, chain_from_batch, chain_schema};
#[cfg(test)]
use vike_model::{QuoteTick, TradeTick};

#[path = "codec_tests.rs"]
#[cfg(test)]
mod codec_tests;
