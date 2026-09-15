//! Opt-in, best-effort recorder that persists observed `SymbolProperties` into the `kind=properties`
//! PIT series (see the PIT symbol-properties design). Capture is gated on `VIKE_RECORD_PROPERTIES=1`;
//! disabled is a no-op. A store error is logged and dropped — recording MUST never fail or block a
//! venue's startup. Called once per venue at instrument-fetch time, never on the hot path.

use std::sync::Arc;

use vike_model::SymbolProperties;

use crate::hist::HistStore;

/// Env var that enables properties recording.
pub const RECORD_PROPERTIES_ENV: &str = "VIKE_RECORD_PROPERTIES";

pub struct PropertiesRecorder {
    store: Arc<dyn HistStore + Send + Sync>,
    enabled: bool,
}

impl PropertiesRecorder {
    pub fn new(store: Arc<dyn HistStore + Send + Sync>, enabled: bool) -> Self {
        Self { store, enabled }
    }

    /// Enabled iff `VIKE_RECORD_PROPERTIES == "1"`.
    pub fn from_vars(
        store: Arc<dyn HistStore + Send + Sync>,
        vars: &std::collections::HashMap<String, String>,
    ) -> Self {
        Self { store, enabled: Self::gate(vars) }
    }

    /// The exact-`"1"` gate over a caller-supplied map — the PURE half both [`Self::from_vars`]
    /// and [`Self::open_from_vars`] share, and the only shape a test can drive now that edition
    /// 2024 has made `std::env::set_var` an `unsafe fn` this workspace forbids (the sibling
    /// `crates/vike-data/src/chain_rec.rs`'s `from_vars` carries the same argument).
    fn gate(vars: &std::collections::HashMap<String, String>) -> bool {
        vars.get(RECORD_PROPERTIES_ENV).map(String::as_str) == Some("1")
    }

    /// This recorder's one variable, read from the process env exactly once — as ONE explicit
    /// `env::var(CONST)` call, deliberately: the settings-registry scanner resolves a read by call
    /// site, and a bulk `env::vars()` sweep would leave the declared row stale.
    fn env_snapshot() -> std::collections::HashMap<String, String> {
        std::env::var(RECORD_PROPERTIES_ENV)
            .ok()
            .map(|v| (RECORD_PROPERTIES_ENV.to_string(), v))
            .into_iter()
            .collect()
    }

