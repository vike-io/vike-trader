//! Option-chain snapshot rows — the point-in-time options-surface series (`kind=chain`).
//!
//! One durable series persists a venue's observed option-chain snapshots over time:
//! - `kind=chain` ([`ChainRow`]) — one option instrument's quote/greeks state inside one chain
//!   snapshot, keyed `venue=…`/`symbol=<underlying>` in the store tree (e.g.
//!   `kind=chain/venue=deribit/symbol=BTC`). All rows of one recorded chain share `ts` (the
//!   chain's `asof_ms`), so a snapshot is the set of rows at one `ts`.
//!
//! This is the PIT twin of `kind=properties` for the options surface: venues fetch a live chain
//! (e.g. `vike-deribit`'s `chain.rs` book-summary parse) and an opt-in recorder
//! ([`crate::ChainRecorder`], `VIKE_RECORD_CHAINS=1`) persists what was observed, so backtests and
//! analytics can ask "what did the BTC surface look like at ts?" ([`crate::HistStore::chain_as_of`]).
//!
//! CRITICAL namespace note: like the exec trade-log ([`crate::exec_log`]) and realized funding
//! ([`crate::funding_log`]), `kind=chain` is DISTINCT from every market kind
//! (`kind=trade`/`kind=book`/…), so chain snapshots never collide with an underlying's public
//! prints even when `(venue, symbol)` match.
//!
//! Pure data — no I/O. The DataFusion+Parquet codec (schema v1 `vike.schema.chain`, quote/greek
//! fields NULLABLE — absent stays absent, never 0.0/NaN) lives next to the other series codecs in
//! `datafusion_hist::codec`; this row type is always compiled and model-only, exactly like
//! [`crate::FundingRow`], so the trait signatures in [`crate::HistStore`] can name it without the
//! `hist-datafusion` feature.

use serde::{Deserialize, Serialize};

/// One option instrument's state inside one chain snapshot, persisted as the `kind=chain`
/// HistStore series. Identity `(venue, symbol=<underlying>)` is the PARTITION path; `underlying`
/// is ALSO carried as a stored column (like [`crate::ExecFillRow`] carries venue+symbol), so the
/// compaction re-encode (`ctx=""`) is lossless and a decoded row is self-describing.
///
/// `instrument` is the exact venue instrument id (e.g. `"BTC-27JUN26-100000-C"`, verbatim incl.
/// any settlement suffix) — the per-instrument grouping identity [`crate::HistStore::chain_as_of`]
/// keys on. Quote/greek fields are `Option<f64>`: absent stays absent (SQL NULL in the store),
/// never 0.0 or NaN — the same convention as the `vike-options` `OptionQuote` they mirror.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChainRow {
    /// Snapshot observation timestamp, epoch milliseconds (UTC) — the chain's `asof_ms`. All rows
    /// of one snapshot share it. Named `ts` so it maps to the universal `ts` column every series
    /// codec + the ts-range read filter key off.
    pub ts: i64,
    /// Underlying coin/index (e.g. "BTC") — also the series partition `symbol`.
    pub underlying: String,
    /// Exact venue instrument id, verbatim (e.g. "BTC-27JUN26-100000-C", "SOL_USDC-26JUN26-90-P").
    pub instrument: String,
    /// Expiry instant, epoch milliseconds (UTC) — e.g. the 08:00-UTC settle
    /// (`vike_options::expiry_ms`).
    pub expiry_ms: i64,
    /// Strike price (USD).
    pub strike: f64,
    /// `true` = call, `false` = put.
    pub is_call: bool,
    /// Best bid (USD premium), absent if no bid.
    pub bid: Option<f64>,
    /// Best ask (USD premium), absent if no ask.
    pub ask: Option<f64>,
    /// Mark price (USD premium).
    pub mark: Option<f64>,
    /// Implied volatility, DECIMAL (0.625 = 62.5%).
    pub iv: Option<f64>,
    /// Open interest (venue units).
    pub open_interest: Option<f64>,
    /// Traded volume (venue units).
    pub volume: Option<f64>,
    /// Greek: delta.
    pub delta: Option<f64>,
    /// Greek: gamma.
    pub gamma: Option<f64>,
    /// Greek: theta (per day).
    pub theta: Option<f64>,
    /// Greek: vega (per vol point).
    pub vega: Option<f64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chain_row_roundtrips_serde_and_eq() {
        // A fully-populated call and a sparse put (absent quote fields stay absent through serde).
        let call = ChainRow {
            ts: 1_780_387_200_000,
            underlying: "BTC".into(),
            instrument: "BTC-27JUN26-100000-C".into(),
            expiry_ms: 1_782_547_200_000,
            strike: 100_000.0,
            is_call: true,
            bid: Some(5_200.0),
            ask: Some(6_240.0),
            mark: Some(5_720.0),
            iv: Some(0.625),
            open_interest: Some(120.0),
            volume: Some(8.0),
            delta: Some(0.55),
            gamma: Some(0.000_01),
            theta: Some(-45.2),
            vega: Some(210.0),
        };
        let back: ChainRow = serde_json::from_str(&serde_json::to_string(&call).unwrap()).unwrap();
        assert_eq!(call, back);

        let sparse = ChainRow {
            is_call: false,
            instrument: "BTC-27JUN26-100000-P".into(),
            bid: None,
            ask: None,
            mark: None,
            iv: None,
            open_interest: None,
            volume: None,
            delta: None,
            gamma: None,
            theta: None,
            vega: None,
            ..call
        };
        let back: ChainRow =
            serde_json::from_str(&serde_json::to_string(&sparse).unwrap()).unwrap();
        assert_eq!(sparse, back);
        assert_eq!(back.bid, None, "absent stays absent, never 0.0");
    }
}
