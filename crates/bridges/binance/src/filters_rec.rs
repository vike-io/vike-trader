//! Opt-in PIT instrument-properties recording glue for Binance (spot + perp): the venue face of
//! the shared [`crate::family::filters_rec`], binding it to the `"binance"` venue key.
//!
//! Binance is unlike bybit/okx here: its instrument properties come from FREE FUNCTIONS returning
//! maps (`spot::parse_symbol_properties`, `perp::parse_binance_perp_instruments`), not a method on a
//! live exec object, and there is no live spawn site for Binance exec in the app today (only the
//! kline DATA feed runs live). So this module is deliberately just the recording glue over both
//! parse outputs — the entrypoint the composition root calls once Binance exec is live-mounted —
//! rather than any `spawn_with_recorder`/constructor plumbing that nothing would call yet.
//!
//! The glue body is shared with vike-aster (F0, dedup rung 1); the venue key is the only thing
//! that differed between the two copies, so it is a parameter in `family` and this wrapper pins
//! it. The tests below stay HERE — they drive Binance's OWN `spot`/`perp` parsers, which are
//! venue-specific and deliberately out of the shared core.

/// Record parsed Binance instrument properties into the PIT store when a recorder is present
/// (opt-in). Entrypoint for the composition root when Binance exec is live-mounted. Best-effort.
pub fn record_properties_all(
    rec: &Option<std::sync::Arc<vike_data::PropertiesRecorder>>,
    entries: impl IntoIterator<Item = (String, vike_model::SymbolProperties)>,
    ts_ns: i64,
) {
    crate::family::filters_rec::record_properties_all(rec, "binance", entries, ts_ns);
}

#[cfg(test)]
mod tests {
    //! PIT properties recording (task 6): `record_properties_all` writes one row per symbol into the
    //! store when a recorder is present, keyed by venue `"binance"`, over BOTH the spot
    //! (`parse_symbol_properties`) and perp (`parse_binance_perp_instruments`, mapped to `.properties`)
    //! parse outputs. Uses the DataFusion-free `MemHistStore` test double (vike-data's
    //! `test-support` dev-feature) so this venue's default build never pulls DataFusion in.
    use std::sync::Arc;

    use indexmap::IndexMap;
    use vike_data::HistStore;
    use vike_model::SymbolProperties;

    use crate::perp::parse_binance_perp_instruments;
    use crate::spot::parse_symbol_properties;

    const TS_NS: i64 = 1_577_836_800_000_000_000;

    #[test]
    fn binance_spot_records_all_properties() {
        let payload = serde_json::json!({
            "symbols": [
                {
                    "symbol": "BTCUSDT",
                    "filters": [
                        {"filterType": "PRICE_FILTER", "tickSize": "0.10"},
                        {"filterType": "LOT_SIZE", "stepSize": "0.001", "minQty": "0.001", "maxQty": "9000.0"},
                        {"filterType": "NOTIONAL", "minNotional": "5.0"},
                    ]
                },
                {
                    "symbol": "ETHUSDT",
                    "filters": [
                        {"filterType": "PRICE_FILTER", "tickSize": "0.01"},
                        {"filterType": "LOT_SIZE", "stepSize": "0.01", "minQty": "0.01", "maxQty": "5000.0"},
                        {"filterType": "NOTIONAL", "minNotional": "10.0"},
                    ]
                },
            ]
        });
        let filters = parse_symbol_properties(&payload);
        assert_eq!(filters.len(), 2, "sanity: canned exchangeInfo should parse to 2 symbols");

        let store = Arc::new(vike_data::MemHistStore::new());
        let rec = Some(Arc::new(vike_data::PropertiesRecorder::new(store.clone(), true)));
        super::record_properties_all(&rec, filters.clone(), TS_NS);

        for (symbol, f) in &filters {
            assert_eq!(
                store.scan_symbol_properties("binance", symbol, vike_data::TsRange::all()).unwrap(),
                vec![(1_577_836_800_000i64, *f)],
                "symbol {symbol} should have one recorded row"
            );
        }
    }

    #[test]
    fn binance_perp_records_all_properties() {
        let payload = serde_json::json!({
            "symbols": [
                {
                    "symbol": "BTCUSDT",
                    "baseAsset": "BTC",
                    "filters": [
                        {"filterType": "PRICE_FILTER", "tickSize": "0.10"},
                        {"filterType": "LOT_SIZE", "stepSize": "0.001", "minQty": "0.001"},
                        {"filterType": "MARKET_LOT_SIZE", "maxQty": "1000.0"},
                        {"filterType": "MIN_NOTIONAL", "notional": "5.0"},
                    ]
                },
                {
                    "symbol": "ETHUSDT",
                    "baseAsset": "ETH",
                    "filters": [
                        {"filterType": "PRICE_FILTER", "tickSize": "0.01"},
                        {"filterType": "LOT_SIZE", "stepSize": "0.01", "minQty": "0.01"},
                        {"filterType": "MARKET_LOT_SIZE", "maxQty": "500.0"},
                        {"filterType": "MIN_NOTIONAL", "notional": "10.0"},
                    ]
                },
            ]
        });
        let instruments = parse_binance_perp_instruments(&payload);
        assert_eq!(instruments.len(), 2, "sanity: canned exchangeInfo should parse to 2 symbols");

        let entries: IndexMap<String, SymbolProperties> =
            instruments.iter().map(|(sym, inst)| (sym.clone(), inst.properties)).collect();

        let store = Arc::new(vike_data::MemHistStore::new());
        let rec = Some(Arc::new(vike_data::PropertiesRecorder::new(store.clone(), true)));
        super::record_properties_all(&rec, entries.clone(), TS_NS);

        for (symbol, f) in &entries {
            assert_eq!(
                store.scan_symbol_properties("binance", symbol, vike_data::TsRange::all()).unwrap(),
                vec![(1_577_836_800_000i64, *f)],
                "symbol {symbol} should have one recorded row"
            );
        }
    }

