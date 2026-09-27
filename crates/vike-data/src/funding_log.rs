//! Realized perp funding-payment rows — the ACCOUNT funding series (Tier-2), NOT market prints.
//!
//! One durable series persists a strategy's realized perp funding history:
//! - `kind=exec_funding` ([`FundingRow`]) — one realized funding credit/debit (a payment WE paid or
//!   received on a perp position), keyed `venue=…`/`symbol=<coin>` in the store tree.
//!
//! This is the durable twin of the live [`vike_model::FundingEvent`] union member, but a distinct
//! record: `FundingEvent` carries `position_side`/`mark_price` for the live path, whereas this
//! historical row carries the venue `hash` (the per-row at-most-once identity) and the signed `szi`
//! at funding time — the fields realized-funding ACCOUNTING needs. Sourced from a venue's realized
//! funding endpoint (e.g. Hyperliquid `userFunding`); the fetcher lives in the venue bridge, the
//! backfill glue in `vike-backfill`, and this row is what lands in the store.
//!
//! CRITICAL namespace note: like the exec trade-log ([`crate::exec_log`]), `kind=exec_funding` is DISTINCT
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

/// One realized perp funding payment, persisted as the `kind=exec_funding` HistStore series.
/// Identity `(venue, symbol=<coin>)` is the PARTITION path (never a stored column) — the venue
/// `coin` the payment applied to becomes the series `symbol`, dropped from the row exactly as a
/// quote/trade drops its symbol into the partition. `hash` is the per-row at-most-once identity
/// (analogous to [`crate::ExecFillRow::trade_id`]); the store's idempotency is nonetheless
/// batch-level by `commit_key`, never per-row value dedup (the store contract).
///
/// ⚠ **[`FundingRow::account`] is the one field that is NOT in the partition and must not be**, and
/// `docs/decisions/0080-the-account-funding-kind-takes-the-qualified-name.md` is the argument. The partition is `(venue, coin)` while the commit key
/// carries `{account}`, so before this column existed two accounts' payments for one coin landed in
/// ONE series with **nothing distinguishing them** — not the partition, and not `hash`, which is the
/// venue TRANSACTION hash and names no account. A main account's −1200 and a test account's −30 read
/// back as −1230 with no way to subtract either. The column makes a scan attributable; whether the
/// series should ALSO be partitioned per account is deliberately left open by that record, and is a
/// convenience question once the rows can be told apart.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FundingRow {
    /// Funding timestamp, epoch milliseconds (UTC) — the venue funding `time`. Named `ts` (not
    /// `time_ms`) so it maps to the universal `ts` column every series codec + the ts-range read
    /// filter key off.
    pub ts: i64,
    /// **WHOSE payment this is** — the account the funding was realized on.
    ///
    /// The value is not a new concept and is never inferred: it is already a PARAMETER of the
    /// producer (`crates/vike-backfill/src/hyperliquid.rs`'s `backfill_hyperliquid_funding` takes
    /// `account: &str` and spends it on `funding_commit_key` one line later), so this field persists
    /// what the writer already holds. The SPELLING is per-venue and `docs/decisions/0080-the-account-funding-kind-takes-the-qualified-name.md` fixes only
    /// the two shapes that exist today: **the wallet address** on an on-chain venue (hyperliquid,
    /// aster — where an account IS an address), and **the credential key prefix** on a keyed venue,
    /// per the `DUKASCOPY_DEMO1`/`DEMO2` precedent. A venue needing a third shape argues it there.
    pub account: String,
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
            account: "0xmain".into(),
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
            account: "0xmain".into(),
            usdc: 0.75,
            szi: -2.0,
            funding_rate: -0.000_008_8,
            hash: "0xdef".into(),
        };
        let back: FundingRow =
            serde_json::from_str(&serde_json::to_string(&received).unwrap()).unwrap();
        assert_eq!(received, back);
    }

    /// The defect `docs/decisions/0080-the-account-funding-kind-takes-the-qualified-name.md` verdict 5 closes, written as the case that produced it: two
    /// accounts funding ONE coin land in one series, and before the `account` column the rows were
    /// not merely hard to separate — they were indistinguishable, because `hash` is the venue
    /// TRANSACTION hash and names no account.
    #[test]
    fn two_accounts_in_one_series_are_attributable() {
        let rows = [
            FundingRow {
                ts: 1_725_148_800_000,
                account: "0xMAIN".into(),
                usdc: -1200.0,
                szi: 12.5,
                funding_rate: 0.000_125,
                hash: "0x7a3f".into(),
            },
            FundingRow {
                ts: 1_725_152_400_000,
                account: "0xTEST".into(),
                usdc: -30.0,
                szi: -0.4,
                funding_rate: 0.000_125,
                hash: "0x91b2".into(),
            },
        ];

        // the whole series totals both accounts...
        let all: f64 = rows.iter().map(|r| r.usdc).sum();
        assert_eq!(all, -1230.0);

        // ...and ONE account's cost is now recoverable, which is the point.
        let main: f64 = rows.iter().filter(|r| r.account == "0xMAIN").map(|r| r.usdc).sum();
        assert_eq!(main, -1200.0);

        // the hash cannot do this job and never could — it is per-ROW, not per-ACCOUNT.
        assert_ne!(rows[0].hash, rows[1].hash);
        assert_ne!(rows[0].account, rows[1].account);
    }
}
