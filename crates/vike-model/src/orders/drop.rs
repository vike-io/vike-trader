//! An order a gate DROPPED, as a backtest records it: the symbol, the gate's reason, the size refused
//! and the sizer's weight. The simulator collects them (`vike_simulator::OrderBrokerSimulator::dropped`), the result
//! carries them (`vike_analytics::TradingRecordSimulator::dropped`) and a run record writes them
//! (`vike_runs::RunDiagnostics::dropped`), one type all the way, its JSON keys `symbol`, `reason`,
//! `size` and `weight`.

use serde::{Deserialize, Deserializer, Serialize};

/// One order the engine's gates DROPPED, named rather than a tuple because a run record is a
/// document a human opens.
///
/// `vike_analytics::zero_trade::aggregate_denials` turns a run's list into a ranked diagnosis. Both
/// halves are worth keeping: the aggregate is what a reader reads, the rows are what an
/// investigation needs.
///
/// ⚠ **A non-finite `size` or `weight` reads back.** JSON writes a NaN or an infinity as `null`, which no `f64`
/// reads, so one such drop made the whole answer that carried it undecodable (a whole `RunResult`, a run record):
/// `null` reads as NaN (`nan_when_null`). A finite value writes and reads the bytes it always did, and a format
/// that carries a float as it is (msgpack) never writes `null`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OrderDrop {
    /// Which instrument the order was for.
    pub symbol: String,
    /// The gate's own reason string — a `RiskGate` reason, an order-kind cash-gate drop,
    /// `"volume_cap"` or `"latency_reject"`.
    pub reason: String,
    /// The order size that was refused.
    #[serde(deserialize_with = "nan_when_null")]
    pub size: f64,
    /// The sizer's weight for it.
    #[serde(deserialize_with = "nan_when_null")]
    pub weight: f64,
}

/// An `f64` that a JSON writer turned into `null` for not being finite, read back as NaN: which non-finite value it
/// was the `null` does not say, and NaN is the one that claims no sign or size.
fn nan_when_null<'de, D: Deserializer<'de>>(d: D) -> Result<f64, D::Error> {
    Ok(Option::<f64>::deserialize(d)?.unwrap_or(f64::NAN))
}

#[cfg(test)]
mod tests {
    use super::OrderDrop;

    /// A NaN, an infinite and a finite drop cross JSON: the finite one in the bytes it always had, the two others as
    /// `null`, read back as NaN instead of failing the document.
    #[test]
    fn a_non_finite_size_or_weight_reads_back_as_nan() {
        let drop = |size: f64, weight: f64| OrderDrop {
            symbol: "BTCUSDT".to_string(),
            reason: "volume_cap".to_string(),
            size,
            weight,
        };
        let finite = serde_json::to_string(&drop(2.5, 1.0)).unwrap();
        assert_eq!(finite, r#"{"symbol":"BTCUSDT","reason":"volume_cap","size":2.5,"weight":1.0}"#);
        assert_eq!(serde_json::from_str::<OrderDrop>(&finite).unwrap(), drop(2.5, 1.0));
        for (size, weight) in [(f64::NAN, 1.0), (f64::INFINITY, f64::NEG_INFINITY)] {
            let text = serde_json::to_string(&drop(size, weight)).unwrap();
            let back: OrderDrop =
                serde_json::from_str(&text).expect("a non-finite drop reads back");
            assert!(back.size.is_nan(), "{text}");
            assert_eq!(back.weight.is_nan(), !weight.is_finite(), "{text}");
        }
    }
}