    /// The impure wrapper over [`Self::from_vars`] — the process env, read once.
    pub fn from_env(store: Arc<dyn HistStore + Send + Sync>) -> Self {
        Self::from_vars(store, &Self::env_snapshot())
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// Open the opt-in PIT-`SymbolProperties` recorder straight from the process environment,
    /// best-effort — the ONE env-gate site the live-exec composition root (`vike-mount`) routes
    /// through instead of hand-rolling the gate+open+wrap in every venue arm.
    ///
    /// Returns `None` (recording disabled) UNLESS `VIKE_RECORD_PROPERTIES == "1"` — the exact
    /// string `"1"`, matching the repo's feature-toggle convention (not a fuzzy truthy parse). When
    /// enabled it opens a [`crate::DataFusionHist`] over `tick_store_root` and wraps it in an
    /// enabled [`PropertiesRecorder`]. A store-open failure is logged and folds to `None` —
    /// recording MUST never fail or block a venue's startup.
    ///
    /// When the env var is unset/not `"1"` this does ZERO work and touches no store (no store open /
    /// WAL recovery / tokio runtime), so the default (off) path is byte-identical to constructing no
    /// recorder at all. Gated on `hist-datafusion` because it names the DataFusion backend; the
    /// recorder type itself stays feature-free.
    #[cfg(feature = "hist-datafusion")]
    pub fn open_from_vars(
        tick_store_root: &std::path::Path,
        vars: &std::collections::HashMap<String, String>,
    ) -> Option<Arc<PropertiesRecorder>> {
        if !Self::gate(vars) {
            return None;
        }
        // A second handle over the same root is safe (per-series locks). On open failure the venue
        // just doesn't record (previously the error was silently `.ok()`-dropped at each call site;
        // logging it here is the best-effort unification, and only ever fires on the enabled path).
        match crate::datafusion_hist::DataFusionHist::open(tick_store_root) {
            Ok(store) => Some(Arc::new(PropertiesRecorder::new(Arc::new(store), true))),
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "properties store open failed; recording disabled for this venue"
                );
                None
            }
        }
    }

    /// The impure wrapper over [`Self::open_from_vars`] — the process env, read once.
    #[cfg(feature = "hist-datafusion")]
    pub fn open_from_env(tick_store_root: &std::path::Path) -> Option<Arc<PropertiesRecorder>> {
        Self::open_from_vars(tick_store_root, &Self::env_snapshot())
    }

    /// Record one symbol's properties. `ts_ns` is the observation wall-clock (nanoseconds); the row ts
    /// and the per-day commit_key derive from it. No-op when disabled; store errors are logged.
    pub fn record(&self, venue: &str, symbol: &str, properties: SymbolProperties, ts_ns: i64) {
        if !self.enabled {
            return;
        }
        let ts_ms = ts_ns / 1_000_000;
        let date = vike_model::time::epoch_ms_to_utc_date(ts_ms);
        let key = format!("{venue}:{symbol}:{date}");
        if let Err(e) =
            self.store.append_symbol_properties(venue, symbol, &[(ts_ms, properties)], Some(&key))
        {
            tracing::warn!(venue, symbol, error = %e, "properties recording failed (dropped)");
        }
    }

    /// Best-effort record through an `Option<Arc<Self>>` handle — the shape every venue adapter holds
    /// (a recorder is threaded in only under `VIKE_RECORD_PROPERTIES=1`). Dedupes the identical
    /// `if let Some(r) = rec { r.record(..) }` wrapper each bridge (bybit/okx/deribit) hand-rolled;
    /// bridge-core can't host it (it can't depend on vike-data), so the helper lives with the type.
    pub fn record_opt(
        rec: &Option<Arc<Self>>,
        venue: &str,
        symbol: &str,
        properties: SymbolProperties,
        ts_ns: i64,
    ) {
        if let Some(r) = rec {
            r.record(venue, symbol, properties, ts_ns);
        }
    }

    pub fn record_all(
        &self,
        venue: &str,
        entries: impl IntoIterator<Item = (String, SymbolProperties)>,
        ts_ns: i64,
    ) {
        if !self.enabled {
            return;
        }
        for (symbol, properties) in entries {
            self.record(venue, &symbol, properties, ts_ns);
        }
    }
}

// The tests construct a `DataFusionHist`, so they need the backend feature; the recorder itself
// does not.
#[cfg(all(test, feature = "hist-datafusion"))]
mod tests {
    use super::*;
    use crate::datafusion_hist::DataFusionHist;
    use crate::hist::{HistStore, TsRange};
    use std::sync::Arc;

    fn tmp() -> (Arc<dyn HistStore + Send + Sync>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(DataFusionHist::open(dir.path()).unwrap());
        (store as Arc<dyn HistStore + Send + Sync>, dir)
    }

    const NS_2020_01_01: i64 = 1_577_836_800_000_000_000; // 2020-01-01T00:00:00Z in ns

    #[test]
    fn disabled_recorder_writes_nothing() {
        let (store, _t) = tmp();
        let rec = PropertiesRecorder::new(store.clone(), false);
        rec.record(
            "bybit",
            "BTCUSDT",
            SymbolProperties { tick_size: 0.1, ..Default::default() },
            NS_2020_01_01,
        );
        assert!(
            store.scan_symbol_properties("bybit", "BTCUSDT", TsRange::all()).unwrap().is_empty()
        );
    }

