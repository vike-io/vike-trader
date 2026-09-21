//! Realized perp funding-payment rows — the ACCOUNT funding series (Tier-2), NOT market prints.
//!
//! One durable series persists a strategy's realized perp funding history:
//! - `kind=funding` ([`FundingRow`]) — one realized funding credit/debit (a payment WE paid or
//!   received on a perp position), keyed `venue=…`/`symbol=<coin>` in the store tree.
//!
//! This is the durable twin of the live [`vike_model::FundingEvent`] union member, but a distinct
//! record: `FundingEvent` carries `position_side`/`mark_price` for the live path, whereas this
//! historical row carries the venue `hash` (the per-row at-most-once identity) and the signed `szi`
//! at funding time — the fields realized-funding ACCOUNTING needs. Sourced from a venue's realized
//! funding endpoint (e.g. Hyperliquid `userFunding`); the fetcher lives in the venue bridge, the
//! backfill glue in `vike-backfill`, and this row is what lands in the store.
//!
//! CRITICAL namespace note: like the exec trade-log ([`crate::exec_log`]), `kind=funding` is DISTINCT
//! from every market kind (`kind=trade`/`kind=book`/…), so account funding never collides with a
//! symbol's public prints even when `(venue, symbol)` match — see the codec in
//! `datafusion_hist::codec` and the namespace-guard test in `tests/funding_series.rs`.
//!
//! Pure data — no I/O. The DataFusion+Parquet codec (schema/encode/decode) lives next to the other
//! series codecs in `datafusion_hist::codec` (it impls the crate-private `SeriesCodec` trait, so it
//! must sit inside that module); this row type is always compiled and model-only, exactly like
//! [`crate::ExecFillRow`], so the trait signatures in [`crate::HistStore`] can name it without the
//! `hist-datafusion` feature.

use serde::{Deserialize, Serialize};

/// One realized perp funding payment, persisted as the `kind=funding` HistStore series. Identity
/// `(venue, symbol=<coin>)` is the PARTITION path (never a stored column) — the venue `coin` the
/// payment applied to becomes the series `symbol`, dropped from the row exactly as a quote/trade
/// drops its symbol into the partition. `hash` is the per-row at-most-once identity (analogous to
/// [`crate::ExecFillRow::trade_id`]); the store's idempotency is nonetheless batch-level by
/// `commit_key`, never per-row value dedup (the store contract).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FundingRow {
    /// Funding timestamp, epoch milliseconds (UTC) — the venue funding `time`. Named `ts` (not
    /// `time_ms`) so it maps to the universal `ts` column every series codec + the ts-range read
    /// filter key off.
    pub ts: i64,
    /// The SIGNED realized funding payment, in USDC: **negative = paid**, **positive = received**.
    pub usdc: f64,
    /// Signed position size at funding time (+ long / − short).
    pub szi: f64,
    /// The funding rate applied this interval.
    pub funding_rate: f64,
    /// The venue transaction hash (`0x…`), the per-row identity for at-most-once accounting.
    pub hash: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn funding_row_roundtrips_serde_and_eq() {
        let paid = FundingRow {
            ts: 1_681_222_254_710,
            usdc: -1.25,
            szi: 0.5,
            funding_rate: 0.000_012_5,
            hash: "0xabc".into(),
        };
        let back: FundingRow =
            serde_json::from_str(&serde_json::to_string(&paid).unwrap()).unwrap();
        assert_eq!(paid, back);

        // a RECEIVED row (positive usdc, short szi, negative rate) round-trips too
        let received = FundingRow {
            ts: 1_681_222_254_720,
            usdc: 0.75,
            szi: -2.0,
            funding_rate: -0.000_008_8,
            hash: "0xdef".into(),
        };
        let back: FundingRow =
            serde_json::from_str(&serde_json::to_string(&received).unwrap()).unwrap();
        assert_eq!(received, back);
    }
}
