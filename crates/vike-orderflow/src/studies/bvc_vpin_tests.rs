use super::*;

// Φ(0) = ½ exactly (erf(0) = 0); Φ is symmetric: Φ(x) + Φ(−x) = 1; Φ(1) ≈ 0.8413447.
#[test]
fn norm_cdf_pins() {
    assert_eq!(norm_cdf(0.0).to_bits(), 0.5_f64.to_bits());
    assert!((norm_cdf(1.0) - 0.8413447460685429).abs() < 1e-12);
    assert!((norm_cdf(2.0) + norm_cdf(-2.0) - 1.0).abs() < 1e-15);
    assert_eq!(norm_cdf(40.0), 1.0); // saturation, no NaN
    assert_eq!(norm_cdf(-40.0), 0.0);
}

// Tick rule: up → all buy (1.0), down → all sell (0.0), flat → balanced (0.5).
#[test]
fn tick_rule_pins() {
    assert_eq!(tick_rule_buy_fraction(0.5), 1.0);
    assert_eq!(tick_rule_buy_fraction(-0.5), 0.0);
    assert_eq!(tick_rule_buy_fraction(0.0), 0.5);
}

// BVC: Φ(ΔP/σ); σ ≤ 0 (or non-finite) falls back to the tick rule — never Φ(x/0)=NaN.
#[test]
fn bvc_buy_fraction_and_sigma_zero_fallback() {
    assert_eq!(bvc_buy_fraction(0.0, 1.0).to_bits(), 0.5_f64.to_bits()); // Φ(0)
    assert!((bvc_buy_fraction(1.0, 1.0) - 0.8413447460685429).abs() < 1e-12);
    // σ = 0 → tick-rule fallback (no NaN)
    assert_eq!(bvc_buy_fraction(0.5, 0.0), 1.0);
    assert_eq!(bvc_buy_fraction(-0.5, 0.0), 0.0);
    assert_eq!(bvc_buy_fraction(0.0, 0.0), 0.5);
    assert_eq!(bvc_buy_fraction(1.0, f64::NAN), 1.0);
    assert!(!bvc_buy_fraction(0.0, 0.0).is_nan());
}

// A hand-computed BVC bucket-imbalance series. closes = [100, 101, 100]:
//   • close 100 → reference (no ΔP, no sample)
//   • close 101 → ΔP=+1, σ-window=[1] (n=1<2) ⇒ σ=0 ⇒ tick-rule(+1)=1.0 ⇒ |2·1−1| = 1.0
//   • close 100 → ΔP=−1, σ-window=[1,−1]: mean 0, var (1+1)/2 = 1, σ = 1; z = −1;
//     buy_frac = Φ(−1); imbalance = |2·Φ(−1) − 1| = erf(1/√2) = 0.6826894921370859.
#[test]
fn bvc_imbalance_series_hand_computed() {
    let imbs = BvcVpin::imbalances_from_closes(&[100.0, 101.0, 100.0], 3, VolumeClassifier::Bvc);
    assert_eq!(imbs.len(), 2);
    assert_eq!(imbs[0], 1.0); // σ=0 warm-up fallback, one-way ⇒ 1.0
    assert!((imbs[1] - 0.6826894921370859).abs() < 1e-12);
}

// Limiting case — a monotone one-way tape: every ΔP equal ⇒ σ=0 every bucket ⇒ tick-rule
// all-buy ⇒ each imbalance 1.0 ⇒ VPIN = 1.0 (maximally toxic). directional_bias = +1.0 (ask).
#[test]
fn bvc_all_buy_is_one() {
    let closes = [100.0, 101.0, 102.0, 103.0];
    let mut v = BvcVpin::with_params(1.0, 3, VolumeClassifier::Bvc);
    for &c in &closes {
        v.on_bucket(c);
    }
    assert_eq!(v.completed_buckets(), 3);
    assert_eq!(v.value(), Some(1.0));
    assert_eq!(v.directional_bias(), Some(1.0));
    assert_eq!(BvcVpin::from_bucket_closes(&closes, 3, VolumeClassifier::Bvc), Some(1.0));
}

// Limiting case — a flat tape: every ΔP = 0 ⇒ tick-rule balanced (0.5) ⇒ each imbalance 0.0
// ⇒ VPIN = 0.0 (no toxicity), directional_bias = 0.0. Also proves the Φ(0/0) NaN guard.
#[test]
fn bvc_flat_is_zero_no_nan() {
    let closes = [50.0, 50.0, 50.0, 50.0];
    let mut v = BvcVpin::with_params(1.0, 3, VolumeClassifier::Bvc);
    for &c in &closes {
        v.on_bucket(c);
    }
    assert_eq!(v.value(), Some(0.0));
    assert_eq!(v.directional_bias(), Some(0.0));
    assert!(!v.value().unwrap().is_nan());
}

