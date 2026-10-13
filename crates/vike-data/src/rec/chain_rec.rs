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
//! (the new bucket id has simply never been committed). Over-recording by at most one snapshot per
//! cadence change is the intended trade — the alternative (persisting the cadence) would make an
//! operator's knob silently inert.
//!
//! ⚠ **It can DROP a snapshot in one narrow case, and this doc said it never could until that case
//! was built.** A snapshot is skipped when its key equals one already committed for the same
//! `(venue, underlying, expiry)`, and the key does not name the cadence: `b * c1` and `b * c2` are
//! `b * (c2 - c1)` milliseconds apart, so two cadences a fraction of a percent apart reuse bucket
//! numbers for instants DAYS apart (3 600 s against 3 601 s: about 5.7 days at present-day
//! timestamps), and an expiry polled across both instants loses the later snapshot. Cadences an
//! ordinary ratio apart put their buckets at different magnitudes and never meet.
//! `crates/vike-store/src/chain_rec_tests.rs`'s
//! `a_cadence_change_can_add_a_snapshot_and_never_swallows_one_at_ordinary_ratios` and
//! `a_near_equal_cadence_can_swallow_a_snapshot_written_days_earlier` are the measurements. The
//! practical rule is the operator page's: choose the cadence once.
//!
//! **The production mount is the data daemon** (`docs/decisions/0084`: only the datahub touches
//! the store). With the `flags.record_chains` row on (and a maintenance scheduler compacting the
//! store, without which the mount refuses the recorder),
//! `crates/vike-datahub/src/datahub_cli/mounts.rs`'s `mount_options_lane` builds a recorder with
//! [`ChainRecorder::with_cadence`] over the store handle the daemon SERVES, at the
//! `config.datahub_chain_cadence_secs` row in milliseconds (`vike-datahub`'s `chain_cadence_ms`),
//! which is [`DEFAULT_CHAIN_CADENCE_MS`] for a box that wrote none. The options lane's Deribit poll
//! threads the recorder into
//! `vike_deribit::chain::DeribitOptionsProvider::with_chain_recorder`, so every polled chain lands
//! here. `vike_config::CONSUMPTION`'s row for that key is the gated authority on who reads
//! it; this doc is deliberately not a second one. The environment doors this module used to carry
//! (`from_env`, `open_from_env`, and the map halves keyed by the variable names) are deleted, and
//! both variables refuse startup (decision 0111).
//!
//! There is no store-OPENING constructor any more: `ChainRecorder::open` opened its own
//! `DataFusionHist` at a caller-resolved root, had no caller once the daemon took the mount, and was
//! deleted — a second store opened at a root the daemon already serves is what decision 0084
//! exists to prevent. A recorder writes through a handle its root already holds.

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
