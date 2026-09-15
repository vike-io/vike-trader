//! Opt-in PIT properties recording for Polymarket. Polymarket markets are BINARY prediction markets —
//! there is no lot/step/min-qty/min-notional grid, only a per-token tick size (see
//! [`crate::instruments::PolyMarket::tick_size`]). This is the tick-only twin of the other venues'
//! properties-recording wiring (task 8 of the PIT instrument-properties recording work): entirely behind
//! this crate's `polymarket` Cargo feature, same as the rest of the module tree.

use std::sync::Arc;

use vike_data::PropertiesRecorder;
use vike_model::SymbolProperties;

/// Record a Polymarket token's tick size into the PIT properties store (opt-in) as a tick-only
/// `SymbolProperties` (other fields 0.0 — Polymarket binary markets have no lot/notional grid).
/// Symbol = token_id, venue = "polymarket". Best-effort. Entrypoint for when token resolution is
/// live-wired; `pub` so it isn't dead code under the feature.
///
/// Records NO venue hold. Prefer [`record_token_properties`] wherever the market's
/// `condition_id` is in hand — the hold is a per-market venue property a replay needs (see
/// [`crate::taker_hold`]), and a row written through this function claims the market has none.
pub fn record_token_tick(
    rec: &Option<Arc<PropertiesRecorder>>,
    token_id: &str,
    tick_size: f64,
    ts_ns: i64,
) {
    record_token_properties(rec, token_id, tick_size, 0, ts_ns)
}

/// The full per-token PIT properties row: tick size PLUS the venue's declared order hold
/// (`taker_hold_ms`, resolved by [`crate::taker_hold::fetch_taker_hold_ms`] /
/// [`crate::taker_hold::resolve_taker_hold_ms`] — `itode` → 250 ms on the crypto up/down markets,
/// `seconds_delay` → 3000 ms on sports GAME markets, `0` everywhere else).
///
/// This is the ONE site that puts the hold on the `kind=properties` tape, which is where a
/// backtest reads it back from (`HistStore::properties_as_of` → `EngineParams::properties` → the
/// latency gate's per-symbol hold table). `0` is the absent convention and writes a NULL cell, so
/// a market whose hold could not be resolved is indistinguishable from one that declares none —
/// deliberately: an unknown hold is modelled as no hold, never guessed.
pub fn record_token_properties(
    rec: &Option<Arc<PropertiesRecorder>>,
    token_id: &str,
    tick_size: f64,
    taker_hold_ms: u32,
    ts_ns: i64,
) {
    if let Some(r) = rec {
        r.record(
            "polymarket",
            token_id,
            SymbolProperties { tick_size, taker_hold_ms, ..Default::default() },
            ts_ns,
        );
    }
}

#[cfg(all(test, feature = "polymarket"))]
mod tests {
    //! PIT properties recording (task 8): `record_token_tick` writes a tick-only `SymbolProperties` row
    //! into the store when a recorder is present, keyed by venue `"polymarket"` and symbol =
    //! token_id. Uses the DataFusion-free `MemHistStore` test double (vike-data's `test-support`
    //! dev-feature) so this venue's default build never pulls DataFusion in.
    use std::sync::Arc;

    use vike_data::{HistStore, MemHistStore, PropertiesRecorder, TsRange};

    use super::{record_token_properties, record_token_tick};

    const TOKEN_ID: &str = "0xTOKEN0000000000000000000000000000000000000000000000000000001";
    const NS_2020_01_01: i64 = 1_577_836_800_000_000_000; // 2020-01-01T00:00:00Z in ns

    #[test]
    fn poly_records_tick_as_properties() {
        let store = Arc::new(MemHistStore::new());
        let rec = Arc::new(PropertiesRecorder::new(store.clone(), true));
        record_token_tick(&Some(rec), TOKEN_ID, 0.01, NS_2020_01_01);
        let rows = store.scan_symbol_properties("polymarket", TOKEN_ID, TsRange::all()).unwrap();
        assert_eq!(rows.len(), 1);
        let f = rows[0].1;
        assert_eq!(f.tick_size, 0.01);
        assert_eq!(f.step_size, 0.0);
        assert_eq!(f.min_qty, 0.0);
        assert_eq!(f.max_qty, 0.0);
        assert_eq!(f.min_notional, 0.0);
        assert_eq!(f.taker_hold_ms, 0, "the tick-only entrypoint claims no venue hold");
    }

    /// Both live venue holds ride through the recorder onto the PIT tape. (This uses the
    /// DataFusion-free `MemHistStore`, so it pins the RECORDER, not the Parquet codec — the
    /// columnar half is pinned by vike-data's `properties_codec_round_trips_the_venue_taker_hold`,
    /// and it has to be: a `SymbolProperties` field with no codec column comes back `0` with no
    /// error at all.)
    #[test]
    fn poly_records_the_venue_taker_hold() {
        for hold in [crate::taker_hold::HOLD_ITODE_MS, 3_000] {
            let store = Arc::new(MemHistStore::new());
            let rec = Arc::new(PropertiesRecorder::new(store.clone(), true));
            record_token_properties(&Some(rec), TOKEN_ID, 0.001, hold, NS_2020_01_01);
            let rows =
                store.scan_symbol_properties("polymarket", TOKEN_ID, TsRange::all()).unwrap();
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].1.taker_hold_ms, hold);
            assert_eq!(rows[0].1.tick_size, 0.001);
        }
    }

    #[test]
    fn poly_no_recorder_writes_nothing() {
        let store = Arc::new(MemHistStore::new());
        record_token_tick(&None, TOKEN_ID, 0.01, NS_2020_01_01);
        assert!(
            store
                .scan_symbol_properties("polymarket", TOKEN_ID, TsRange::all())
                .unwrap()
                .is_empty()
        );
    }
}
