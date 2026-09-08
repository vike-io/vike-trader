//! CPython-pinned parity gate for the vike-options port. Oracle:
//! `vike-trader-app data/options/{greeks,model,columns}.py` on CPython 3.14
//! (`tests/unit/data/test_options_{greeks,model,columns}.py` are the ported shapes; the
//! pinned constants below were probed from the live oracle venv on 2026-07-07 and recorded
//! as IEEE-754 hex bit patterns).
//!
//! Tiers (fixtures/README.md): pure-arithmetic paths assert EXACT (`to_bits` / string
//! equality); erf/exp/log-derived outputs (BS price, greeks, implied vol, theor) gate at
//! ≤1e-12 relative — the prescribed tier for transcendental-derived sites (empirically the
//! libm-based port is bit-identical to the oracle on every pinned probe on the dev box;
//! platform exp/log may wobble ≤1 ulp on other targets). NEVER widen either tier.

use vike_options::columns::{
    CHAIN_FIELDS, GREEKS_FIELDS, cell_value, fmt, fmt_strike, header, kind,
};
use vike_options::{
    AssetClass, Expiry, OptionChain, OptionKind, OptionQuote, StrikeRow, black_scholes_greeks,
    black_scholes_price, enrich_quote, expiry_ms, implied_vol, limit_strikes, make_expiry,
    years_to_expiry,
};

const C: OptionKind = OptionKind::Call;
const P: OptionKind = OptionKind::Put;

/// erf/exp/log-derived tier: ≤1e-12 relative against a CPython-pinned bit pattern.
fn assert_rel(what: &str, got: f64, oracle_bits: u64) {
    let oracle = f64::from_bits(oracle_bits);
    let rel = if oracle != 0.0 { ((got - oracle) / oracle).abs() } else { (got - oracle).abs() };
    assert!(
        rel <= 1e-12,
        "{what}: got {got:e} ({:016x}), oracle {oracle:e} ({oracle_bits:016x}), rel {rel:e}",
        got.to_bits()
    );
}

/// Pure-arithmetic tier: exact bits.
fn assert_bits(what: &str, got: f64, oracle_bits: u64) {
    assert_eq!(
        got.to_bits(),
        oracle_bits,
        "{what}: got {got:e} ({:016x}), oracle bits {oracle_bits:016x}",
        got.to_bits()
    );
}

// ---- greeks.py ----

#[test]
fn bs_price_atm_reference() {
    // S=K=100, t=1, sigma=0.20, r=0 -> call == put == 7.965567455405804
    let c = black_scholes_price(100.0, 100.0, 1.0, 0.20, C, 0.0).unwrap();
    let p = black_scholes_price(100.0, 100.0, 1.0, 0.20, P, 0.0).unwrap();
    assert_rel("call_atm", c, 0x401fdcbdb70c3310);
    assert_rel("put_atm", p, 0x401fdcbdb70c3310);
}

#[test]
fn bs_greeks_atm_reference() {
    let (d, g, t, v) = black_scholes_greeks(100.0, 100.0, 1.0, 0.20, C, 0.0).unwrap();
    assert_rel("call_delta", d, 0x3fe1464507526871); // 0.539827837277029
    assert_rel("gamma", g, 0x3f9452efb9e5417e); // 0.01984762737385059
    assert_rel("theta_per_day", t, 0xbf8645d91fe2b0fa); // -0.010875412259644158
    assert_rel("vega_per_point", v, 0x3fd967aba85e91dd); // 0.39695254747701175
    let (dp, gp, tp, vp) = black_scholes_greeks(100.0, 100.0, 1.0, 0.20, P, 0.0).unwrap();
    assert_rel("put_delta", dp, 0xbfdd7375f15b2f1e); // -0.460172162722971
    // at r=0 the put shares the call's gamma/vega/theta; put delta = call delta - 1 (exact op)
    assert_bits("put_gamma==call", gp, g.to_bits());
    assert_bits("put_theta==call", tp, t.to_bits());
    assert_bits("put_vega==call", vp, v.to_bits());
    assert_bits("put_delta identity", dp, (d - 1.0).to_bits());
}

