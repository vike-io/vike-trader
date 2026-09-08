//! Opt-in, best-effort recorder that persists observed option-chain snapshots into the
//! `kind=chain` PIT series (the [`crate::PropertiesRecorder`] twin for the options surface).
//! Capture is gated on `VIKE_RECORD_CHAINS=1`; disabled is a no-op. A store error is logged and
//! dropped — recording MUST never fail or block the caller (a chain fetch is a UI/strategy read
//! path, never allowed to stall on the store). Called at chain-fetch time, never on the hot path.
//!
//! Idempotency: the store-level `commit_key` is the guard (exactly like `PropertiesRecorder`'s
//! per-UTC-day key) — one key per `(venue, underlying, expiry, snapshot-ts bucket)`, where the
//! bucket is `ts_ms` rounded DOWN to a configurable cadence (default 1 minute). Re-recording the
//! same chain within one bucket is a store no-op, so a 5s-refresh UI loop lands at most one
//! snapshot per expiry per minute. The key includes the chain's `expiry_ms` (read from the first
//! row) BECAUSE one recorded chain covers ONE expiry: a full-surface pass records N chains back to
//! back in the same bucket, and a `(venue, underlying, bucket)`-only key would drop all but the
//! first. Callers therefore pass one expiry's rows per `record` call (the natural unit — e.g. one
//! `vike-deribit` `fetch_chain` result).
//!
//! Cadence-change caveat: the bucket is `ts_ms.div_euclid(cadence_ms)`, so changing
//! `VIKE_RECORD_CHAINS_CADENCE_MS` between runs RE-NUMBERS the buckets. A restart with a different
//! cadence can therefore land one extra snapshot inside a wall-clock period that already had one
//! (the new bucket id has simply never been committed). It can never DROP a wanted snapshot: a
//! collision requires the freshly-computed key to already exist, and the recorder only ever skips a
//! key it wrote itself for that exact `(venue, underlying, expiry)`. Over-recording by at most one
//! snapshot per cadence change is the intended trade — the alternative (persisting the cadence) would
//! make an operator's knob silently inert.
//!
//! PRODUCTION WIRING: [`ChainRecorder::open_from_env`] is the app-root constructor — it opens the
//! `DataFusionHist` at the caller-resolved store root ONLY when the env gate is on (so a disabled
//! startup never touches the store) and hands back a shareable `Arc`. `vike-app` calls it with its
//! `tick_store_root()` and threads the result into `vike_app_core::tools::spawn_tool_fetchers`,
//! which gives it to the Deribit options provider via `with_chain_recorder`. That is the same
//! shape `vike-mount` uses to reach venues with [`crate::PropertiesRecorder`], except the store
//! open lives HERE (one CI-tested helper) instead of being re-inlined per call site.

use std::sync::Arc;

use crate::chain_log::ChainRow;
use crate::hist::HistStore;

/// Env var that enables chain recording.
pub const RECORD_CHAINS_ENV: &str = "VIKE_RECORD_CHAINS";

/// Env var overriding the idempotency-bucket cadence in ms (parsed by [`ChainRecorder::from_env`];
/// unset/unparsable → [`DEFAULT_CHAIN_CADENCE_MS`]).
pub const RECORD_CHAINS_CADENCE_ENV: &str = "VIKE_RECORD_CHAINS_CADENCE_MS";

/// Default idempotency-bucket cadence: at most one snapshot per (underlying, expiry) per minute.
pub const DEFAULT_CHAIN_CADENCE_MS: i64 = 60_000;

pub struct ChainRecorder {
    store: Arc<dyn HistStore + Send + Sync>,
    enabled: bool,
    cadence_ms: i64,
}

impl ChainRecorder {
    /// Default cadence ([`DEFAULT_CHAIN_CADENCE_MS`]).
    pub fn new(store: Arc<dyn HistStore + Send + Sync>, enabled: bool) -> Self {
        Self::with_cadence(store, enabled, DEFAULT_CHAIN_CADENCE_MS)
    }

    /// Explicit cadence (non-positive is clamped to 1 ms — every snapshot in its own bucket).
    pub fn with_cadence(
        store: Arc<dyn HistStore + Send + Sync>,
        enabled: bool,
        cadence_ms: i64,
    ) -> Self {
        Self { store, enabled, cadence_ms: cadence_ms.max(1) }
    }

