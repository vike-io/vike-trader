use super::*;

// The all-zero bag is the OFF default — both knobs `0.0`.
#[test]
fn default_is_the_all_zero_off_bag() {
    let d = ToxicityParams::default();
    assert_eq!(d.widen.to_bits(), 0.0_f64.to_bits(), "widen defaults to 0.0");
    assert_eq!(d.size_cut.to_bits(), 0.0_f64.to_bits(), "size_cut defaults to 0.0");
    assert_eq!(d, ToxicityParams { widen: 0.0, size_cut: 0.0 }, "the OFF bag");
}

// A minimal SpreadMakerParams with `toxicity` set to `t` — every OTHER sub-bag left OFF/None so
// the test isolates the toxicity serde contract.
fn params_with_toxicity(t: Option<ToxicityParams>) -> SpreadMakerParams {
    SpreadMakerParams {
        qty: 1.0,
        half_spread: 0.5,
        target_inventory: 0.0,
        max_inventory: 1.0,
        skew: 0.0,
        fill_window_ms: 0,
        net_fill_threshold: 0.0,
        suppress_cooldown_ms: 0,
        style: QuoteStyle::Mid,
        depth_levels: 1,
        tick_size: 0.0,
        filter_own: false,
        avellaneda_stoikov: None,
        refresh_tolerance: None,
        ladder: None,
        reward: None,
        toxicity: t,
    }
}

// Backward-compat: a payload that OMITS `toxicity` decodes to `toxicity: None` (the additive
// `#[serde(default)]` contract, so old journals / GUI payloads still decode).
#[test]
fn omitted_toxicity_decodes_to_none() {
    let json = serde_json::json!({
        "qty": 1.0, "half_spread": 0.5, "target_inventory": 0.0, "max_inventory": 1.0,
        "skew": 0.0, "fill_window_ms": 0, "net_fill_threshold": 0.0, "suppress_cooldown_ms": 0,
        "style": "Mid", "depth_levels": 1, "tick_size": 0.0, "filter_own": false
    });
    let p: SpreadMakerParams = serde_json::from_value(json).unwrap();
    assert_eq!(p.toxicity, None, "an absent toxicity key decodes as None");
}

// OFF (`None`) serializes WITHOUT a `toxicity` key (the `skip_serializing_if` contract keeps an
// OFF maker's payload byte-identical to its pre-feature bytes).
#[test]
fn off_toxicity_is_skipped_on_serialize() {
    let js = serde_json::to_string(&params_with_toxicity(None)).unwrap();
    assert!(!js.contains("toxicity"), "None omits the toxicity key entirely: {js}");
}

// A `Some(ToxicityParams{..})` bag round-trips through JSON unchanged (and DOES emit the key).
#[test]
fn some_toxicity_round_trips() {
    let p = params_with_toxicity(Some(ToxicityParams { widen: 1.5, size_cut: 0.8 }));
    let js = serde_json::to_string(&p).unwrap();
    assert!(js.contains("toxicity"), "an ON bag emits the key: {js}");
    let back: SpreadMakerParams = serde_json::from_str(&js).unwrap();
    assert_eq!(back.toxicity, Some(ToxicityParams { widen: 1.5, size_cut: 0.8 }));
}
