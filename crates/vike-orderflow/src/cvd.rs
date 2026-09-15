//! Cumulative volume delta — running Σ(buy − sell) over the trade stream.
use crate::classify::signed;
use vike_model::TradeTick;

#[derive(Default)]
pub struct CvdAccumulator {
    cvd: f64,
}
impl CvdAccumulator {
    pub fn new() -> Self {
        CvdAccumulator { cvd: 0.0 }
    }
    pub fn push(&mut self, t: &TradeTick) {
        if t.size <= 0.0 {
            return;
        }
        let (b, s) = signed(t);
        self.cvd += b - s;
    }
    pub fn value(&self) -> f64 {
        self.cvd
    }
    pub fn from_trades(trades: &[TradeTick]) -> f64 {
        let mut sum = 0.0;
        for t in trades.iter().filter(|t| t.size > 0.0) {
            let (b, s) = signed(t);
            sum += b - s;
        }
        sum
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn tk(size: f64, ibm: bool) -> TradeTick {
        TradeTick {
            ts: 0,
            local_ts: 0,
            price: 1.0,
            size,
            is_buyer_maker: ibm,
            symbol: String::new(),
        }
    }
    #[test]
    fn cvd_expected_and_skips_zero() {
        // +3 buy, -2 sell, skip 0, +1 buy → +2
        let trades = [tk(3.0, false), tk(2.0, true), tk(0.0, false), tk(1.0, false)];
        assert_eq!(CvdAccumulator::from_trades(&trades), 2.0);
    }
    #[test]
    fn cvd_streaming_equals_batch() {
        let trades = [tk(3.0, false), tk(2.0, true), tk(1.0, false)];
        let batch = CvdAccumulator::from_trades(&trades);
        let mut c = CvdAccumulator::new();
        for t in &trades {
            c.push(t);
        }
        assert_eq!(c.value().to_bits(), batch.to_bits());
    }
}
