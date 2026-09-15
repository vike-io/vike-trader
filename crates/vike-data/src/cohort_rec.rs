//! The `kind=cohort` producer: the write-side entry point a cohort fetch hands its rows to, and
//! the one place the series' commit key is BUILT.
//!
//! # Why this is a type and not a `format!` at the call site
//!
//! `kind=cohort` stores five dimensions the PATH does not carry — `axis`, `cohort`, `grading`,
//! `label_basis`, and the asset restated as a column (`crate::store_kind::STORE_KINDS`' `cohort`
//! row argues each). Batch idempotency in this store is keyed on the commit key alone, never on row
//! values, so two fetches that differ only in a dimension the key OMITS address the same series
//! under the same key and the second is a silent no-op: a `--grading unrealized` run over a window
//! already fetched as `realized` would write nothing, report nothing, and leave a reader summing
//! one grading while believing it had three.
//!
//! [`CohortFetch`] is what makes that unavailable. It names every discriminator as a field, so a
//! caller cannot reach [`CohortRecorder::record`] without supplying them, and [`CohortFetch::
//! commit_key`] is the single `format!` that turns them into the key. The alternative — each caller
//! spelling its own key — is how `crates/vike-backfill/src/clickhouse_poly/ingest.rs` and
//! `crates/vike-backfill/src/clickhouse_spot/ingest.rs` came to share the `clickhouse:` namespace on
//! `kind=quote` with only their second segment telling them apart, which the `quote` row pins as a
//! standing hazard.
//!
//! # Why it REFUSES rather than logging and dropping
//!
//! [`crate::PropertiesRecorder`] and [`crate::ChainRecorder`] are best-effort — they log a store
//! error and drop it — because both sit on a live fetch path where a store fault must never stall a
//! venue mount or a UI read. This one does the opposite and returns the error, because its caller is
//! a BATCH: a run that spent an hour paging sixty days of metrics and then dropped the write in a
//! log line is the failure those two recorders are shaped to avoid rather than an instance of it.
//! Nothing here is on a hot path, and nothing here has a mount to protect.
//!
//! There is no env gate for the same reason. A gate on a live recorder answers "should this venue
//! record?"; a gate here would answer "should this run persist the thing it was asked to fetch?",
//! and the honest answer to that is the caller's choice of whether to construct a recorder at all.
//!
//! Holds only `Arc<dyn HistStore>` (the always-available seam), so it is NOT gated on
//! `hist-datafusion` — a consumer references it in a default build without pulling DataFusion.

use std::sync::Arc;

use crate::cohort_log::CohortRow;
use crate::hist::{DataError, HistStore};

/// The identity of ONE cohort fetch: the two path segments plus the four dimensions the path cannot
/// carry, and the window the rows cover.
///
/// Every field is part of [`Self::commit_key`], and that is the type's whole job — see the module
/// doc for what a key missing one of them silently does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CohortFetch<'a> {
    /// The EXCHANGE the positions sit on (`hyperliquid`) — the series `venue`, never the metrics
    /// service that graded them.
    pub venue: &'a str,
    /// The asset (`BTC`) — the series `symbol`, and also every row's `asset` column.
    pub asset: &'a str,
    /// Which ladder was fetched: `size`, `pnl`, `tier`.
    pub axis: &'a str,
    /// Which question the ladder answered: `realized`, `realized-pit`, `unrealized`.
    pub grading: &'a str,
    /// How the label was resolved: `point_in_time`, or whatever the server echoed.
    pub label_basis: &'a str,
    /// First hour requested, epoch MILLISECONDS — the window actually SENT, not the one asked for
    /// in days. (`crates/vike-research/src/sources/api.rs`'s `AxisFetch` recorded the same
    /// distinction for the same reason: "60 days" is not a fact a later reader can check. That
    /// type is GONE — it went with the research crate and has no live twin, since the collector's
    /// window provenance is this record — so the citation is the evidence for the rule, not a file
    /// to go and read; it is filed in `crates/vike-ops/tests/citation_gate.rs`'s
    /// `DEAD_PATH_EXCEPTIONS`.)
    pub start_ms: i64,
    /// Last hour requested, epoch milliseconds, inclusive.
    pub end_ms: i64,
}