#[test]
fn bs_nonzero_r_reference() {
    // r=0.04 exercises the exp(-r t) discount + both theta branches
    let c = black_scholes_price(105.0, 98.0, 0.5, 0.35, C, 0.04).unwrap();
    let p = black_scholes_price(105.0, 98.0, 0.5, 0.35, P, 0.04).unwrap();
    assert_rel("call_r", c, 0x402e00106ae95e74); // 15.000125256526395
    assert_rel("put_r", p, 0x40183d0688e54c50); // 6.059595240588422
    let (d, g, t, v) = black_scholes_greeks(105.0, 98.0, 0.5, 0.35, P, 0.04).unwrap();
    assert_rel("put_delta_r", d, 0xbfd41fa33d0e5696);
    assert_rel("put_gamma_r", g, 0x3f8bf99b740e813b);
    assert_rel("put_theta_r", t, 0xbf957e3f2577f411);
    assert_rel("put_vega_r", v, 0x3fd0ddf213d2f204);
}

#[test]
fn bs_invalid_inputs_are_none() {
    assert_eq!(black_scholes_greeks(100.0, 100.0, 1.0, 0.0, C, 0.0), None); // sigma<=0
    assert_eq!(black_scholes_greeks(100.0, 100.0, 0.0, 0.2, C, 0.0), None); // t<=0
    assert_eq!(black_scholes_greeks(0.0, 100.0, 1.0, 0.2, C, 0.0), None); // S<=0
    assert_eq!(black_scholes_price(100.0, 0.0, 1.0, 0.2, C, 0.0), None); // K<=0
    assert_eq!(black_scholes_price(100.0, 100.0, 1.0, 0.0, C, 0.0), None);
}

#[test]
fn deep_otm_price_is_exactly_zero() {
    // d1/d2 ≈ -8.6/-6.1: erf saturates to -1 exactly, so the price is EXACTLY 0.0 (not tiny).
    // This is the erf-tail edge — exact on every platform, so it pins at the exact tier.
    let c = black_scholes_price(60000.0, 100000.0, 3.0 / 365.0, 0.65, C, 0.0).unwrap();
    assert_bits("deep_otm", c, 0x0000000000000000);
}

#[test]
fn implied_vol_bisection_reference() {
    // round-trip: price(sigma=0.20) -> bisection lands on 0.19999999671876428
    let price = black_scholes_price(100.0, 100.0, 1.0, 0.20, C, 0.0).unwrap();
    let iv = implied_vol(price, 100.0, 100.0, 1.0, C, 0.0).unwrap();
    assert_rel("iv_roundtrip", iv, 0x3fc99999928db8ba);
    assert!((iv - 0.20).abs() < 1e-3, "oracle test shape: approx 0.20");
    // a real solve away from the round-trip: 0.43734273922469
    let iv2 = implied_vol(1234.5, 60000.0, 62000.0, 14.0 / 365.0, C, 0.0).unwrap();
    assert_rel("iv_solve", iv2, 0x3fdbfd6c66873d08);
}

#[test]
fn implied_vol_unsolvable_is_none() {
    assert_eq!(implied_vol(5.0, 120.0, 100.0, 1.0, C, 0.0), None); // 5 < intrinsic (~20)
    assert_eq!(implied_vol(0.0, 100.0, 100.0, 1.0, C, 0.0), None);
    assert_eq!(implied_vol(10.0, 100.0, 100.0, 0.0, C, 0.0), None); // t<=0
}

#[test]
fn years_to_expiry_30_days_and_clamp() {
    // oracle pins: _expiry_ms("2026-07-02") == 1782979200000 (08:00 UTC settle)
    assert_eq!(expiry_ms("2026-07-02"), 1_782_979_200_000);
    assert_eq!(expiry_ms("2026-06-02"), 1_780_387_200_000);
    let exp_ms = 1_782_979_200_000_i64;
    let now = exp_ms - 30 * 86_400 * 1000;
    // (30 d in ms) / (365 d in ms) — correctly-rounded division == 30.0/365.0 bit-for-bit
    assert_bits("30dte_years", years_to_expiry("2026-07-02", now), (30.0_f64 / 365.0).to_bits());
    // past expiry clamps to 0
    assert_bits("clamped", years_to_expiry("2026-07-02", exp_ms + 1000), 0);
}

#[test]
fn enrich_quote_fills_greeks_when_iv_present() {
    let q = OptionQuote { iv: Some(0.20), ..OptionQuote::new(100.0, C) };
    let out = enrich_quote(q, Some(100.0), 1.0, 0.0);
    assert_rel("enriched_delta", out.delta.unwrap(), 0x3fe1464507526871);
    assert!(out.gamma.is_some() && out.theta.is_some() && out.vega.is_some());
    // no iv / no spot -> unchanged
    assert_eq!(enrich_quote(OptionQuote::new(100.0, C), Some(100.0), 1.0, 0.0).delta, None);
    let q = OptionQuote { iv: Some(0.20), ..OptionQuote::new(100.0, C) };
    assert_eq!(enrich_quote(q, None, 1.0, 0.0).delta, None);
}

