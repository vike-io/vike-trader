use super::*;

// The generalization default: the domain is UnitInterval (Polymarket 0–1) with a zero
// half-spread floor, so a fresh A-S maker prices in the exact domain it always did.
#[test]
fn default_domain_is_the_unit_interval_with_no_floor() {
    assert_eq!(PriceDomain::default(), PriceDomain::UnitInterval);
    let p = AsParams::default();
    assert_eq!(p.price_domain, PriceDomain::UnitInterval, "default domain = Polymarket [0,1]");
    assert_eq!(p.min_half_spread_ticks.to_bits(), 0.0_f64.to_bits(), "default floor = 0 (off)");
}

// Backward-compat: an AsParams blob that OMITS `price_domain`/`min_half_spread_ticks` (an old
// journal / GUI payload predating the crypto generalization) decodes to the byte-identical
// defaults — the additive `#[serde(default)]` contract the other A-S knobs
// (terminal/underlying/atm) keep. Every value here matches `AsParams::default`, so the decoded
// struct must EQUAL it.
#[test]
fn omitted_domain_fields_decode_to_the_byte_identical_defaults() {
    let json = serde_json::json!({
        "gamma": 0.1, "horizon_mode": "TimeToResolution", "tau_hold_ms": 3_600_000,
        "resolution_ts": null, "resolution_blackout_ms": 60_000, "variance_mode": "LocalCapped",
        "kappa_mode": "Fixed", "kappa_default": 50.0, "kappa_min": 1.0, "kappa_max": 1_000.0,
        "sigma_half_life": 32.0, "trade_window_ms": 60_000, "n_min": 20, "q_scale": 100.0,
        "min_standoff_ticks": 1.0, "use_micro_price": false
    });
    let p: AsParams = serde_json::from_value(json).unwrap();
    assert_eq!(p.price_domain, PriceDomain::UnitInterval, "absent price_domain ⇒ UnitInterval");
    assert_eq!(p.min_half_spread_ticks.to_bits(), 0.0_f64.to_bits(), "absent floor ⇒ 0.0");
    assert_eq!(
        p.max_half_spread_ticks.to_bits(),
        0.0_f64.to_bits(),
        "absent ceiling ⇒ 0.0 (uncapped)"
    );
    assert_eq!(p, AsParams::default(), "the omitted-field payload equals the default AsParams");
}

// The Unbounded / Band domains round-trip through JSON unchanged (Band carries its f64 walls),
// and a full AsParams tuned for the crypto domain (Unbounded + a 2-tick floor) round-trips too.
#[test]
fn domain_variants_round_trip() {
    for d in [
        PriceDomain::UnitInterval,
        PriceDomain::Unbounded,
        PriceDomain::Band { lo: 64_000.0, hi: 65_000.0 },
    ] {
        let js = serde_json::to_string(&d).unwrap();
        assert_eq!(serde_json::from_str::<PriceDomain>(&js).unwrap(), d, "round-trip: {js}");
    }
    let p = AsParams {
        price_domain: PriceDomain::Unbounded,
        min_half_spread_ticks: 2.0,
        ..AsParams::default()
    };
    let back: AsParams = serde_json::from_str(&serde_json::to_string(&p).unwrap()).unwrap();
    assert_eq!(back, p, "AsParams with the crypto domain round-trips");
}