impl CohortFetch<'_> {
    /// The batch commit key for this fetch.
    ///
    /// Segment order is `(venue, asset, axis, grading, label_basis, window)` — coarsest first, so a
    /// human reading a manifest sorts into series, then into ladders, then into gradings. The
    /// `cohort:` namespace prefix is deliberate: two of the twelve kinds' producers omit one, and
    /// the `properties` row pins the near-collision that produced (its `{venue}:{symbol}:{date}`
    /// matches the klines window key segment-for-segment up to the third).
    ///
    /// ⚠ **A changed template re-admits history the store already holds.** Keys are compared as
    /// opaque strings, so re-spelling one makes every already-ingested window look fresh, and the
    /// series doubles. `crates/vike-data/tests/store_kind_gate.rs`'s
    /// `every_declared_commit_key_template_exists_in_its_producer` reddens on the edit rather than
    /// on the re-ingest, which is the only moment the change is still cheap.
    pub fn commit_key(&self) -> String {
        let CohortFetch { venue, asset, axis, grading, label_basis, start_ms, end_ms } = *self;
        format!("cohort:{venue}:{asset}:{axis}:{grading}:{label_basis}:{start_ms}-{end_ms}")
    }
}

/// Persists cohort marginals into the `kind=cohort` series.
pub struct CohortRecorder {
    store: Arc<dyn HistStore + Send + Sync>,
}

impl CohortRecorder {
    pub fn new(store: Arc<dyn HistStore + Send + Sync>) -> Self {
        Self { store }
    }

