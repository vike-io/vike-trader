//! The opt-in fat-finger price collar: its band, per-symbol override and covered-reduce bypass.

use super::*;
use vike_model::PriceCollar;

// ---- fat-finger price collar (OPT-IN) ----

/// `pct` fraction + absolute floor, as the venue-wide default (no per-symbol rows).
fn collared(pct: f64, abs_floor: f64) -> RiskLimits {
    RiskLimits { price_collar: Some(PriceCollar { pct, abs_floor }), ..RiskLimits::new() }
}

/// A stop/take-profit carries `trigger_price` and NO `price` — the collar must judge it too.
fn trigger_order(side: i32, qty: f64, trigger: f64) -> OrderRequest {
    OrderRequest {
        order_type: "stop".to_string(),
        trigger_price: Some(trigger),
        ..market(side, qty)
    }
}

#[test]
fn price_collar_denies_a_fat_finger_in_both_directions() {
    // mark 100, band = max(0.05 * 100, 0.0) = 5.0
    let ctx = RiskContext { mark_price: 100.0, ..RiskContext::default() };
    // 10x UP — the classic mis-scale. EVERY pre-existing axis admits it (no cap is set, and
    // its notional would fit under an ordinary one anyway): this is the hole the axis closes.
    let mut g = RiskGate::new(collared(0.05, 0.0));
    let v = g.check(&limit(1, 1.0, 1_000.0), &ctx);
    assert!(!v.ok && v.reason == "price-collar", "a 10x-up limit must deny: {v:?}");
    // 10x DOWN — a sell at a tenth of the mark is the same fat finger
    let v = g.check(&limit(-1, 1.0, 10.0), &ctx);
    assert!(!v.ok && v.reason == "price-collar", "a 10x-down limit must deny: {v:?}");
    // a hair outside the band on each side
    let v = g.check(&limit(1, 1.0, 105.01), &ctx);
    assert!(!v.ok && v.reason == "price-collar", "just above the band: {v:?}");
    let v = g.check(&limit(-1, 1.0, 94.99), &ctx);
    assert!(!v.ok && v.reason == "price-collar", "just below the band: {v:?}");
    // a TRIGGER price is judged the same way (a stop carries price = None)
    let v = g.check(&trigger_order(1, 1.0, 200.0), &ctx);
    assert!(!v.ok && v.reason == "price-collar", "a fat-finger trigger must deny: {v:?}");
    // a non-finite price is denied outright, never silently admitted (`NaN > band` is false)
    let v = g.check(&limit(1, 1.0, f64::NAN), &ctx);
    assert!(!v.ok && v.reason == "price-collar", "a NaN price must deny: {v:?}");

    // a collar denial must not burn a rate slot — the axis runs BEFORE the throttle, like
    // the margin and impact vetoes.
    let mut lim = collared(0.05, 0.0);
    lim.max_orders_per_window = Some(1);
    let mut t = RiskGate::new(lim);
    assert!(!t.check(&limit(1, 1.0, 1_000.0), &ctx).ok);
    assert!(t.throttle_times().is_empty(), "a collared order must not burn a rate slot");
    assert!(t.check(&limit(1, 1.0, 100.0), &ctx).ok, "the slot must still be available");
}

#[test]
fn price_collar_allows_inside_the_band() {
    let ctx = RiskContext { mark_price: 100.0, ..RiskContext::default() };
    let mut g = RiskGate::new(collared(0.05, 0.0));
    for px in [96.0, 100.0, 104.0] {
        let v = g.check(&limit(1, 1.0, px), &ctx);
        assert!(v.ok, "price {px} is inside the +/-5 band: {v:?}");
    }
    // band-EQUAL passes on both edges (the comparison is strict `>`)
    let v = g.check(&limit(1, 1.0, 105.0), &ctx);
    assert!(v.ok, "the upper band edge must pass: {v:?}");
    let v = g.check(&limit(-1, 1.0, 95.0), &ctx);
    assert!(v.ok, "the lower band edge must pass: {v:?}");
    // an in-band trigger passes too
    let v = g.check(&trigger_order(-1, 1.0, 96.0), &ctx);
    assert!(v.ok, "an in-band trigger must pass: {v:?}");
    // a MARKET order carries neither price nor trigger => never collared, at any band
    let mut zero = RiskGate::new(collared(0.0, 0.0));
    let v = zero.check(&market(1, 1.0), &ctx);
    assert!(v.ok, "a market order has no price to collar: {v:?}");
}

