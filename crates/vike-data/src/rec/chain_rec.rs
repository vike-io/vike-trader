//! Opt-in, best-effort recorder that persists observed option-chain snapshots into the
//! `kind=chain` PIT series (the [`crate::PropertiesRecorder`] twin for the options surface).
//! Capture is gated by the `enabled` PARAMETER — a root hands in the `flags.record_chains` row;
//! this crate reads no environment — and disabled is a no-op. A store error is logged and
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
//! Cadence-change caveat: the bucket is `ts_ms.div_euclid(cadence_ms)`, so changing the cadence a
//! root passes between runs RE-NUMBERS the buckets. A restart with a different
//! cadence can therefore land one extra snapshot inside a wall-clock period that already had one
//! (the new bucket id has simply never been committed). It can never DROP a wanted snapshot: a
//! collision requires the freshly-computed key to already exist, and the recorder only ever skips a
//! key it wrote itself for that exact `(venue, underlying, expiry)`. Over-recording by at most one
//! snapshot per cadence change is the intended trade — the alternative (persisting the cadence) would
//! make an operator's knob silently inert.
//!
//! ⚠ **NO PRODUCTION WIRING.** The desktop's local market-data plane that once opened this
//! recorder is gone — `crates/vike-desktop/src/main.rs` binds its `chain_rec` to `None` — and
//! `ChainRecorder::open` has no call site anywhere in the tree, so the `flags.record_chains` row
//! records nothing in any binary. `vike_config::CONSUMPTION`'s row for that key is the gated
//! authority (a `Reader::Uncalled` naming `ChainRecorder::open`, so the day it gains a caller the
//! row goes red); this doc is deliberately not a second one. The environment doors this module
//! used to carry (`from_env`, `open_from_env`, and the map halves keyed by the variable names) are
//! deleted, and both variables refuse startup (decision 0111).
//!
//! What `ChainRecorder::open` still IS, for whoever mounts this next: the app-root shape — it
//! opens the `DataFusionHist` at the caller-resolved store root ONLY when `enabled` is true (so a
//! disabled startup never touches the store) and hands back a shareable `Arc`. Consumers reach the
//! Deribit options provider via `with_chain_recorder`, the same shape `vike-mount` uses to reach
//! venues with [`crate::PropertiesRecorder`], with the store open living HERE (one CI-tested
//! helper) instead of being re-inlined per call site. The recorder, its idempotency rule and its
//! `kind=chain` series are all intact; only the mount is missing.

use std::sync::Arc;

use crate::chain_log::ChainRow;
use crate::store::hist::HistStore;

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

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// The resolved idempotency-bucket cadence in ms (post-clamp — see [`Self::with_cadence`]).
    pub fn cadence_ms(&self) -> i64 {
        self.cadence_ms
    }

    /// App-root constructor: `Some(recorder)` iff `enabled` (the `flags.record_chains` row the root
    /// read) AND the `kind=chain`-capable store at `root` opens; `None` otherwise. The gate is
    /// checked FIRST, so a disabled startup never opens (or creates) the store — the disabled path
    /// stays byte-identical to pre-recording behavior. An open failure is logged and degrades to
    /// `None` (recording is best-effort; it must never keep the app from starting). `cadence_ms`
    /// is [`Self::with_cadence`]'s ([`DEFAULT_CHAIN_CADENCE_MS`] unless the root has a reason).
    ///
    /// Lives here (rather than inlined at the call site) so the store-open + gate pair is one
    /// CI-tested unit; the binary keeps only the root-path resolution, which is its own concern.
    #[cfg(feature = "hist-datafusion")]
    pub fn open(
        root: impl AsRef<std::path::Path>,
        enabled: bool,
        cadence_ms: i64,
    ) -> Option<Arc<Self>> {
        if !enabled {
            return None;
        }
        let root = root.as_ref();
        match crate::store::datafusion_hist::DataFusionHist::open(root) {
            Ok(store) => {
                let rec = Self::with_cadence(Arc::new(store), true, cadence_ms);
                tracing::info!(
                    root = %root.display(),
                    cadence_ms = rec.cadence_ms,
                    "option-chain snapshot recording ENABLED"
                );
                Some(Arc::new(rec))
            }
            Err(e) => {
                tracing::warn!(
                    root = %root.display(),
                    error = %e,
                    "chain recording is on but the chain store failed to open; recording disabled"
                );
                None
            }
        }
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
    use crate::store::datafusion_hist::DataFusionHist;
    use crate::store::hist::{HistStore, TsRange};
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

    /// The bucketing arithmetic itself, driven through the explicit-cadence constructor. Guards the
    /// interaction the review called out: the non-positive→1ms clamp feeding `div_euclid`.
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

    /// `open`, the app-root constructor (the store hop a mounting root would take): the gate is a
    /// PARAMETER, checked before the store is touched, and the cadence is the caller's.
    #[test]
    fn open_gates_on_its_parameter_and_keeps_the_cadence() {
        let dir = tempfile::tempdir().unwrap();
        let unused = dir.path().join("gate-off");
        assert!(ChainRecorder::open(&unused, false, 2_500).is_none(), "gate off → None");
        assert!(!unused.exists(), "and the store is never even opened/created when disabled");

        let root = dir.path().join("on");
        let rec = ChainRecorder::open(&root, true, 2_500).expect("gate on + openable → Some");
        assert!(rec.enabled() && rec.cadence_ms() == 2_500, "enabled, the caller's cadence");
        // and it writes to the store it just opened at that root
        rec.record("deribit", "BTC", &[row(1, "BTC-X-1-C", 1, 1.0, true)], NS_2020_01_01);
        let reopened = DataFusionHist::open(&root).unwrap();
        assert_eq!(
            reopened.scan_chain("deribit", "BTC", TsRange::all()).unwrap().len(),
            1,
            "open's recorder persists to `root`"
        );
        // A non-positive cadence clamps exactly as `with_cadence` does.
        let clamped = ChainRecorder::open(dir.path().join("clamp"), true, 0).expect("opens");
        assert_eq!(clamped.cadence_ms(), 1, "0 clamps to 1ms");
    }
}
