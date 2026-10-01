//! Cohort open-interest rows — the `kind=cohort` series (one hourly marginal per cohort LABEL).
//!
//! One durable series persists a graded positioning panel:
//! - `kind=cohort` ([`CohortRow`]) — for one hour, one asset and one cohort label, the long-side
//!   notional and the total notional the grading service reported, keyed
//!   `venue=<exchange>`/`symbol=<asset>` in the store tree.
//!
//! # Why the label is a COLUMN and not a schema
//!
//! The obvious alternative is a column per label per metric, which reads better at a SQL prompt and
//! is wrong here for a reason the store enforces rather than merely prefers:
//! `crate::store_kind::STORE_KINDS`' `cohort` row states the arithmetic (36 admitted labels, two
//! wire numbers each, 73 columns at one grading and 129 across three, against a widest existing
//! kind of 16) and `crates/vike-data/tests/store_kind_gate.rs`'s
//! `every_row_matches_its_codec_field_list` pins [`crate::store_kind::StoreKind::columns`] VERBATIM
//! against the codec field list. A wide schema would therefore turn every taxonomy revision into a
//! schema migration, and the taxonomy is somebody else's data: the rungs are
//! `crates/vike-backfill/src/vikedata/parse.rs`'s `SIZE_COHORTS` / `PNL_COHORTS` / `TIER_COHORTS`,
//! and that file's own doc records one being regenerated out from under a consumer already.
//!
//! # The five dimensions that are NOT the path
//!
//! `(venue, symbol=<asset>)` is the partition, and the asset is ALSO a stored column (the
//! `crate::chain_log::ChainRow` `underlying` precedent — one decode context string cannot re-inject
//! five). `axis`, `cohort`, `grading` and `label_basis` have nowhere else to live, and the last two
//! are the ones a reader is tempted to drop: they are per-FETCH facts, constant across the batch,
//! so they look like metadata. They are not. The three gradings produce SHAPE-IDENTICAL rows for
//! the same asset over the same hours, so a series without them cannot tell a re-graded fetch from
//! the realized one it aliases — see [`CohortRow::grading`].
//!
//! Pure data — no I/O. The DataFusion+Parquet codec (schema/encode/decode) lives next to the other
//! series codecs in `datafusion_hist::codec` (it impls the crate-private `SeriesCodec` trait, so it
//! must sit inside that module); this row type is always compiled and model-only, exactly like
//! [`crate::FundingRow`], so the trait signatures in [`crate::HistStore`] can name it without the
//! `hist-datafusion` feature.

use serde::{Deserialize, Serialize};