/// Why BOTH halves of the band are required: on a cheap instrument (the Polymarket shape — a
/// 0.02 mark) a pure percentage collar denies ordinary quoting, so the absolute floor must
/// dominate. Above it the percentage takes back over.
#[test]
fn price_collar_absolute_floor_dominates_at_a_tiny_mark() {
    let ctx = RiskContext { mark_price: 0.02, ..RiskContext::default() };
    // pct alone: 10% of 0.02 = 0.002, so an ordinary 0.025 quote would be DENIED
    let v = RiskGate::new(collared(0.10, 0.0)).check(&limit(1, 100.0, 0.025), &ctx);
    assert!(!v.ok && v.reason == "price-collar", "a pct-only collar is too tight: {v:?}");
    // with a 0.01 absolute floor the band is max(0.002, 0.01) = 0.01 => 0.025 is admitted
    let v = RiskGate::new(collared(0.10, 0.01)).check(&limit(1, 100.0, 0.025), &ctx);
    assert!(v.ok, "the absolute floor must widen the band at a tiny mark: {v:?}");
    // ...and the mis-scale this axis exists for is STILL caught: 0.55 -> 5.5 in miniature,
    // here 0.02 -> 0.055, which is 0.035 away
    let v = RiskGate::new(collared(0.10, 0.01)).check(&limit(1, 100.0, 0.055), &ctx);
    assert!(!v.ok && v.reason == "price-collar", "a mis-scaled 0.055 must deny: {v:?}");
    // the DOWN mis-scale (0.02 -> 0.002) likewise
    let v = RiskGate::new(collared(0.10, 0.01)).check(&limit(-1, 100.0, 0.002), &ctx);
    assert!(!v.ok && v.reason == "price-collar", "a mis-scaled 0.002 must deny: {v:?}");
    // symmetrically, on an EXPENSIVE mark the percentage dominates that same 0.01 floor:
    // band = max(0.10 * 50_000, 0.01) = 5_000, so 52_000 is admitted.
    let rich = RiskContext { mark_price: 50_000.0, ..RiskContext::default() };
    let v = RiskGate::new(collared(0.10, 0.01)).check(&limit(1, 1.0, 52_000.0), &rich);
    assert!(v.ok, "the percentage must govern an expensive mark: {v:?}");
    // the band fn itself, pinned at both ends
    let c = PriceCollar { pct: 0.10, abs_floor: 0.01 };
    assert_eq!(c.band(50_000.0), 5_000.0);
    assert_eq!(c.band(0.02), 0.01);
}

/// An UNPRICED mark skips the axis entirely — the mount's readiness gate guarantees
/// priced-before-order-flow, so denying here would be a pure false positive.
#[test]
fn price_collar_skips_an_unpriced_mark() {
    for mark in [0.0, -1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let ctx = RiskContext { mark_price: mark, ..RiskContext::default() };
        let v = RiskGate::new(collared(0.05, 0.0)).check(&limit(1, 1.0, 1_000.0), &ctx);
        assert!(v.ok, "an unpriced mark ({mark}) must skip the collar: {v:?}");
    }
}

/// A COMBO's `price` is the SIGNED NET across legs, not a price in any single instrument's
/// mark units — collaring it would deny every combo (a credit net is NEGATIVE). Its LEGS are
/// not silently un-checked either: `check_combo` clears each leg's price/trigger, so a leg
/// carries nothing to collar and prices off its own mark exactly as it does today.
#[test]
fn price_collar_never_judges_a_combo_net_price() {
    let ctx = RiskContext { mark_price: 100.0, ..RiskContext::default() };
    // a +20 debit net and a -20 credit net are both far outside a +/-5 band around 100
    let v = RiskGate::new(collared(0.05, 0.0)).check(&combo_limit(1, 2.0, 20.0), &ctx);
    assert!(v.ok, "a debit combo net must not be collared: {v:?}");
    let v = RiskGate::new(collared(0.05, 0.0)).check(&combo_limit(-1, 2.0, -20.0), &ctx);
    assert!(v.ok, "a credit combo net must not be collared: {v:?}");
    // the combo entry point is likewise unaffected
    let mut g = RiskGate::new(collared(0.05, 0.0));
    let v = g.check_combo(&combo_limit(1, 2.0, 20.0), &RiskContext::default(), leg_marks);
    assert!(v.ok, "check_combo must be unaffected by an armed collar: {v:?}");
}

