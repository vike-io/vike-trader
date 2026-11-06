//! `EquitySample` — one point on a venue's (or the cross-venue `"TOTAL"`) equity curve.
//! Emitted by the vike-core equity sampler (PR-3), persisted as the `kind=equity` HistStore
//! series, and read by the future live-tearsheet path. Pure data — no I/O.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EquitySample {
    pub ts: i64,
    pub venue: String,
    pub equity: f64,
    pub realized: f64,
    pub unrealized: f64,
    pub missing_prices: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn equity_sample_roundtrips_serde_and_eq() {
        let s = EquitySample {
            ts: 1_000,
            venue: "binance".into(),
            equity: 10_500.0,
            realized: 500.0,
            unrealized: 0.0,
            missing_prices: 0,
        };
        let j = serde_json::to_string(&s).unwrap();
        let back: EquitySample = serde_json::from_str(&j).unwrap();
        assert_eq!(s, back);
        let total = EquitySample { venue: "TOTAL".into(), ..s.clone() };
        assert_eq!(total.venue, "TOTAL");
    }
}