    /// The `/api/v3` parser tags every row `CryptoSpot` — the endpoint IS the venue's answer, and
    /// recording it is 0061 STEP 2's whole point: the class reaches the PIT tape as data instead of
    /// being re-derived downstream from the symbol text.
    #[test]
    fn binance_spot_properties_name_the_spot_class() {
        let payload = serde_json::json!({
            "symbols": [{"symbol": "BTCUSDT", "filters": []}]
        });
        assert_eq!(
            parse_symbol_properties(&payload)["BTCUSDT"].asset_class,
            Some(vike_model::AssetClass::CryptoSpot)
        );
    }

    /// The `/fapi` parser reads the VENUE'S `contractType` per entry rather than assuming its own
    /// endpoint is perp-only — `/fapi/v1/exchangeInfo` lists dated futures beside the perpetuals,
    /// so a blanket `CryptoPerp` would be a wrong answer for part of its own output. The class is
    /// the one the picker's typed `Contract` projects (`family::catalog::fapi_contract`), driven
    /// here over a slice of the REAL list (fetched 2026-10-11). An entry with NO `contractType`,
    /// or a word nobody measured, stays unclassified: this parser never guesses from the symbol
    /// text, which is the implicit encoding `docs/decisions/0061-an-instrument-names-its-kind.md`
    /// removes.
    #[test]
    fn binance_perp_properties_take_the_class_from_contract_type() {
        use vike_model::AssetClass;
        let mut payload: serde_json::Value = serde_json::from_str(include_str!(
            "../tests/fixtures/exchange_info/fapi_2026-10-11.json"
        ))
        .expect("fixture parses");
        let rows = payload["symbols"].as_array_mut().expect("symbols[]");
        // No `PERPETUAL_DELIVERING` row is live today: derive one from the real BTCUSDT row.
        let mut delivering = rows[0].clone();
        delivering["symbol"] = "ETHUSDT_DELIV".into();
        delivering["contractType"] = "PERPETUAL_DELIVERING".into();
        delivering["deliveryDate"] = 1_798_185_600_000_i64.into();
        let mut untyped = rows[0].clone();
        untyped["symbol"] = "NOTYPE".into();
        untyped.as_object_mut().expect("row").remove("contractType");
        let mut unmeasured = rows[0].clone();
        unmeasured["symbol"] = "NEWWORD".into();
        unmeasured["contractType"] = "NEXT_MONTH".into();
        rows.extend([delivering, untyped, unmeasured]);

        let got = parse_binance_perp_instruments(&payload);
        let class = |s: &str| got[s].properties.asset_class;
        assert_eq!(class("BTCUSDT"), Some(AssetClass::CryptoPerp));
        for dated in ["BTCUSDT_261225", "BTCUSDT_270326"] {
            assert_eq!(
                class(dated),
                Some(AssetClass::CryptoFuture),
                "a dated contract on the perp endpoint is not a perp"
            );
        }
        assert_eq!(
            class("XAUUSDT"),
            Some(AssetClass::CryptoPerp),
            "TRADIFI_PERPETUAL is a perpetual, not the dated arm of a binary word"
        );
        assert_eq!(
            class("ETHUSDT_DELIV"),
            Some(AssetClass::CryptoPerp),
            "a perpetual scheduled for delivery is still a perp (its date is `settling_ms`)"
        );
        assert_eq!(class("NOTYPE"), None, "no venue answer → no class, never a guess");
        assert_eq!(class("NEWWORD"), None, "an unmeasured word names nothing");
        // The grid the order path rounds to, off the real row.
        let btc = got["BTCUSDT"].properties;
        assert_eq!(
            (btc.tick_size, btc.step_size, btc.min_qty, btc.max_qty, btc.min_notional),
            (0.1, 0.001, 0.001, 120.0, 50.0)
        );
    }

    #[test]
    fn binance_no_recorder_writes_nothing() {
        let store = Arc::new(vike_data::MemHistStore::new());
        let entries: IndexMap<String, SymbolProperties> =
            [("BTCUSDT".to_string(), SymbolProperties::default())].into_iter().collect();
        super::record_properties_all(&None, entries, TS_NS);
        assert!(
            store
                .scan_symbol_properties("binance", "BTCUSDT", vike_data::TsRange::all())
                .unwrap()
                .is_empty()
        );
    }
}