    /// Enabled iff `VIKE_RECORD_CHAINS == "1"` (the exact string, like the sibling recorder
    /// gates); cadence from `VIKE_RECORD_CHAINS_CADENCE_MS` when set and parsable, else 1 minute.
    ///
    /// The PURE half, over a caller-supplied map — the `Injected` shape
    /// `crates/vike-ops/tests/settings_registry.rs` names as the target state, and now the only
    /// shape a test can drive at all: edition 2024 made `std::env::set_var` an `unsafe fn`, and
    /// this workspace's `unsafe_code = "forbid"` cannot be lifted by an `#[allow]`. Determinism
    /// under parallel test threads was always the better reason.
    pub fn from_vars(
        store: Arc<dyn HistStore + Send + Sync>,
        vars: &std::collections::HashMap<String, String>,
    ) -> Self {
        let enabled = vars.get(RECORD_CHAINS_ENV).map(String::as_str) == Some("1");
        let cadence_ms = vars
            .get(RECORD_CHAINS_CADENCE_ENV)
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(DEFAULT_CHAIN_CADENCE_MS);
        Self::with_cadence(store, enabled, cadence_ms)
    }

    /// This recorder's two variables, read from the process env exactly once.
    ///
    /// ⚠ Two explicit `env::var(CONST)` calls rather than a bulk `env::vars()` sweep, deliberately:
    /// the settings-registry scanner resolves a read BY CALL SITE, so a sweep is a computed key it
    /// cannot see and both declared rows would go stale.
    fn env_snapshot() -> std::collections::HashMap<String, String> {
        let mut vars = std::collections::HashMap::new();
        if let Ok(v) = std::env::var(RECORD_CHAINS_ENV) {
            vars.insert(RECORD_CHAINS_ENV.to_string(), v);
        }
        if let Ok(v) = std::env::var(RECORD_CHAINS_CADENCE_ENV) {
            vars.insert(RECORD_CHAINS_CADENCE_ENV.to_string(), v);
        }
        vars
    }

