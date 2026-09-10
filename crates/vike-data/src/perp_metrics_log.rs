//! Perp market-CONTEXT metrics — the `kind=perp_metrics` series (the venue's own per-interval
//! numbers about a perpetual that are NOT its funding rate).
//!
//! One durable series persists the venue-reported context of a perp:
//! - `kind=perp_metrics` ([`PerpMetricRow`]) — for one funding interval and one perp, the venue's
//!   published FUNDING PREMIUM, keyed `venue=<perp venue>`/`symbol=<the same store symbol the
//!   funding-rate series uses>` in the store tree.
//!
//! # Why this is a KIND and not a column on `vike_model::Bar`
//!
//! `Bar` is the workspace's OHLCV type and the obvious place to hang another market number — its
//! `funding` field is exactly that precedent. It is the wrong home here, for two reasons measured
//! rather than assumed:
//!
//! 1. **Cost.** `Bar` has no `Default` impl and is built by struct literal at 432 sites across 189
//!    files. A new field is a 432-site edit for a number that is `None` at all but a handful of
//!    them.
//! 2. **Meaning, which is the reason that survives even if the cost were zero.** `Bar` is shared by
//!    every venue, every backtest, every chart and the parity fixtures. A funding premium is a
//!    PERP-only venue metric that exists only at funding timestamps; making every equity bar, every
//!    FX bar and every spot bar carry a permanently-absent slot for it puts a venue-family concept
//!    in the workspace's most central type. `crates/vike-data/src/store_kind.rs`'s `bar` row also
//!    records that the codec stores seven columns while `Bar` has ten fields — `bid`/`ask`/`symbol`
//!    are dropped on every round trip — so a `Bar` field is not even automatically a stored one.
//!
//! # Why the funding RATE is deliberately NOT here
//!
//! ⚠ The premium and the funding rate arrive in the SAME venue response (Hyperliquid's
//! `fundingHistory` row is `{coin, fundingRate, premium, time}`), at the same timestamps, from the
//! same fetch — so the tempting shape is one row carrying both. That would give the market funding
//! rate a SECOND home: it already has one, on [`vike_model::Bar::funding`], in the `kind=bar`
//! series under the reserved `interval=funding` label that
//! `crates/vike-backfill/src/funding_rate.rs`'s `default_interval` exists to
//! keep in its own keyspace. `crates/vike-data/src/store_kind.rs`'s `funding` row already PINS one
//! name collision in this family (`kind=funding` is the ACCOUNT's realized payments, not the market
//! rate); a duplicate STORAGE of the rate would be the worse version of the same defect, because
//! the two copies could disagree. So this kind carries what has no home, and nothing that does.
//!
//! # What is NOT stored here, and why the absence is the interesting part
//!
//! ⚠ **Open interest is absent DELIBERATELY, and it is absent because the venue does not serve
//! it as history.** Hyperliquid publishes open interest only as a CURRENT SNAPSHOT — in the
//! `POST /info {"type":"metaAndAssetCtxs"}` response and on the `activeAssetCtx` websocket
//! channel — and its `/info` surface has no historical open-interest verb at all. A backfill
//! therefore cannot reconstruct past open interest from the API however it is written, so the
//! column is not here rather than being here and permanently NULL. This row type is shaped so that
//! decision can be revisited as a COLUMN (`open_interest: f64_add`, the schema-tolerant additive
//! flavor the `book` kind already uses) rather than as a second kind, if a source is ever
//! adopted.
//!
//! Pure data — no I/O. The DataFusion+Parquet codec (schema/encode/decode) lives next to the other
//! series codecs in `datafusion_hist::codec` (it impls the crate-private `SeriesCodec` trait, so it
//! must sit inside that module); this row type is always compiled and model-only, exactly like
//! [`crate::CohortRow`], so the trait signatures in [`crate::HistStore`] can name it without the
//! `hist-datafusion` feature.

use serde::{Deserialize, Serialize};

/// One `(funding interval, perp)` market-context observation, persisted as the `kind=perp_metrics`
/// HistStore series.
///
/// Identity `(venue, symbol)` is the PARTITION path and is NOT repeated as a column — unlike
/// [`crate::CohortRow`] and [`crate::ChainRow`], which store their asset again because they carry
/// further dimensions that must be stored anyway. This row has no such dimension: one `(venue,
/// symbol)` series holds one value per timestamp, so the path re-injects the whole identity and a
/// duplicate column would be a second spelling of it. [`crate::FundingRow`] is the precedent — it
/// drops the coin from the row for the same reason.
///
/// The store's idempotency is batch-level by `commit_key`, never per-row value dedup (the store
/// contract); `crates/vike-backfill/src/funding_rate.rs`'s `perp_metrics_commit_key` is the shape
/// its one producer uses.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PerpMetricRow {
    /// The funding-interval timestamp, epoch MILLISECONDS (UTC) — the venue's own `time` field,
    /// the SAME instant the matching `Bar.funding` row carries, so a study can align the two
    /// series by timestamp without interpolating.
    pub ts: i64,
    /// The venue's published funding PREMIUM for this interval — a fraction, not a percentage, and
    /// SIGNED (negative when the perp trades below its oracle/index).
    ///
    /// ⚠ **It is not the funding rate and it is not derivable from it.** A venue computes the rate
    /// from the premium plus an interest-rate term and then CLAMPS it to a per-venue cap, so the
    /// rate loses information the premium keeps — which is exactly why the study's `premium_z_24h`
    /// is a separate feature from its `funding_z_24h` rather than a rescaling of it.
    ///
    /// NON-nullable, and that is a statement about the producer rather than a default: a row is
    /// written only for an interval whose premium the venue actually reported (Binance's
    /// `/fapi/v1/fundingRate` reports none at all and therefore writes no rows here), so a NULL
    /// would document a state that cannot reach this type.
    pub premium: f64,
    /// Perp open interest at this hour, in the venue's own units, or `None` for a row whose
    /// producer had no reading — the funding-rate collector reads an endpoint that carries no OI
    /// and writes every row with `None`.
    ///
    /// ⚠ ADDITIVE (`f64_add` in the codec and `store_kind.rs`): appended LAST, read through the
    /// absent-column-tolerant path, so every part written before this field existed decodes with
    /// `None` — exec_fill's `mark_price` contract, applied here. The module doc above argued this
    /// exact revisit condition, and the column arrives because a SOURCE did: `data.vike.io`'s
    /// `/v1/hyperliquid/assets/hourly` panel serves hourly OI history from its own HL node, which
    /// the venue's public `/info` surface still does not.
    pub open_interest: Option<f64>,
}
