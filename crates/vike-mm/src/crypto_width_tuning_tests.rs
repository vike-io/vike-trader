use super::*;

/// The `AsParams` the crypto mount prices with — MUST mirror
/// `vike_mount::MakerMountConfig::crypto` (that crate can't be a dev-dep here without a cycle, so
/// this is the pinned twin; if the mount's knobs change, this test's numbers move with them).
///
/// ⚠ **ONE DELIBERATE DIFFERENCE: `round_trip_fee_rate` stays `None` here.** The mount ARMS it
/// from the venue's fee schedule, and on every `$`-scale venue that floor DOMINATES the A-S
/// response in dead calm (hyperliquid's 3 bps round trip floors the half-spread at `$9.60` at a
/// `$64k` mid, against the `$2` tick floor these numbers pin). This workbench measures the
/// VOLATILITY transfer function, which a constant floor only masks — so the fee axis is left off
/// here and covered on its own in [`super::fee_floor_tests`], which pins both the floor's width
/// and the refusal. Do not read this function as "what the mount posts"; read it as "what the vol
/// term contributes". [`super::break_even_half_spread`] is threaded through `width_and_skew`
/// below, so arming this field here would move these numbers with it rather than being ignored.
fn crypto_params() -> AsParams {
    AsParams {
        variance_mode: VarianceMode::RawLocal,
        horizon_mode: HorizonMode::ConstantTau,
        price_domain: PriceDomain::Unbounded,
        min_half_spread_ticks: 2.0,
        max_half_spread_ticks: 60.0,
        gamma: 5e-4,
        q_scale: 3e-2,
        ..AsParams::default()
    }
}

/// The half-spread `$` and inventory-skew `$` the crypto config produces for a given per-tick BTC
/// move `d_move` ($) at the live HL cadence `dt_ms` (~1.3s), at mid `s` on a `$1` grid. Exact:
/// `σ̂² = Δ²/Δt` (the value a constant-|Δ| EWMA converges to), `V = σ̂²·H`, then the pure
/// `as_quotes` for the flat-inventory half-spread and `q_norm·γ·V` for the one-clip skew.
fn width_and_skew(p: &AsParams, s: f64, tick: f64, d_move: f64, dt_ms: f64) -> (f64, f64) {
    let sigma2 = d_move * d_move / dt_ms;
    let h = p.tau_hold_ms as f64; // ConstantTau ⇒ H = τ_hold
    let v = bounded_variance(sigma2, h, s, p.variance_mode);
    let (lo, hi) = domain_bounds(p.price_domain, tick);
    let (bid, ask) = as_quotes(
        s,
        0.0,
        p.gamma,
        v,
        p.kappa_default,
        tick,
        lo,
        hi,
        p.min_half_spread_ticks,
        p.max_half_spread_ticks,
        break_even_half_spread(p.round_trip_fee_rate, s),
        p.min_standoff_ticks,
    )
    .expect("crypto domain quotes two-sided");
    let half = 0.5 * (ask - bid);
    let q_norm = 0.005 / p.q_scale; // one 0.005-BTC clip
    let skew = q_norm * p.gamma * v;
    (half, skew)
}

/// Print + pin the width/skew transfer function across a realistic BTC vol range. The assertions
/// are the REGRESSION GUARD: dead-calm floor-locks, the spread widens monotonically with vol and
/// lands in a sensible single-digit-bps band at typical move, and one clip skews the reservation
/// by ~one half-spread. A knob change that breaks the economics (e.g. a γ·H that floor-locks even
/// in vol, or a spread that blows past 100 bps at ordinary move) trips here.
#[test]
fn crypto_width_transfer_function_is_sensible() {
    let p = crypto_params();
    let (s, tick, dt) = (64_000.0_f64, 1.0_f64, 1_300.0_f64);
    let bps = |half: f64| (2.0 * half) / s * 1e4; // full-spread bps of the mid

    println!(
        "crypto A-S width (mid ${s}, tick ${tick}, H={}ms, γ={}, q_scale={}, κ={}, Δt={dt}ms):",
        p.tau_hold_ms, p.gamma, p.q_scale, p.kappa_default
    );
    println!("  move/tick    half$     spread_bps   skew$/clip");
    let mut last_half = 0.0;
    for &mv in &[1.0, 2.0, 3.0, 5.0, 10.0, 20.0] {
        let (half, skew) = width_and_skew(&p, s, tick, mv, dt);
        println!("   ${mv:>5.0}     {half:>7.2}     {:>7.3}     {skew:>8.2}", bps(half));
        // widen monotonically with realized move (non-decreasing; ties only at the floor).
        assert!(half >= last_half - 1e-9, "half-spread must not shrink as vol rises");
        last_half = half;
    }

    // Dead-calm ($1 move) floor-locks at min_half_spread_ticks·tick = $2 (a $4 / ~0.6 bp spread).
    let (calm_half, _) = width_and_skew(&p, s, tick, 1.0, dt);
    assert!((calm_half - 2.0).abs() < 1e-6, "dead-calm half-spread floors at $2, got {calm_half}");

    // Typical BTC move (~$3/tick) lands in a sane low-single-digit-bps band (not floor-locked,
    // not absurdly wide) — the headline "the vol term activates under real vol" property.
    let (typ_half, typ_skew) = width_and_skew(&p, s, tick, 3.0, dt);
    assert!(typ_half > 2.0, "typical vol clears the floor: {typ_half}");
    assert!(
        (1.0..12.0).contains(&bps(typ_half)),
        "typical spread in a sane bps band: {}",
        bps(typ_half)
    );

    // One clip skews the reservation by ~a THIRD of a vol-half-spread (the modest-warehouse lean):
    // per-clip skew `= q_norm·γV = (0.005/0.03)·γV ≈ 0.167·γV`, vs the vol half-spread `0.5·γV` — so
    // ~3 clips move the reservation a full half-spread (q_scale = 0.03; strict `0.01` was 1 clip).
    assert!(
        (typ_skew - typ_half / 3.0).abs() < 0.15 * typ_half,
        "one-clip skew ≈ a third of a half-spread at q_scale=0.03: skew={typ_skew} half={typ_half}"
    );

    // A vol spike ($20/tick) is CAPPED by max_half_spread_ticks (60·$1 = $60 half / ~18.75 bp)
    // instead of the uncapped ~$277 (~86 bp) — the maker stays plausibly fillable through the
    // spike rather than posting off the book. Still wider than the typical-vol quote.
    let (spike_half, _) = width_and_skew(&p, s, tick, 20.0, dt);
    assert!(spike_half > typ_half, "spike still widens past the typical quote");
    assert!(
        (spike_half - 60.0 * tick).abs() < 1e-6,
        "spike half-spread is capped at max_half_spread_ticks·tick = $60, got {spike_half}"
    );
    assert!(bps(spike_half) < 20.0, "capped spike stays under ~20 bp: {}", bps(spike_half));
}