    /// The impure wrapper over [`Self::from_vars`] — the process env, read once.
    pub fn from_env(store: Arc<dyn HistStore + Send + Sync>) -> Self {
        Self::from_vars(store, &Self::env_snapshot())
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// The resolved idempotency-bucket cadence in ms (post-clamp — see [`Self::with_cadence`]).
    pub fn cadence_ms(&self) -> i64 {
        self.cadence_ms
    }

    /// App-root constructor: `Some(recorder)` iff `VIKE_RECORD_CHAINS=1` AND the `kind=chain`-
    /// capable store at `root` opens; `None` otherwise. The env gate is checked FIRST, so a
    /// disabled startup never opens (or creates) the store — the disabled path stays byte-identical
    /// to pre-recording behavior, exactly like `vike-mount`'s `PropertiesRecorder` arms. An open
    /// failure is logged and degrades to `None` (recording is best-effort; it must never keep the
    /// app from starting). Cadence comes from [`RECORD_CHAINS_CADENCE_ENV`] via
    /// [`Self::from_env`].
    ///
    /// Lives here (rather than inlined at the call site) so the store-open + env-gate pair is one
    /// CI-tested unit; the binary keeps only the root-path resolution, which is its own concern.
    #[cfg(feature = "hist-datafusion")]
    pub fn open_from_vars(
        root: impl AsRef<std::path::Path>,
        vars: &std::collections::HashMap<String, String>,
    ) -> Option<Arc<Self>> {
        if vars.get(RECORD_CHAINS_ENV).map(String::as_str) != Some("1") {
            return None;
        }
        let root = root.as_ref();
        match crate::datafusion_hist::DataFusionHist::open(root) {
            Ok(store) => {
                let rec = Self::from_vars(Arc::new(store), vars);
                tracing::info!(
                    root = %root.display(),
                    cadence_ms = rec.cadence_ms,
                    "VIKE_RECORD_CHAINS=1 → option-chain snapshot recording ENABLED"
                );
                Some(Arc::new(rec))
            }
            Err(e) => {
                tracing::warn!(
                    root = %root.display(),
                    error = %e,
                    "VIKE_RECORD_CHAINS=1 but the chain store failed to open; recording disabled"
                );
                None
            }
        }
    }

    /// The impure wrapper over [`Self::open_from_vars`] — the process env, read once.
    #[cfg(feature = "hist-datafusion")]
    pub fn open_from_env(root: impl AsRef<std::path::Path>) -> Option<Arc<Self>> {
        Self::open_from_vars(root, &Self::env_snapshot())
    }

    /// Record one chain snapshot (ONE expiry's rows — see the module doc). `ts_ns` is the
    /// observation wall-clock (nanoseconds, matching [`crate::PropertiesRecorder::record`]); the
    /// idempotency bucket derives from it, while each row keeps its own `ts` (the chain's
    /// `asof_ms`, caller-set). No-op when disabled or `rows` is empty; store errors are logged and
    /// dropped.
    pub fn record(&self, venue: &str, underlying: &str, rows: &[ChainRow], ts_ns: i64) {
        if !self.enabled || rows.is_empty() {
            return;
        }
        let ts_ms = ts_ns / 1_000_000;
        let bucket = ts_ms.div_euclid(self.cadence_ms);
        let expiry = rows[0].expiry_ms;
        let key = format!("chain:{venue}:{underlying}:{expiry}:{bucket}");
        if let Err(e) = self.store.append_chain_snapshot(venue, underlying, rows, Some(&key)) {
            tracing::warn!(venue, underlying, error = %e, "chain recording failed (dropped)");
        }
    }
}

// The round-trip tests construct a `DataFusionHist`, so they need the backend feature; the
// recorder itself does not (it holds only `Arc<dyn HistStore>` — same shape as `properties_rec`).
#[cfg(all(test, feature = "hist-datafusion"))]
mod tests {
    use super::*;
    use crate::datafusion_hist::DataFusionHist;
    use crate::hist::{HistStore, TsRange};
    use std::sync::Arc;

    const NS_2020_01_01: i64 = 1_577_836_800_000_000_000; // 2020-01-01T00:00:00Z in ns

    fn tmp() -> (Arc<dyn HistStore + Send + Sync>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(DataFusionHist::open(dir.path()).unwrap());
        (store as Arc<dyn HistStore + Send + Sync>, dir)
    }

    fn row(ts: i64, instrument: &str, expiry_ms: i64, strike: f64, is_call: bool) -> ChainRow {
        ChainRow {
            ts,
            underlying: "BTC".into(),
            instrument: instrument.into(),
            expiry_ms,
            strike,
            is_call,
            bid: Some(1.0),
            ask: Some(2.0),
            mark: Some(1.5),
            iv: Some(0.6),
            open_interest: None,
            volume: None,
            delta: Some(0.5),
            gamma: None,
            theta: None,
            vega: None,
        }
    }

    #[test]
    fn disabled_recorder_writes_nothing() {
        let (store, _t) = tmp();
        let rec = ChainRecorder::new(store.clone(), false);
        let ts_ms = NS_2020_01_01 / 1_000_000;
        rec.record("deribit", "BTC", &[row(ts_ms, "BTC-X-100-C", 1, 100.0, true)], NS_2020_01_01);
        assert!(store.scan_chain("deribit", "BTC", TsRange::all()).unwrap().is_empty());
    }

    #[test]
    fn same_bucket_twice_is_one_snapshot() {
        let (store, _t) = tmp();
        let rec = ChainRecorder::new(store.clone(), true); // 1min cadence
        let ts_ms = NS_2020_01_01 / 1_000_000;
        let rows = vec![
            row(ts_ms, "BTC-X-100-C", 7, 100.0, true),
            row(ts_ms, "BTC-X-100-P", 7, 100.0, false),
        ];
        rec.record("deribit", "BTC", &rows, NS_2020_01_01);
        // 30s later, same minute bucket → dropped by the commit key
        rec.record("deribit", "BTC", &rows, NS_2020_01_01 + 30_000_000_000);
        assert_eq!(store.scan_chain("deribit", "BTC", TsRange::all()).unwrap(), rows);
        // next minute → a second snapshot lands
        let later = ts_ms + 60_000;
        rec.record(
            "deribit",
            "BTC",
            &[row(later, "BTC-X-100-C", 7, 100.0, true)],
            NS_2020_01_01 + 60_000_000_000,
        );
        assert_eq!(store.scan_chain("deribit", "BTC", TsRange::all()).unwrap().len(), 3);
    }