    #[test]
    fn enabled_recorder_writes_one_row_at_ms() {
        let (store, _t) = tmp();
        let rec = PropertiesRecorder::new(store.clone(), true);
        let f = SymbolProperties {
            tick_size: 0.1,
            step_size: 0.01,
            min_qty: 0.01,
            max_qty: 0.0,
            min_notional: 5.0,
            contract_size: 0.0,
            tick_scheme: None,
            taker_hold_ms: 0,
        };
        rec.record("bybit", "BTCUSDT", f, NS_2020_01_01);
        let got = store.scan_symbol_properties("bybit", "BTCUSDT", TsRange::all()).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].1, f);
        assert_eq!(got[0].0, NS_2020_01_01 / 1_000_000, "ts stored in ms");
    }

    /// The recorder round-trip for a grid that HAS a contract size (the deribit case): it must
    /// survive record → parquet → scan, since `make_engine`'s multiplier grid and the PIT
    /// `properties_as_of` replay both read back through this path.
    #[test]
    fn recorder_round_trips_a_contract_size() {
        let (store, _t) = tmp();
        let rec = PropertiesRecorder::new(store.clone(), true);
        let f = SymbolProperties {
            tick_size: 0.0005,
            step_size: 0.1,
            min_qty: 0.1,
            max_qty: 0.0,
            min_notional: 0.0,
            contract_size: 10.0,
            tick_scheme: None,
            taker_hold_ms: 0,
        };
        rec.record("deribit", "BTC-PERPETUAL", f, NS_2020_01_01);
        let got = store.scan_symbol_properties("deribit", "BTC-PERPETUAL", TsRange::all()).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].1, f, "the whole grid, contract_size included");
        assert_eq!(got[0].1.contract_size, 10.0);
        assert_eq!(got[0].1.multiplier(), 10.0);
    }

    /// `open_from_env` gates on the EXACT string `"1"` and folds every other value (incl. unset) to
    /// `None`. One self-contained test over an injected map, touching no process state at all.
    #[test]
    fn open_from_vars_gates_on_exact_1() {
        // Driven through the INJECTED constructor over synthetic maps, never the process env:
        // edition 2024 made `std::env::set_var` an `unsafe fn` this workspace forbids.
        // `open_from_env` is a one-line wrapper over this and carries no logic of its own.
        let dir = tempfile::tempdir().unwrap();
        let vars = |v: Option<&str>| -> std::collections::HashMap<String, String> {
            v.map(|v| (RECORD_PROPERTIES_ENV.to_string(), v.to_string())).into_iter().collect()
        };

        assert!(
            PropertiesRecorder::open_from_vars(dir.path(), &vars(None)).is_none(),
            "unset → None"
        );

        let rec =
            PropertiesRecorder::open_from_vars(dir.path(), &vars(Some("1"))).expect("\"1\" → Some");
        assert!(rec.enabled(), "enabled recorder when the value is exactly \"1\"");

        assert!(
            PropertiesRecorder::open_from_vars(dir.path(), &vars(Some("true"))).is_none(),
            "not exactly \"1\" → None (no fuzzy truthy parse)"
        );
    }

    /// `record_opt` forwards through a `Some` handle and is a total no-op on `None` — the exact
    /// contract the bybit/okx/deribit adapters depend on (they only hold `Option<Arc<Self>>`).
    #[test]
    fn record_opt_forwards_some_and_noops_none() {
        let (store, _t) = tmp();
        let f = SymbolProperties { tick_size: 0.1, ..Default::default() };

        // None → nothing written.
        PropertiesRecorder::record_opt(&None, "bybit", "BTCUSDT", f, NS_2020_01_01);
        assert!(
            store.scan_symbol_properties("bybit", "BTCUSDT", TsRange::all()).unwrap().is_empty()
        );

        // Some(enabled) → one row, venue/symbol routed through.
        let rec = Some(Arc::new(PropertiesRecorder::new(store.clone(), true)));
        PropertiesRecorder::record_opt(&rec, "okx", "ETH-USDT-SWAP", f, NS_2020_01_01);
        let got = store.scan_symbol_properties("okx", "ETH-USDT-SWAP", TsRange::all()).unwrap();
        assert_eq!(got, vec![(NS_2020_01_01 / 1_000_000, f)]);
    }

    #[test]
    fn record_all_one_row_per_entry_and_daily_idempotent() {
        let (store, _t) = tmp();
        let rec = PropertiesRecorder::new(store.clone(), true);
        let f = SymbolProperties { tick_size: 0.1, ..Default::default() };
        rec.record_all(
            "okx",
            [("BTC-USDT-SWAP".to_string(), f), ("ETH-USDT-SWAP".to_string(), f)],
            NS_2020_01_01,
        );
        rec.record_all(
            "okx",
            [("BTC-USDT-SWAP".to_string(), f)],
            NS_2020_01_01 + 3_600_000_000_000, // +1h same day
        );
        assert_eq!(
            store.scan_symbol_properties("okx", "BTC-USDT-SWAP", TsRange::all()).unwrap().len(),
            1
        );
        assert_eq!(
            store.scan_symbol_properties("okx", "ETH-USDT-SWAP", TsRange::all()).unwrap().len(),
            1
        );
    }
}
