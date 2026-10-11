use super::*;

fn sample(ts: i64, o: f64, h: f64, l: f64, c: f64, v: f64) -> Bar {
    bar(ts, o, h, l, c, v, None, None)
}

#[test]
fn folds_ohlcv_and_closes_on_a_later_bucket() {
    let mut k = BarConsolidator::new(60_000);
    assert!(k.fold(&sample(60_000, 1.0, 2.0, 0.5, 1.5, 10.0)).is_none(), "first sample opens");
    assert!(k.fold(&sample(65_000, 1.5, 3.0, 1.4, 2.8, 5.0)).is_none(), "same bucket folds in");
    // OHLC: open is the FIRST sample's (never overwritten), high/low are the extremes across
    // the window, close is the last sample's, volume sums.
    let f = k.forming().expect("window open");
    assert_eq!(
        (f.ts, f.open, f.high, f.low, f.close, f.volume),
        (60_000, 1.0, 3.0, 0.5, 2.8, 15.0)
    );
    // a sample in a later bucket closes the prior window and opens the next
    let closed = k.fold(&sample(120_000, 2.8, 2.9, 2.7, 2.85, 3.0)).expect("closes prior");
    assert_eq!(
        (closed.ts, closed.open, closed.high, closed.low, closed.close, closed.volume),
        (60_000, 1.0, 3.0, 0.5, 2.8, 15.0)
    );
    assert_eq!(k.forming().expect("next window open").ts, 120_000);
    // flush drains the partial window, then is idempotent
    assert_eq!(k.flush().expect("partial flushes").ts, 120_000);
    assert!(k.flush().is_none());
}

#[test]
fn a_gap_skips_empty_buckets_rather_than_synthesizing_them() {
    // The fold emits only windows that SAW a sample: jumping 60_000 -> 300_000 closes the one
    // open window and opens the 300_000 one — the three untraded buckets between are simply
    // absent (a consolidator never invents a bar nothing happened in).
    let mut k = BarConsolidator::new(60_000);
    assert!(k.fold(&sample(60_000, 1.0, 1.0, 1.0, 1.0, 1.0)).is_none());
    let closed = k.fold(&sample(300_000, 2.0, 2.0, 2.0, 2.0, 1.0)).expect("closes prior");
    assert_eq!(closed.ts, 60_000);
    assert_eq!(k.forming().expect("open").ts, 300_000);
}

#[test]
fn an_out_of_order_sample_folds_in_and_never_closes_backwards() {
    // THE monotonic close rule (why vike_ibkr's `!=`-closing BarAggregator is not built on
    // this): a stale tick must not emit a backwards-stamped bar onto a live bar lane. It folds
    // into the OPEN window instead — extremes still update, the window start does not move.
    let mut k = BarConsolidator::new(60_000);
    k.fold(&sample(120_000, 1.0, 1.0, 1.0, 1.0, 1.0));
    assert!(k.fold(&sample(30_000, 9.9, 9.9, 0.1, 9.9, 2.0)).is_none(), "stale never closes");
    let f = k.forming().expect("window open");
    assert_eq!((f.ts, f.open, f.high, f.low, f.volume), (120_000, 1.0, 9.9, 0.1, 3.0));
}

#[test]
fn buckets_align_to_the_epoch_by_floor_mod_including_pre_epoch_ts() {
    // Python floor-mod (`rem_euclid`), NOT Rust `%`: a negative ts must floor DOWN to its
    // window start (-1 -> -60_000), not truncate toward zero (which would give 0).
    let mut k = BarConsolidator::new(60_000);
    k.fold(&sample(-1, 1.0, 1.0, 1.0, 1.0, 0.0));
    assert_eq!(k.forming().expect("open").ts, -60_000);
    // and mid-window ts floor to the window start, not to themselves
    let mut k = BarConsolidator::new(60_000);
    k.fold(&sample(61_234, 1.0, 1.0, 1.0, 1.0, 0.0));
    assert_eq!(k.forming().expect("open").ts, 60_000);
}

#[test]
fn a_one_price_tick_stream_folds_like_a_tick_bar_synth() {
    // The tick-lane shape (vike_mount's TickBarSynthesizer): each tick enters as a degenerate
    // one-price sample, so OHLC tracks the mid and volume stays exactly 0.0.
    let mut k = BarConsolidator::new(60_000);
    for (ts, px) in [(1_000, 0.50), (2_000, 0.40), (3_000, 0.50)] {
        assert!(k.fold(&sample(ts, px, px, px, px, 0.0)).is_none());
    }
    let closed = k.fold(&sample(61_000, 0.50, 0.50, 0.50, 0.50, 0.0)).expect("window 0 closes");
    assert_eq!(closed.ts, 0);
    assert_eq!(closed.open.to_bits(), 0.50f64.to_bits());
    assert_eq!(closed.high.to_bits(), 0.50f64.to_bits());
    assert_eq!(closed.low.to_bits(), 0.40f64.to_bits()); // the dip
    assert_eq!(closed.close.to_bits(), 0.50f64.to_bits());
    assert_eq!(closed.volume.to_bits(), 0.0f64.to_bits(), "one-price samples carry no volume");
}

#[test]
fn a_nonsensical_interval_cannot_divide_by_zero() {
    // The floor is a bare div-by-zero guard (callers apply their own sane default first).
    for interval in [0, -5] {
        let mut k = BarConsolidator::new(interval);
        assert_eq!(k.interval_ms(), 1);
        assert!(k.fold(&sample(7, 1.0, 1.0, 1.0, 1.0, 0.0)).is_none());
        assert_eq!(k.forming().expect("open").ts, 7);
    }
}
