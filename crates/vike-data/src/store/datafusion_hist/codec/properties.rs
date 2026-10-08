//! The point-in-time `SymbolProperties` codec and the two cell decoders its decode calls.

use std::sync::Arc;

use datafusion::arrow::array::{Array, ArrayRef};
use datafusion::arrow::datatypes::{DataType, Field, Schema};
use datafusion::arrow::record_batch::RecordBatch;

use vike_model::{AssetClass, SymbolProperties, TickScheme};

use crate::store::hist::DataError;

use super::super::q;
use super::{
    SeriesCodec, f64_col, gather_f64, gather_i64, gather_opt_f64, gather_opt_i64,
    gather_opt_string, i64_col, opt_f64_col, opt_i64_col, opt_str_nullable_col,
};

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

/// Decode one properties row's `asset_class` cell. `None` (an absent COLUMN — a part written before
/// this column existed — OR a NULL cell) → no class, which is the honest reading: nobody recorded
/// one, and `vike_model::SymbolProperties::asset_class`'s doc argues at length why that must stay
/// distinguishable from a class somebody chose.
///
/// An UNRECOGNISED word is an ERROR rather than a silent `None`, the same judgement
/// [`decode_tick_scheme`] makes for a malformed grid. `AssetClass::from_sql_word` answers `None`
/// for anything outside the closed vocabulary, and this codec's own writer can only ever emit
/// `AssetClass::sql_word`, so reaching that arm means the part was written by something other than
/// this tree — or by a tree whose vocabulary has since been RENAMED, which is exactly the case an
/// operator needs told rather than quietly folded into "unclassified".
fn decode_asset_class(cell: Option<&str>) -> Result<Option<AssetClass>, DataError> {
    match cell {
        None => Ok(None),
        Some(word) => AssetClass::from_sql_word(word)
            .map(Some)
            .ok_or_else(|| q(format!("unknown asset_class {word:?} in a properties row"))),
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
        // additive, appended LAST + nullable — same contract as the three columns above, and STEP 2
        // of `docs/decisions/0061-an-instrument-names-its-kind.md`: what KIND of instrument this
        // grid belongs to (`SymbolProperties::asset_class`), so the tape RECORDS spot-vs-perp
        // instead of leaving every consumer to re-derive it from the symbol string.
        //
        // Stored as the variant's own `AssetClass::sql_word` — which that type's module doc pins
        // equal to its serde word, so the Parquet cell, the JSON shape and the settings-database
        // column all carry ONE spelling rather than three. `None` (nobody recorded a class) is
        // written as a SQL NULL, never a sentinel word, so "unclassified" cannot collide with a
        // future variant; a part written before this column existed likewise decodes to `None`
        // (the `str_add_null` flavor reads through `opt_str_nullable_col`, which preserves a NULL
        // cell as absent rather than flattening it to "").
        asset_class: str_add_null = |&i| rows[i].1.asset_class.map(|c| c.sql_word().to_string()),
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
            // absent column (a part written before this column existed) OR a NULL cell → `None`
            // ("nobody recorded a class"); a present word is parsed through the closed
            // `AssetClass` vocabulary, and a word OUTSIDE it ERRORS rather than degrading to
            // `None` — see `decode_asset_class` for why that is the same judgement the
            // `tick_scheme` cell makes.
            asset_class: decode_asset_class(asset_class.as_ref().and_then(|c| c[i].as_deref()))?,
        },
    ),
    sort_key = |row| (row.0, 0),
}
