use super::*;

fn tk(price: f64, size: f64, ibm: bool) -> TradeTick {
    TradeTick { ts: 0, local_ts: 0, price, size, is_buyer_maker: ibm, symbol: String::new() }
}

// V=4: bucket1 = buy 2 + buy 2 → |4−0|/4 = 1.0; bucket2 = sell 1 + buy 1 + sell 2
// → |1−3|/4 = 0.5. VPIN = (1.0 + 0.5)/2 = 0.75.
#[test]
fn vpin_expected_values() {
    let mut v = Vpin::with_params(4.0, 50, true);
    assert_eq!(v.committed(), None);
    assert_eq!(v.interim(), None);
    assert_eq!(v.push(&tk(1.0, 2.0, false), None), Vec::<f64>::new());
    assert_eq!(v.push(&tk(1.0, 2.0, false), None), vec![1.0]);
    assert_eq!(v.committed(), Some(1.0));
    v.push(&tk(1.0, 1.0, true), None);
    v.push(&tk(1.0, 1.0, false), None);
    assert_eq!(v.push(&tk(1.0, 2.0, true), None), vec![0.5]);
    assert_eq!(v.committed(), Some(0.75));
    assert_eq!(v.completed_buckets(), 2);
    assert_eq!(v.forming_bucket(), None);
}

// One buy of 10 into V=4 buckets: two completed (imb 1.0 each) + forming buy=2.
// Interim folds the partial bucket (|2−0|/2 = 1.0) in: (1+1+1)/3 = 1.0; then a sell 2
// completes it at |2−2|/4 = 0 → committed (1+1+0)/3 = 2/3.
#[test]
fn vpin_overflow_spills_and_interim() {
    let mut v = Vpin::with_params(4.0, 50, true);
    assert_eq!(v.push(&tk(1.0, 10.0, false), None), vec![1.0, 1.0]);
    let f = v.forming_bucket().unwrap();
    assert_eq!((f.buy_vol, f.sell_vol, f.filled()), (2.0, 0.0, 2.0));
    assert_eq!(v.interim(), Some(1.0));
    assert_eq!(v.committed(), Some(1.0)); // forming bucket not committed
    assert_eq!(v.push(&tk(1.0, 2.0, true), None), vec![0.0]);
    assert_eq!(v.committed(), Some(2.0 / 3.0));
    assert_eq!(v.interim(), Some(2.0 / 3.0)); // nothing forming → interim == committed
}

// window=2 evicts: buckets score 1.0, 0.0, 1.0 → after the 3rd, VPIN = (0+1)/2 = 0.5.
// Dyadic values → the O(1) rolling sum is exact.
#[test]
fn vpin_window_eviction_rolling_sum() {
    let mut v = Vpin::with_params(2.0, 2, true);
    v.push(&tk(1.0, 2.0, false), None); // bucket1: 1.0
    v.push(&tk(1.0, 1.0, false), None);
    v.push(&tk(1.0, 1.0, true), None); // bucket2: 0.0
    assert_eq!(v.committed(), Some(0.5));
    v.push(&tk(1.0, 2.0, true), None); // bucket3: 1.0 → evicts bucket1
    assert_eq!(v.committed(), Some(0.5));
    assert_eq!(v.completed_buckets(), 2);
    // interim with a FULL window evicts the oldest committed sample for the calc:
    v.push(&tk(1.0, 1.0, false), None); // forming: buy 1 → partial imb 1.0
    // window holds [0.0, 1.0]; interim = (1.0 + 1.0)/2 = 1.0 (0.0 evicted)
    assert_eq!(v.interim(), Some(1.0));
    assert_eq!(v.committed(), Some(0.5)); // untouched
}

// has_aggressor=false + mid: price>=mid → Buy, else Sell — the aggressor flags are set
// to CONTRADICT the mid test, proving mid wins; mid=None falls back to the flag.
#[test]
fn vpin_mid_fallback_classification() {
    let mut v = Vpin::with_params(2.0, 50, false);
    // both trades would classify Sell by flag; mid says 101→Buy, 99→Sell
    v.push(&tk(101.0, 1.0, true), Some(100.0));
    assert_eq!(v.push(&tk(99.0, 1.0, true), Some(100.0)), vec![0.0]); // buy1 sell1
    // mid=None → aggressor flag (false → Buy): two buys → imb 1.0
    v.push(&tk(99.0, 1.0, false), None);
    assert_eq!(v.push(&tk(99.0, 1.0, false), None), vec![1.0]);
}

#[test]
fn vpin_zero_size_skipped() {
    let mut v = Vpin::new(1.0);
    assert!(v.push(&tk(1.0, 0.0, false), None).is_empty());
    assert_eq!(v.forming_bucket(), None);
    assert_eq!(v.interim(), None);
}

// Streaming (rolling sum) vs batch (naive-fold tail mean): exact-dyadic sizes so the
// eviction arithmetic is exact → bit parity holds even past the window.
#[test]
fn vpin_streaming_equals_batch_dyadic() {
    let trades = vec![
        tk(1.0, 2.0, false),
        tk(1.0, 1.5, true),
        tk(1.0, 0.5, false),
        tk(1.0, 4.0, true),
        tk(1.0, 1.0, false),
        tk(1.0, 3.0, true),
    ];
    let window = 3;
    let batch = Vpin::from_trades(&trades, 2.0, window).unwrap();
    let mut v = Vpin::with_params(2.0, window, true);
    let mut imbs = Vec::new();
    for t in &trades {
        imbs.extend(v.push(t, None));
    }
    assert_eq!(imbs, Vpin::bucket_imbalances(&trades, 2.0));
    assert!(imbs.len() > window); // eviction actually exercised
    assert_eq!(v.committed().unwrap().to_bits(), batch.to_bits());
}

// Non-dyadic sizes: per-bucket imbalances still bit-identical (same fold order); the
// rolling-sum mean is allowed to differ from the fresh fold only at rounding noise.
#[test]
fn vpin_streaming_matches_batch_nondyadic() {
    let mut trades = Vec::new();
    for i in 0..40 {
        let size = 0.1 + (i as f64) * 0.07;
        trades.push(tk(1.0, size, i % 3 == 0));
    }
    let window = 5;
    let batch = Vpin::from_trades(&trades, 1.3, window).unwrap();
    let mut v = Vpin::with_params(1.3, window, true);
    let mut imbs = Vec::new();
    for t in &trades {
        imbs.extend(v.push(t, None));
    }
    assert_eq!(imbs, Vpin::bucket_imbalances(&trades, 1.3));
    let s = v.committed().unwrap();
    assert!((s - batch).abs() <= 1e-12, "streaming {s} vs batch {batch}");
}
