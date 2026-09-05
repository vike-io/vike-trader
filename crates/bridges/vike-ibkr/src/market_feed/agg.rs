//! Aggregate IBKR's fixed 5-second realtime bars up to the requested interval (e.g. "1m"). Pure +
//! unit-tested: given a stream of 5s bars, emit a forming bar on each and a closed bar at each
//! interval boundary.
use vike_model::Bar;

pub struct BarAggregator {
    interval_ms: i64,
    cur: Option<Bar>,
    bucket_start: i64,
}

impl BarAggregator {
    pub fn new(interval_ms: i64) -> Self {
        BarAggregator { interval_ms: interval_ms.max(1), cur: None, bucket_start: 0 }
    }
    /// Fold one 5s bar. Returns `(forming, Some(closed))` where `closed` is the completed prior
    /// bucket when this 5s bar starts a new one, else `None`.
    pub fn fold(&mut self, b5: &Bar) -> (Bar, Option<Bar>) {
        let bucket = b5.ts - (b5.ts.rem_euclid(self.interval_ms));
        let mut closed = None;
        if self.cur.is_none() || bucket != self.bucket_start {
            closed = self.cur.take();
            self.bucket_start = bucket;
            self.cur = Some(Bar {
                ts: bucket,
                open: b5.open,
                high: b5.high,
                low: b5.low,
                close: b5.close,
                volume: b5.volume,
                funding: None,
                bid: None,
                ask: None,
                symbol: None,
            });
        } else if let Some(c) = self.cur.as_mut() {
            c.high = c.high.max(b5.high);
            c.low = c.low.min(b5.low);
            c.close = b5.close;
            c.volume += b5.volume;
        }
        (self.cur.clone().expect("cur set"), closed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn b(ts: i64, o: f64, h: f64, l: f64, c: f64, v: f64) -> Bar {
        Bar {
            ts,
            open: o,
            high: h,
            low: l,
            close: c,
            volume: v,
            funding: None,
            bid: None,
            ask: None,
            symbol: None,
        }
    }
    #[test]
    fn aggregates_5s_into_1m_and_closes_on_boundary() {
        let mut a = BarAggregator::new(60_000);
        let (f1, c1) = a.fold(&b(60_000, 1.0, 2.0, 0.5, 1.5, 10.0));
        assert!(c1.is_none());
        assert_eq!(
            (f1.ts, f1.open, f1.high, f1.low, f1.close, f1.volume),
            (60_000, 1.0, 2.0, 0.5, 1.5, 10.0)
        );
        let (f2, c2) = a.fold(&b(65_000, 1.5, 3.0, 1.4, 2.8, 5.0)); // same 1m bucket
        assert!(c2.is_none());
        assert_eq!((f2.high, f2.low, f2.close, f2.volume), (3.0, 0.5, 2.8, 15.0));
        let (f3, c3) = a.fold(&b(120_000, 2.8, 2.9, 2.7, 2.85, 3.0)); // next 1m bucket
        let closed = c3.expect("closes prior bucket");
        assert_eq!((closed.ts, closed.close, closed.volume), (60_000, 2.8, 15.0));
        assert_eq!(f3.ts, 120_000);
    }
}