// Ready/not-ready boundary: window N=2 needs N imbalances = N+1 = 3 bucket closes.
#[test]
fn bvc_ready_boundary() {
    let mut v = BvcVpin::with_params(1.0, 2, VolumeClassifier::Bvc);
    assert_eq!(v.on_bucket(100.0), None); // reference only
    assert_eq!(v.value(), None);
    assert_eq!(v.on_bucket(101.0), Some(1.0)); // imbalance #1 (σ=0 warm-up)
    assert_eq!(v.completed_buckets(), 1);
    assert_eq!(v.value(), None); // 1 < window 2 → not ready
    let imb2 = v.on_bucket(100.0).unwrap(); // imbalance #2
    assert!((imb2 - 0.6826894921370859).abs() < 1e-12);
    assert_eq!(v.value().unwrap(), (1.0 + imb2) / 2.0);
    // side-bias: signed = [+1.0, −0.6826894921370859] ⇒ mean = +0.15865525393145707
    assert!((v.directional_bias().unwrap() - 0.15865525393145707).abs() < 1e-12);
}

// The directional side-bias sign flips with net flow: a mostly-DOWN tape ⇒ sell-toxic (< 0).
#[test]
fn bvc_directional_bias_sign() {
    let closes = [100.0, 99.0, 98.0, 99.0]; // net down
    let mut v = BvcVpin::with_params(1.0, 3, VolumeClassifier::Bvc);
    for &c in &closes {
        v.on_bucket(c);
    }
    assert!(v.directional_bias().unwrap() < 0.0, "net-down ⇒ sell-toxic (bid side)");
    assert!(v.value().unwrap() > 0.0);
}

// on_trade's VBS bucketing (with spill) reduces to the on_bucket close series and is
// bit-identical to feeding the closes directly.
#[test]
fn bvc_on_trade_equals_on_bucket() {
    // VBS=4: three exact buckets closing at 100, 101, 100 → 2 imbalances (window 2 ⇒ ready).
    let mut a = BvcVpin::with_params(4.0, 2, VolumeClassifier::Bvc);
    assert!(a.on_trade(100.0, 4.0).is_empty()); // first bucket = reference
    assert_eq!(a.on_trade(101.0, 4.0), vec![1.0]);
    let last = a.on_trade(100.0, 4.0);
    assert_eq!(last.len(), 1);

    let mut b = BvcVpin::with_params(4.0, 2, VolumeClassifier::Bvc);
    for &c in &[100.0, 101.0, 100.0] {
        b.on_bucket(c);
    }
    assert_eq!(a.value().unwrap().to_bits(), b.value().unwrap().to_bits());
    assert_eq!(a.directional_bias().unwrap().to_bits(), b.directional_bias().unwrap().to_bits());
}

// A single big trade spills across several VBS buckets (all closing at the same price).
#[test]
fn bvc_on_trade_spills() {
    let mut v = BvcVpin::with_params(4.0, 50, VolumeClassifier::Bvc);
    v.on_trade(50.0, 4.0); // reference bucket
    let closed = v.on_trade(51.0, 12.0); // 12 / 4 = 3 buckets, all close @ 51
    assert_eq!(closed.len(), 3);
    assert_eq!(v.completed_buckets(), 3);
}

#[test]
fn bvc_zero_size_skipped() {
    let mut v = BvcVpin::new(1.0);
    assert!(v.on_trade(100.0, 0.0).is_empty());
    assert!(v.on_trade(100.0, -5.0).is_empty());
    assert_eq!(v.completed_buckets(), 0);
    assert_eq!(v.value(), None);
}

// Streaming↔batch: the per-bucket imbalance SERIES is bit-identical (shared σ fold order),
// even past window eviction; the rolling-sum VPIN matches the fresh-fold mean to rounding.
#[test]
fn bvc_streaming_equals_batch() {
    // A wandering close series that exercises + and − ΔP and the σ window.
    let closes: Vec<f64> =
        (0..40).map(|i| 100.0 + ((i * 7) % 11) as f64 - ((i * 3) % 5) as f64 * 0.5).collect();
    let window = 5;
    let batch_series = BvcVpin::imbalances_from_closes(&closes, window, VolumeClassifier::Bvc);
    let mut v = BvcVpin::with_params(1.0, window, VolumeClassifier::Bvc);
    let mut stream_series = Vec::new();
    for &c in &closes {
        if let Some(imb) = v.on_bucket(c) {
            stream_series.push(imb);
        }
    }
    assert!(stream_series.len() > window); // eviction actually exercised
    for (a, b) in stream_series.iter().zip(batch_series.iter()) {
        assert_eq!(a.to_bits(), b.to_bits()); // bit-identical imbalance series
    }
    let batch = BvcVpin::from_bucket_closes(&closes, window, VolumeClassifier::Bvc).unwrap();
    let s = v.value().unwrap();
    assert!((s - batch).abs() <= 1e-12, "streaming {s} vs batch {batch}");
}

// The TickRule classifier == BVC with σ forced to 0 (both route through tick_rule_buy_fraction).
#[test]
fn bvc_tickrule_classifier() {
    let closes = [10.0, 11.0, 10.5, 9.0, 9.0];
    let imbs = BvcVpin::imbalances_from_closes(&closes, 3, VolumeClassifier::TickRule);
    // ΔP = +1, −0.5, −1.5, 0 ⇒ tick-rule imbalances 1.0, 1.0, 1.0, 0.0
    assert_eq!(imbs, vec![1.0, 1.0, 1.0, 0.0]);
}