/// Per-symbol override, mirroring `im_by_symbol`/`im_for`: the map wins where it has a row,
/// the venue default covers everything else.
#[test]
fn price_collar_per_symbol_override_mirrors_im_by_symbol() {
    let ctx = RiskContext { mark_price: 100.0, ..RiskContext::default() };
    let row = PriceCollar { pct: 0.05, abs_floor: 0.0 };
    let tight = PriceCollar { pct: 0.0, abs_floor: 0.0 };
    let mut map: indexmap::IndexMap<String, PriceCollar> = indexmap::IndexMap::new();
    map.insert("BTCUSDT".to_string(), row);

    // no venue default at all — only a BTCUSDT row (the symbol `market()`/`limit()` build)
    let only_btc = RiskLimits { collar_by_symbol: map.clone(), ..RiskLimits::new() };
    assert_eq!(only_btc.collar_for("BTCUSDT"), Some(row));
    assert_eq!(only_btc.collar_for("ETHUSDT"), None);
    let v = RiskGate::new(only_btc.clone()).check(&limit(1, 1.0, 1_000.0), &ctx);
    assert!(!v.ok && v.reason == "price-collar", "the BTC row must arm the axis: {v:?}");
    // an uncovered symbol with no venue default stays unarmed
    let mut eth = limit(1, 1.0, 1_000.0);
    eth.symbol = "ETHUSDT".to_string();
    let v = RiskGate::new(only_btc).check(&eth, &ctx);
    assert!(v.ok, "a symbol with no row and no default must be unarmed: {v:?}");

    // the per-symbol row OVERRIDES a brutally tight venue default...
    let mixed =
        RiskLimits { price_collar: Some(tight), collar_by_symbol: map, ..RiskLimits::new() };
    assert_eq!(mixed.collar_for("BTCUSDT"), Some(row));
    assert_eq!(mixed.collar_for("ETHUSDT"), Some(tight));
    let v = RiskGate::new(mixed.clone()).check(&limit(1, 1.0, 104.0), &ctx);
    assert!(v.ok, "the per-symbol row must override the venue default: {v:?}");
    // ...while the tight default still governs every OTHER symbol
    let mut eth2 = limit(1, 1.0, 104.0);
    eth2.symbol = "ETHUSDT".to_string();
    let v = RiskGate::new(mixed).check(&eth2, &ctx);
    assert!(!v.ok && v.reason == "price-collar", "the default must govern ETH: {v:?}");
}

/// THE OFF TEST — the byte-identical-when-unset guarantee, both halves:
/// (a) with the axis `None`/empty an order that WOULD trip it is admitted, and
/// (b) neither new key reaches the canonical JSON, so `EngineSnapshot`'s `state_hash` (the
///     journal determinism fence) is unchanged — see
///     `engine_snapshot::tests::state_hash_of_a_default_limits_snapshot_is_pinned`.
#[test]
fn price_collar_off_is_byte_identical() {
    let ctx = RiskContext { mark_price: 100.0, ..RiskContext::default() };
    let off = RiskLimits::new();
    assert_eq!(off.price_collar, None);
    assert!(off.collar_by_symbol.is_empty());
    assert_eq!(off.collar_for("BTCUSDT"), None);
    // a 100x limit, a 100x trigger and a NaN price — all admitted with the axis off
    let cases = [limit(1, 1.0, 10_000.0), trigger_order(1, 1.0, 10_000.0), limit(1, 1.0, f64::NAN)];
    for req in cases {
        let v = RiskGate::new(RiskLimits::new()).check(&req, &ctx);
        assert!(v.ok, "the OFF path must admit exactly as before: {req:?} -> {v:?}");
    }
    // the serialized shape is untouched: no key, not even a null
    let json = serde_json::to_string(&RiskLimits::new()).unwrap();
    assert!(
        !json.contains("price_collar") && !json.contains("collar_by_symbol"),
        "an off collar must not reach the canonical JSON: {json}"
    );
    // `Default` (not just `new()`) is off too — it is what serde reconstructs a pre-collar
    // journal record into, and the round trip must land back on the same value.
    let d = RiskLimits::default();
    assert_eq!(d.price_collar, None);
    assert!(d.collar_by_symbol.is_empty());
    let round: RiskLimits = serde_json::from_str(&json).unwrap();
    assert_eq!(round, RiskLimits::new());
}