/// One `(hour, asset, axis, label)` cohort marginal, persisted as the `kind=cohort` HistStore
/// series.
///
/// Identity `(venue, symbol=<asset>)` is the PARTITION path; unlike a quote or a trade, the asset
/// is ALSO kept as a row column, because the other four dimensions must be stored anyway and a
/// half-stored identity is the shape that invites a consumer to reconstruct the rest from the path.
///
/// The store's idempotency is batch-level by `commit_key`, never per-row value dedup (the store
/// contract) — and for this kind the key must discriminate all four non-path dimensions, which is
/// `crate::cohort_rec::CohortRecorder`'s `commit_key`'s whole subject.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CohortRow {
    /// The hour bucket, epoch MILLISECONDS (UTC).
    ///
    /// ⚠ **Milliseconds, and the upstream is SECONDS.** Every series codec and the `date=`
    /// partitioner read this column as epoch-ms (`vike_model::time::epoch_ms_to_utc_date` decides
    /// which partition a row lands in), while the cohort metrics API serves whole unix seconds on
    /// the hour — `crates/vike-backfill/src/vikedata/mod.rs`'s `floor_to_hour` is where the upstream
    /// value is floored. A row built by multiplying the wrong way lands 1970-01-01 and partitions
    /// there, silently and only once, which is why the conversion belongs to the producer and the
    /// unit is stated on the field rather than left to the name.
    pub ts: i64,
    /// The asset the positions are in (`BTC`), the venue's own coin spelling — the series `symbol`,
    /// stored again here (see the type's doc).
    pub asset: String,
    /// Which ladder the label comes from: `size`, `pnl` or `tier`. Stored rather than pushed into
    /// the path because the three ladders share the hour grid and are read together; a consumer
    /// asking "what did whales do" filters this column.
    ///
    /// The wire spellings are `crates/vike-backfill/src/vikedata/client.rs`'s `Axis`'s `as_str`, and
    /// this column is the string it returns — normalising it into an enum here would put a
    /// taxonomy in the schema, which is the thing this kind's shape exists to avoid.
    pub axis: String,
    /// The normalised cohort label — `4xWhale`, `3xSmart`, `above_2_5m`. One of the 36 the
    /// taxonomies admit today, and deliberately NOT validated against them at this layer: the
    /// admitted set is upstream data that has already been regenerated once, and a store that
    /// refuses an unknown rung would reject tomorrow's tape rather than record it.
    pub cohort: String,
    /// Which question the ladder answered: `realized`, `realized-pit` or `unrealized` — the wire
    /// spellings of `crates/vike-backfill/src/vikedata/client.rs`'s `Grading`'s `echoed`.
    ///
    /// ⚠ **This column is what stops a silent ALIAS.** The three gradings return the same shape
    /// over the same hours for the same asset, so two fetches differing only in grading are
    /// indistinguishable once stored without it — not overwritten (appends never overwrite), but
    /// interleaved, which is worse: a reader sums both and reports one.
    pub grading: String,
    /// How the label was RESOLVED — `point_in_time` for a frozen label, whatever the server echoed
    /// otherwise. The second per-fetch fact, and a distinct one from [`Self::grading`]: that field
    /// says what was ranked, this one says when the rank was computed, and
    /// `crates/vike-backfill/src/vikedata/client.rs`'s `Grading` carries the warning that a realized
    /// grading is honest or lookahead depending on this value alone.
    pub label_basis: String,
    /// The LONG side's notional, USD — the endpoint's `total_position_value_long`.
    pub long_usd: f64,
    /// The TOTAL notional, USD — the endpoint's `total_position_value`, i.e. `sum(abs(size_usd))`
    /// over the cohort, which is long + short.
    ///
    /// The total is stored rather than the short side because the total is what the wire carries;
    /// `short = total - long` is one subtraction at read time, whereas storing the derived value
    /// and reconstructing the total would bake this crate's arithmetic into the tape.
    pub total_usd: f64,
}

impl CohortRow {
    /// The SHORT side's notional, `total - long` — derived, never stored (see [`Self::total_usd`]).
    ///
    /// Returns whatever the subtraction gives, including a negative: a total below the long side is
    /// a contradiction in the SOURCE, and this accessor is not the place to hide one. The producer
    /// is where such a row is refused — `crates/vike-backfill/src/vikedata/parse.rs`'s
    /// `rows_from_metrics` already refuses it at fetch time with the two values in the message.
    pub fn short_usd(&self) -> f64 {
        self.total_usd - self.long_usd
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row() -> CohortRow {
        CohortRow {
            ts: 1_756_000_800_000,
            asset: "BTC".into(),
            axis: "size".into(),
            cohort: "4xWhale".into(),
            grading: "realized".into(),
            label_basis: "point_in_time".into(),
            long_usd: 60.0,
            total_usd: 100.0,
        }
    }

    #[test]
    fn cohort_row_roundtrips_serde_and_eq() {
        let r = row();
        let back: CohortRow = serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
        assert_eq!(r, back);
    }

    #[test]
    fn the_short_side_is_derived_from_the_total_and_the_long() {
        assert_eq!(row().short_usd(), 40.0);
    }

    #[test]
    fn a_total_below_the_long_side_surfaces_as_a_negative_short_rather_than_a_clamp() {
        // A clamp here would launder a source contradiction into a plausible zero, and the row that
        // produced it would then look ordinary in the tape. The producer refuses such a row; this
        // accessor reports what it was given.
        let bad = CohortRow { long_usd: 150.0, total_usd: 100.0, ..row() };
        assert_eq!(bad.short_usd(), -50.0);
    }

    /// The two per-fetch facts are part of row EQUALITY, which is the property that makes them
    /// worth storing: two rows identical in every other dimension are still different rows.
    #[test]
    fn two_gradings_of_one_hour_are_not_the_same_row() {
        let realized = row();
        let pit = CohortRow { grading: "realized-pit".into(), ..row() };
        let current = CohortRow { label_basis: "current".into(), ..row() };
        assert_ne!(realized, pit);
        assert_ne!(realized, current);
    }
}