// ---- model.py ----

#[test]
fn make_expiry_dte_and_label() {
    let now = 1_780_387_200_000; // 2026-06-02 08:00 UTC
    let e = make_expiry("2026-07-02", now);
    assert_eq!(e, Expiry { date: "2026-07-02".into(), dte: 30, label: "02 Jul".into() });
    assert_eq!(make_expiry("2026-06-02", now).label, "0DTE");
    // late in the UTC day (23:59): tomorrow is dte 1 with a date label, NOT a second 0DTE —
    // the calendar-day rule the Python docstring exists for (oracle: dte=1, "03 Jun")
    let late = 1_780_444_740_000; // 2026-06-02 23:59 UTC
    let t = make_expiry("2026-06-03", late);
    assert_eq!((t.dte, t.label.as_str()), (1, "03 Jun"));
}

fn chain_of(strikes: &[f64], spot: Option<f64>) -> OptionChain {
    OptionChain {
        underlying: "BTC".into(),
        asset_class: AssetClass::Crypto,
        underlying_price: spot,
        expiry: make_expiry("2026-07-02", 1_780_387_200_000),
        asof_ms: 1_780_387_200_000,
        source: "deribit".into(),
        rows: strikes.iter().map(|&s| StrikeRow { strike: s, call: None, put: None }).collect(),
    }
}

#[test]
fn limit_strikes_windows_n_each_side_of_spot() {
    let strikes = [80.0, 90.0, 100.0, 110.0, 120.0, 130.0, 140.0];
    let chain = chain_of(&strikes, Some(104.0));
    // spot 104 falls between 100 and 110 -> symmetric ±2 keeps 2 below spot + 2 at/above
    let out = limit_strikes(chain.clone(), Some(2));
    let got: Vec<f64> = out.rows.iter().map(|r| r.strike).collect();
    assert_eq!(got, [90.0, 100.0, 110.0, 120.0]);
    // ±1 keeps exactly one strike below spot + one at/above (2 rows total, symmetric)
    let got: Vec<f64> =
        limit_strikes(chain.clone(), Some(1)).rows.iter().map(|r| r.strike).collect();
    assert_eq!(got, [100.0, 110.0]);
    // None / 0 / window wider than the ladder / no spot are no-ops
    assert_eq!(limit_strikes(chain.clone(), None).rows.len(), 7);
    assert_eq!(limit_strikes(chain.clone(), Some(0)).rows.len(), 7);
    assert_eq!(limit_strikes(chain, Some(99)).rows.len(), 7);
    assert_eq!(limit_strikes(chain_of(&strikes, None), Some(2)).rows.len(), 7);
}

#[test]
fn option_kind_wire_letters() {
    assert_eq!(OptionKind::from_cp("C"), Some(C));
    assert_eq!(OptionKind::from_cp("P"), Some(P));
    assert_eq!(OptionKind::from_cp("Z"), None);
    assert_eq!(C.as_cp(), "C");
    assert_eq!(P.as_cp(), "P");
}

// ---- columns.py ----

fn quote() -> OptionQuote {
    OptionQuote {
        bid: Some(13.6),
        ask: Some(13.9),
        last: Some(14.1),
        mark: Some(13.75),
        iv: Some(0.1784),
        open_interest: Some(1952.0),
        volume: Some(4085.0),
        ..OptionQuote::new(7600.0, C)
    }
}

#[test]
fn chain_and_greeks_field_sets_have_known_headers() {
    for f in CHAIN_FIELDS.iter().chain(GREEKS_FIELDS.iter()) {
        assert!(!header(f).is_empty());
        assert!(matches!(kind(f), "px" | "pct" | "int" | "bar" | "g"));
    }
}

#[test]
fn cell_value_direct_fields() {
    let q = quote();
    let spot = Some(7600.75);
    assert_eq!(cell_value("bid", Some(&q), spot, 0, 0.0), Some(13.6));
    assert_eq!(cell_value("ask", Some(&q), spot, 0, 0.0), Some(13.9));
    assert_eq!(cell_value("ltp", Some(&q), spot, 0, 0.0), Some(14.1));
    assert_eq!(cell_value("volume", Some(&q), spot, 0, 0.0), Some(4085.0));
    assert_eq!(cell_value("oi", Some(&q), spot, 0, 0.0), Some(1952.0));
    assert_eq!(cell_value("iv", Some(&q), spot, 0, 0.0), Some(0.1784));
}