/// An ARMED COLLAR MUST NOT DENY A PROTECTIVE BRACKET EXIT: `vike_model::build_bracket`'s
/// stop-loss (`order_type: "stop"`, `trigger_price`) and take-profit (`limit`, `price`) legs sit
/// DELIBERATELY far from the mark, and the covered-reduce bypass is what admits them.
#[test]
fn price_collar_never_denies_a_protective_bracket_exit() {
    use vike_model::BracketSpec;
    // long 1.0 at a mark of 100, under a brutally tight +/-2 collar
    let held = RiskContext { position_size: 1.0, mark_price: 100.0, ..RiskContext::default() };
    // a MARKET entry (no price to collar), a stop 10 below the mark and a take-profit 20
    // above it — both exits 5x/10x outside the +/-2 band, BY DESIGN.
    let spec = BracketSpec {
        venue: "binance".to_string(),
        symbol: "BTCUSDT".to_string(),
        side: 1,
        qty: 1.0,
        entry_price: None,
        stop_loss: 90.0,
        take_profit: 120.0,
    };
    let [entry, sl, tp] = vike_model::build_bracket(&spec, "e1", "s1", "t1");
    // the exits are reduce-only, opposite-side and covered by the held position
    assert!(sl.reduce_only && sl.trigger_price == Some(90.0) && sl.order_type == "stop");
    assert!(tp.reduce_only && tp.price == Some(120.0) && tp.order_type == "limit");
    for leg in [&entry, &sl, &tp] {
        let v = RiskGate::new(collared(0.02, 0.0)).check(leg, &held);
        assert!(v.ok, "an armed collar must not deny a bracket leg: {leg:?} -> {v:?}");
    }

    // ...and the bypass is POSITION-COVERED, not flag-trusting: on a FLAT book the very same
    // legs open exposure (nothing server-side reduces either), so the collar still bites.
    let flat = RiskContext { mark_price: 100.0, ..RiskContext::default() };
    for leg in [&sl, &tp] {
        let v = RiskGate::new(collared(0.02, 0.0)).check(leg, &flat);
        assert!(
            !v.ok && v.reason == "price-collar",
            "a flat-book 'reduce_only' leg is an OPENING order and must stay collared: {v:?}"
        );
    }

    // the OPENING leg is never bypassed either: a fat-fingered LIMIT entry still denies.
    let mut far_entry = spec.clone();
    far_entry.entry_price = Some(1_000.0);
    let [bad_entry, _, _] = vike_model::build_bracket(&far_entry, "e2", "s2", "t2");
    let v = RiskGate::new(collared(0.02, 0.0)).check(&bad_entry, &held);
    assert!(!v.ok && v.reason == "price-collar", "a fat-finger entry must deny: {v:?}");
}

/// A garbage collar config must FAIL OPEN, never closed: a negative/`NaN` band makes
/// `|p - mark| > band` always true, turning the opt-in knob into a TOTAL KILL SWITCH.
#[test]
fn price_collar_garbage_config_is_never_a_kill_switch() {
    let ctx = RiskContext { mark_price: 100.0, ..RiskContext::default() };
    let garbage = [
        PriceCollar { pct: f64::NAN, abs_floor: 0.0 },
        PriceCollar { pct: -0.5, abs_floor: 0.0 },
        PriceCollar { pct: 0.0, abs_floor: f64::NAN },
        PriceCollar { pct: 0.0, abs_floor: -1.0 },
        PriceCollar { pct: f64::NAN, abs_floor: f64::NAN },
        PriceCollar { pct: -0.5, abs_floor: -1.0 },
        PriceCollar { pct: f64::NEG_INFINITY, abs_floor: f64::NEG_INFINITY },
    ];
    for c in garbage {
        let band = c.band(100.0);
        assert!(band.is_finite() && band >= 0.0, "{c:?} must yield a sane band, got {band}");
        let lim = RiskLimits { price_collar: Some(c), ..RiskLimits::new() };
        // an AT-THE-MARK limit — the order a kill switch would have denied — is admitted
        let v = RiskGate::new(lim.clone()).check(&limit(1, 1.0, 100.0), &ctx);
        assert!(v.ok, "{c:?} must not deny an at-the-mark limit: {v:?}");
        // so is an at-the-mark trigger, and a market order (no price at all)
        let v = RiskGate::new(lim.clone()).check(&trigger_order(-1, 1.0, 100.0), &ctx);
        assert!(v.ok, "{c:?} must not deny an at-the-mark trigger: {v:?}");
        let v = RiskGate::new(lim).check(&market(1, 1.0), &ctx);
        assert!(v.ok, "{c:?} must not deny a market order: {v:?}");
    }
    // a garbage half does not poison a VALID half: the surviving component still governs
    assert_eq!(PriceCollar { pct: f64::NAN, abs_floor: 0.02 }.band(100.0), 0.02);
    assert_eq!(PriceCollar { pct: -0.5, abs_floor: 0.02 }.band(100.0), 0.02);
    // ...and the two WHOLLY-garbage shapes (an unclamped `NaN` and NEGATIVE band) collapse to the
    // TIGHTEST LEGAL band (0.0), what an explicit `PriceCollar { pct: 0.0, abs_floor: 0.0 }` means.
    assert_eq!(PriceCollar { pct: f64::NAN, abs_floor: f64::NAN }.band(100.0), 0.0);
    assert_eq!(PriceCollar { pct: -0.5, abs_floor: -1.0 }.band(100.0), 0.0);
}