    #[test]
    fn distinct_expiries_in_one_bucket_both_land() {
        // A full-surface pass records one chain per expiry back to back — the expiry in the commit
        // key keeps the second from being swallowed by the first's bucket.
        let (store, _t) = tmp();
        let rec = ChainRecorder::new(store.clone(), true);
        let ts_ms = NS_2020_01_01 / 1_000_000;
        rec.record(
            "deribit",
            "BTC",
            &[row(ts_ms, "BTC-JUN-100-C", 100, 100.0, true)],
            NS_2020_01_01,
        );
        rec.record(
            "deribit",
            "BTC",
            &[row(ts_ms, "BTC-SEP-100-C", 200, 100.0, true)],
            NS_2020_01_01 + 1_000_000_000, // +1s: same minute bucket, different expiry
        );
        assert_eq!(store.scan_chain("deribit", "BTC", TsRange::all()).unwrap().len(), 2);
    }

    /// The bucketing arithmetic itself, driven through the explicit-cadence constructor so it needs
    /// no env var (and so cannot race the one env-owning test below). Guards the interaction the
    /// review called out: the non-positive→1ms clamp feeding `div_euclid`.
    #[test]
    fn cadence_controls_the_idempotency_bucket() {
        let ts_ms = NS_2020_01_01 / 1_000_000;
        let ns = |ms: i64| ms * 1_000_000;
        let rows = |ms: i64| vec![row(ms, "BTC-X-100-C", 7, 100.0, true)];

        // 10s cadence: +5s is the SAME bucket (dropped), +10s is the next one (lands).
        let (store, _t) = tmp();
        let rec = ChainRecorder::with_cadence(store.clone(), true, 10_000);
        assert_eq!(rec.cadence_ms(), 10_000);
        rec.record("deribit", "BTC", &rows(ts_ms), ns(ts_ms));
        rec.record("deribit", "BTC", &rows(ts_ms + 5_000), ns(ts_ms + 5_000));
        assert_eq!(store.scan_chain("deribit", "BTC", TsRange::all()).unwrap().len(), 1);
        rec.record("deribit", "BTC", &rows(ts_ms + 10_000), ns(ts_ms + 10_000));
        assert_eq!(store.scan_chain("deribit", "BTC", TsRange::all()).unwrap().len(), 2);

        // Non-positive clamps to 1ms — every distinct ms is its own bucket, nothing is deduped
        // (the clamp exists so `div_euclid` can never divide by zero).
        let (store2, _t2) = tmp();
        let rec2 = ChainRecorder::with_cadence(store2.clone(), true, 0);
        assert_eq!(rec2.cadence_ms(), 1, "non-positive cadence clamps to 1ms");
        rec2.record("deribit", "BTC", &rows(ts_ms), ns(ts_ms));
        rec2.record("deribit", "BTC", &rows(ts_ms + 1), ns(ts_ms + 1));
        assert_eq!(store2.scan_chain("deribit", "BTC", TsRange::all()).unwrap().len(), 2);
        rec2.record("deribit", "BTC", &rows(ts_ms + 1), ns(ts_ms + 1)); // same ms → same bucket
        assert_eq!(store2.scan_chain("deribit", "BTC", TsRange::all()).unwrap().len(), 2);
    }