#[test]
fn cell_value_derived_fields_exact() {
    // pure-arithmetic tier: the SAME expressions the oracle computes, asserted exactly
    let q = quote();
    let spot = 7600.75;
    assert_bits(
        "distance",
        cell_value("distance", Some(&q), Some(spot), 0, 0.0).unwrap(),
        0.75_f64.to_bits(), // |7600 - 7600.75| is exact binary arithmetic
    );
    assert_bits(
        "reldist",
        cell_value("reldist", Some(&q), Some(spot), 0, 0.0).unwrap(),
        (0.75 / spot).to_bits(),
    );
    assert_bits(
        "bidpct",
        cell_value("bidpct", Some(&q), Some(spot), 0, 0.0).unwrap(),
        (13.6 / spot).to_bits(),
    );
    assert_bits(
        "askpct",
        cell_value("askpct", Some(&q), Some(spot), 0, 0.0).unwrap(),
        (13.9 / spot).to_bits(),
    );
    // spread% = (ask-bid)/mark
    assert_bits(
        "spread",
        cell_value("spread", Some(&q), Some(spot), 0, 0.0).unwrap(),
        ((13.9 - 13.6) / 13.75_f64).to_bits(),
    );
    // annualized premium yield — oracle pin 0.021771929824561404
    assert_bits(
        "annbid",
        cell_value("annbid", Some(&q), Some(spot), 30, 0.0).unwrap(),
        0x3f964b617a44e9d5,
    );
    assert_bits(
        "annask",
        cell_value("annask", Some(&q), Some(spot), 30, 0.0).unwrap(),
        ((13.9_f64 / 7600.0) * (365.0 / 30.0)).to_bits(),
    );
}

#[test]
fn cell_value_theor_uses_black_scholes() {
    // 30 DTE, iv 17.84% — oracle pin 155.4377943450886 (erf-derived tier)
    let th = cell_value("theor", Some(&quote()), Some(7600.75), 30, 0.0).unwrap();
    assert_rel("theor30", th, 0x40636e02694950f0);
}

#[test]
fn cell_value_none_quote_and_missing_context_are_safe() {
    let q = quote();
    assert_eq!(cell_value("bid", None, Some(7600.0), 0, 0.0), None);
    assert_eq!(cell_value("distance", Some(&q), None, 0, 0.0), None); // no spot
    assert_eq!(cell_value("reldist", Some(&q), None, 0, 0.0), None);
    // Python falsiness: spot == 0.0 is "no spot" for the ratio fields
    assert_eq!(cell_value("reldist", Some(&q), Some(0.0), 0, 0.0), None);
    assert_eq!(cell_value("bidpct", Some(&q), Some(0.0), 0, 0.0), None);
    // theor with no IV -> None (BS needs sigma)
    assert_eq!(cell_value("theor", Some(&OptionQuote::new(100.0, C)), Some(100.0), 30, 0.0), None);
    // mark == 0.0 -> spread N/A (falsy mark)
    let zero_mark = OptionQuote { mark: Some(0.0), ..quote() };
    assert_eq!(cell_value("spread", Some(&zero_mark), Some(7600.75), 0, 0.0), None);
    // unknown field -> None (Python falls through)
    assert_eq!(cell_value("nope", Some(&q), Some(7600.75), 0, 0.0), None);
}

#[test]
fn fmt_by_kind_matches_python_format_specs() {
    // string pins straight from the oracle venv
    assert_eq!(fmt(None, "bid"), "—");
    assert_eq!(fmt(Some(13.6), "bid"), "13.60");
    assert_eq!(fmt(Some(104_000.0), "bid"), "104,000.00");
    assert_eq!(fmt(Some(-1234.5), "bid"), "-1,234.50");
    assert_eq!(fmt(Some(0.1784), "iv"), "17.84%");
    assert_eq!(fmt(Some(0.0037), "spread"), "0.37%");
    assert_eq!(fmt(Some(4085.0), "volume"), "4,085");
    assert_eq!(fmt(Some(1_234_567.0), "volume"), "1,234,567");
    assert_eq!(fmt(Some(0.5), "volume"), "0"); // Python banker's `:,.0f` == Rust `{:.0}`
    assert_eq!(fmt(Some(0.521), "delta"), "0.521");
    assert_eq!(fmt(Some(-0.0108754), "theta"), "-0.011");
}

#[test]
fn fmt_strike_trims_whole_strikes() {
    // twin of OptionsTab._fmt_strike: "64,000" for BTC, "14.5" for VIX
    assert_eq!(fmt_strike(64000.0), "64,000");
    assert_eq!(fmt_strike(14.5), "14.5");
    assert_eq!(fmt_strike(100_000.0), "100,000");
    assert_eq!(fmt_strike(0.5), "0.5");
}