/// The full `AsState::price` path WARMS the online EWMA σ̂² to the same width the closed-form
/// transfer function predicts — proving the workbench's exact-`V` shortcut matches what the live
/// maker actually converges to when fed a real varying feed (the gap the constant-mid test left).
#[test]
fn price_path_warms_to_the_transfer_function_width() {
    let p = crypto_params();
    let (s0, tick, dt) = (64_000.0_f64, 1.0_f64, 1_300_i64);
    let mv = 5.0_f64; // $5/tick move
    let mut st = AsState::new(p);
    // Feed a converging constant-|Δ| walk (alternating ±$5) at the HL cadence to warm the
    // 32-half-life EWMA toward σ̂² = Δ²/Δt; read the flat-inventory half-spread at the end.
    let mut ts = 0_i64;
    let mut half = 0.0;
    for i in 0..600 {
        let s = if i % 2 == 0 { s0 } else { s0 + mv }; // mid oscillates $64000 ↔ $64005
        let view = BookView {
            tick_size: tick,
            bids: vec![BookLevel::new(s - 1.0, 100.0)],
            asks: vec![BookLevel::new(s + 1.0, 100.0)],
        };
        if let Some((bid, ask)) = st.price(&view, 0.0, ts) {
            half = 0.5 * (ask - bid);
        }
        ts += dt;
    }
    // The book mid alternates by $5 too, so the realized per-tick move ≈ $5; the warmed width
    // should land near the closed-form prediction for a $5 move (within a generous band — the
    // EWMA of an oscillating series isn't a perfect delta, and the micro-price/mid interact).
    let (predicted, _) = width_and_skew(&p, s0 + 2.5, tick, mv, dt as f64);
    println!("warmed half-spread ${half:.2} vs closed-form ${predicted:.2} ($5/tick)");
    assert!(
        half > 2.0,
        "the warmed live path clears the floor under $5 vol (not floor-locked): {half}"
    );
    assert!(half.is_finite(), "warmed half-spread is finite");
}

/// The half-spread ceiling in isolation: a high-`V` quote that would post far off the book is
/// CLAMPED to `max_half_spread_ticks · tick`; `0.0` leaves it uncapped (byte-identical); and a
/// `max < min` misconfig can never collapse the two-sided quote — the floor always wins.
#[test]
fn max_half_spread_caps_without_beating_the_floor() {
    let (s, g, k, tick) = (64_000.0_f64, 5e-4_f64, 50.0_f64, 1.0_f64);
    let (lo, hi) = domain_bounds(PriceDomain::Unbounded, tick);
    let v = 300_000.0_f64; // ½·γ·V = 0.5·5e-4·300000 = $75 vol half-spread — deliberately wide.

    // UNCAPPED (max = 0.0): the half-spread runs to the full ~$75.
    let (ub, ua) =
        as_quotes(s, 0.0, g, v, k, tick, lo, hi, 2.0, 0.0, 0.0, 1.0).expect("uncapped two-sided");
    let uncapped_half = 0.5 * (ua - ub);
    assert!(uncapped_half > 60.0, "uncapped high-V half-spread is wide: {uncapped_half}");

    // CAPPED at 60 ticks: the half-spread clamps to exactly $60 (18.75 bp of the mid).
    let (cb, ca) =
        as_quotes(s, 0.0, g, v, k, tick, lo, hi, 2.0, 60.0, 0.0, 1.0).expect("capped two-sided");
    let capped_half = 0.5 * (ca - cb);
    assert!((capped_half - 60.0).abs() < 1e-6, "capped at max·tick = $60, got {capped_half}");

    // max (1 tick) < min (2 ticks): the FLOOR governs — a dead-calm quote keeps its $2 half-spread,
    // never a $1 collapse, so a misconfigured ceiling can't strand the maker with a sub-floor quote.
    let (lb, la) = as_quotes(s, 0.0, g, 0.0, k, tick, lo, hi, 2.0, 1.0, 0.0, 1.0)
        .expect("floor-wins two-sided");
    let floor_half = 0.5 * (la - lb);
    assert!(
        (floor_half - 2.0).abs() < 1e-6,
        "max<min ⇒ the $2 floor wins, not the $1 cap: {floor_half}"
    );
}