    /// A mid-bucket restart must not re-record: commit keys live in the series manifest, so they
    /// survive dropping and re-opening the store. Proven for other kinds by the store's own tests;
    /// pinned here for `kind=chain` specifically (the recorder's whole dedupe story depends on it).
    #[test]
    fn commit_keys_survive_a_store_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let ts_ms = NS_2020_01_01 / 1_000_000;
        let rows = vec![row(ts_ms, "BTC-X-100-C", 7, 100.0, true)];
        {
            let store: Arc<dyn HistStore + Send + Sync> =
                Arc::new(DataFusionHist::open(dir.path()).unwrap());
            ChainRecorder::new(store, true).record("deribit", "BTC", &rows, NS_2020_01_01);
        } // store dropped — "app restart"
        let store: Arc<dyn HistStore + Send + Sync> =
            Arc::new(DataFusionHist::open(dir.path()).unwrap());
        // Same bucket after the restart → the manifest's committed key still wins.
        ChainRecorder::new(store.clone(), true).record("deribit", "BTC", &rows, NS_2020_01_01);
        assert_eq!(
            store.scan_chain("deribit", "BTC", TsRange::all()).unwrap(),
            rows,
            "restart inside one bucket re-records nothing"
        );
    }

    #[test]
    fn from_vars_gate_cadence_and_open_from_vars() {
        // Driven through the INJECTED constructors over synthetic maps, never the process env.
        // This test used to own VIKE_RECORD_CHAINS / VIKE_RECORD_CHAINS_CADENCE_MS through
        // `std::env::set_var`; edition 2024 made that an `unsafe fn` this workspace forbids, and a
        // process-global write was never parallel-safe under nextest anyway. `from_env` /
        // `open_from_env` are one-line wrappers over what is exercised here and carry no logic.
        let (store, _t) = tmp();
        let vars = |pairs: &[(&str, &str)]| -> std::collections::HashMap<String, String> {
            pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
        };
        let with = |pairs: &[(&str, &str)]| ChainRecorder::from_vars(store.clone(), &vars(pairs));

        assert!(!with(&[]).enabled(), "unset → disabled");
        assert!(with(&[(RECORD_CHAINS_ENV, "1")]).enabled(), "\"1\" → enabled");
        assert!(!with(&[(RECORD_CHAINS_ENV, "true")]).enabled(), "fuzzy truthy is NOT the gate");

        // ---- cadence parse/clamp (the enable gate is off here; cadence is read independently
        // ---- of it, so both halves of the parse are covered either way).
        let cadence = |v: &str| with(&[(RECORD_CHAINS_CADENCE_ENV, v)]).cadence_ms();
        assert_eq!(with(&[]).cadence_ms(), DEFAULT_CHAIN_CADENCE_MS, "unset cadence → 1 minute");
        assert_eq!(cadence("5000"), 5_000, "parsed override");
        assert_eq!(cadence("0"), 1, "0 clamps to 1ms");
        assert_eq!(cadence("-7"), 1, "negative clamps to 1ms");
        assert_eq!(
            cadence("not-a-number"),
            DEFAULT_CHAIN_CADENCE_MS,
            "unparsable falls back to the default, never panics"
        );

        // ---- open_from_vars: the app-root constructor (the production wiring's store hop) ------
        let dir = tempfile::tempdir().unwrap();
        let unused = dir.path().join("gate-off");
        assert!(ChainRecorder::open_from_vars(&unused, &vars(&[])).is_none(), "gate off → None");
        assert!(!unused.exists(), "and the store is never even opened/created when disabled");

        let root = dir.path().join("on");
        let on = vars(&[(RECORD_CHAINS_ENV, "1"), (RECORD_CHAINS_CADENCE_ENV, "2500")]);
        let rec = ChainRecorder::open_from_vars(&root, &on).expect("gate on + openable → Some");
        assert!(rec.enabled() && rec.cadence_ms() == 2_500, "enabled, cadence from the map");
        // and it writes to the store it just opened at that root
        rec.record("deribit", "BTC", &[row(1, "BTC-X-1-C", 1, 1.0, true)], NS_2020_01_01);
        let reopened = DataFusionHist::open(&root).unwrap();
        assert_eq!(
            reopened.scan_chain("deribit", "BTC", TsRange::all()).unwrap().len(),
            1,
            "open_from_vars's recorder persists to `root`"
        );

        // and the disabled recorder is a full no-op end to end
        let rec = with(&[]);
        rec.record("deribit", "BTC", &[row(1, "BTC-X-1-C", 1, 1.0, true)], NS_2020_01_01);
        assert!(store.scan_chain("deribit", "BTC", TsRange::all()).unwrap().is_empty());
    }
}