    /// Persist one fetch's rows, keyed by [`CohortFetch::commit_key`]. Returns rows written — 0
    /// when `rows` is empty or the key was already ingested.
    ///
    /// ⚠ **Every row is checked against the fetch before anything is written, and a disagreement is
    /// an ERROR, not a repair.** The four discriminators live in the key AND in every row, so the
    /// two can disagree — and a row whose `grading` column says `realized` filed under a key that
    /// says `unrealized` is the aliasing this kind's columns exist to prevent, wearing the opposite
    /// face: the key keeps the batches apart while the rows lie about which batch they came from.
    /// Overwriting the columns from the fetch would hide a caller that mixed two fetches into one
    /// batch, which is the only way this can happen. The whole batch is refused (the store never
    /// sees a partial fetch) and the message names the offending row, its index, and both sides.
    pub fn record(&self, fetch: &CohortFetch<'_>, rows: &[CohortRow]) -> Result<usize, DataError> {
        if rows.is_empty() {
            return Ok(0);
        }
        for (i, r) in rows.iter().enumerate() {
            for (what, row_value, fetch_value) in [
                ("asset", r.asset.as_str(), fetch.asset),
                ("axis", r.axis.as_str(), fetch.axis),
                ("grading", r.grading.as_str(), fetch.grading),
                ("label_basis", r.label_basis.as_str(), fetch.label_basis),
            ] {
                if row_value != fetch_value {
                    return Err(DataError::Query(format!(
                        "cohort row {i} says {what}={row_value:?}, the fetch says {fetch_value:?} \
                         — the batch would be stored under a commit key that names a different \
                         {what} than the rows carry. Refusing the whole batch: split the fetches."
                    )));
                }
            }
        }
        self.store.append_cohort(fetch.venue, fetch.asset, rows, Some(&fetch.commit_key()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hist::TsRange;
    use crate::test_support::MemHistStore;

    const VENUE: &str = "hyperliquid";
    const HOUR: i64 = 1_756_000_800_000;

    fn fetch() -> CohortFetch<'static> {
        CohortFetch {
            venue: VENUE,
            asset: "BTC",
            axis: "size",
            grading: "realized",
            label_basis: "point_in_time",
            start_ms: HOUR,
            end_ms: HOUR + 3_600_000,
        }
    }

    fn row(cohort: &str, long: f64, total: f64) -> CohortRow {
        CohortRow {
            ts: HOUR,
            asset: "BTC".into(),
            axis: "size".into(),
            cohort: cohort.into(),
            grading: "realized".into(),
            label_basis: "point_in_time".into(),
            long_usd: long,
            total_usd: total,
        }
    }

    fn store() -> (Arc<MemHistStore>, CohortRecorder) {
        let store = Arc::new(MemHistStore::new());
        let rec = CohortRecorder::new(store.clone());
        (store, rec)
    }

    #[test]
    fn a_fetch_lands_every_label_of_its_hour_under_one_key() {
        let (store, rec) = store();
        let rows = vec![row("4xWhale", 60.0, 100.0), row("Shrimp", 1.0, 3.0)];
        assert_eq!(rec.record(&fetch(), &rows).unwrap(), 2);
        let back = store.scan_cohort(VENUE, "BTC", TsRange::all()).unwrap();
        assert_eq!(back, rows, "the labels come back in write order, values intact");
    }

    /// The property the whole type exists for: the SAME window fetched under a different grading,
    /// axis or label basis must reach the store as a SEPARATE batch. If any of the three were
    /// missing from the key, the second `record` would be a no-op and the assertion below would
    /// read 2 instead of 6.
    #[test]
    fn a_regraded_fetch_of_one_window_does_not_alias_the_realized_one() {
        let (store, rec) = store();
        let realized = fetch();
        assert_eq!(rec.record(&realized, &[row("4xWhale", 60.0, 100.0)]).unwrap(), 1);

        let mut unrealized_row = row("4xWhale", 55.0, 90.0);
        unrealized_row.grading = "unrealized".into();
        let unrealized = CohortFetch { grading: "unrealized", ..realized };
        assert_ne!(unrealized.commit_key(), realized.commit_key());
        assert_eq!(rec.record(&unrealized, &[unrealized_row]).unwrap(), 1);

        let mut pnl_row = row("3xSmart", 20.0, 30.0);
        pnl_row.axis = "pnl".into();
        let pnl = CohortFetch { axis: "pnl", ..realized };
        assert_eq!(rec.record(&pnl, &[pnl_row]).unwrap(), 1);

        let mut current_row = row("4xWhale", 61.0, 101.0);
        current_row.label_basis = "current".into();
        let current = CohortFetch { label_basis: "current", ..realized };
        assert_eq!(rec.record(&current, &[current_row]).unwrap(), 1);

        assert_eq!(
            store.scan_cohort(VENUE, "BTC", TsRange::all()).unwrap().len(),
            4,
            "four fetches differing only in a NON-PATH dimension are four batches"
        );
    }

    #[test]
    fn re_recording_one_fetch_is_a_no_op_rather_than_a_second_copy() {
        let (store, rec) = store();
        let rows = vec![row("4xWhale", 60.0, 100.0)];
        assert_eq!(rec.record(&fetch(), &rows).unwrap(), 1);
        assert_eq!(rec.record(&fetch(), &rows).unwrap(), 0, "same key — batch-level no-op");
        assert_eq!(store.scan_cohort(VENUE, "BTC", TsRange::all()).unwrap().len(), 1);
    }

    /// A different WINDOW is a different key even for one grading, so a follow-on fetch appends
    /// rather than colliding with yesterday's.
    #[test]
    fn a_different_window_is_a_different_key() {
        let a = fetch();
        let b = CohortFetch { start_ms: a.start_ms + 3_600_000, ..a };
        assert_ne!(a.commit_key(), b.commit_key());
    }

    #[test]
    fn an_empty_batch_writes_nothing_and_does_not_burn_the_key() {
        let (store, rec) = store();
        assert_eq!(rec.record(&fetch(), &[]).unwrap(), 0);
        // The key must still be spendable — an empty fetch is not evidence the window was covered.
        assert_eq!(rec.record(&fetch(), &[row("4xWhale", 60.0, 100.0)]).unwrap(), 1);
        assert_eq!(store.scan_cohort(VENUE, "BTC", TsRange::all()).unwrap().len(), 1);
    }

    #[test]
    fn a_row_disagreeing_with_its_fetch_refuses_the_whole_batch() {
        let (store, rec) = store();
        let mut stray = row("3xSmart", 20.0, 30.0);
        stray.axis = "pnl".into(); // a pnl row inside a size fetch
        let rows = vec![row("4xWhale", 60.0, 100.0), stray];

        let err = rec.record(&fetch(), &rows).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("row 1"), "the message names the offending index: {msg}");
        assert!(msg.contains("axis"), "…and the dimension that disagreed: {msg}");
        assert!(msg.contains("\"pnl\"") && msg.contains("\"size\""), "…and both sides: {msg}");
        assert!(
            store.scan_cohort(VENUE, "BTC", TsRange::all()).unwrap().is_empty(),
            "the batch is refused WHOLE — the good first row must not be stored either, or the \
             fetch is half-persisted under a key that claims it is complete"
        );
    }

    /// Each of the four checked dimensions refuses on its own — a loop that only ever checked the
    /// first would pass the test above and let the other three through.
    #[test]
    fn every_checked_dimension_refuses_on_its_own() {
        let (_store, rec) = store();
        for mutate in [
            (|r: &mut CohortRow| r.asset = "ETH".into()) as fn(&mut CohortRow),
            |r: &mut CohortRow| r.axis = "tier".into(),
            |r: &mut CohortRow| r.grading = "unrealized".into(),
            |r: &mut CohortRow| r.label_basis = "current".into(),
        ] {
            let mut bad = row("4xWhale", 60.0, 100.0);
            mutate(&mut bad);
            assert!(rec.record(&fetch(), &[bad]).is_err());
        }
        // …and the label itself is NOT checked: `cohort` is the one dimension the fetch does not
        // name, because a fetch returns many labels. A taxonomy this layer does not know is data.
        let unknown = row("SomeRungInventedNextQuarter", 1.0, 2.0);
        assert!(rec.record(&fetch(), &[unknown]).is_ok());
    }

    #[test]
    fn the_key_names_every_dimension_that_is_not_the_path() {
        let key = fetch().commit_key();
        assert_eq!(
            key,
            "cohort:hyperliquid:BTC:size:realized:point_in_time:1756000800000-1756004400000"
        );
    }
}
