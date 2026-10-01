use super::*;

/// The spread `A − β·B` means one unit of spread is 1 unit of A against β units
/// of B. Sizing must use that β, not a 50/50 dollar split (the Hummingbot bug).
#[test]
fn hedge_leg_is_scaled_by_beta_not_split_evenly() {
    let (qa, qb) = beta_neutral_qtys(1000.0, 100.0, 50.0, 2.0).unwrap();
    assert!((qa - 10.0).abs() < 1e-12, "qty_a {qa}");
    assert!((qb - 20.0).abs() < 1e-12, "qty_b {qb}"); // 2.0 * 10.0
}

/// beta = 1 is the degenerate equal-UNITS case, NOT equal-dollars. A 50/50
/// dollar split would give qty_b = 20 here; beta-neutral gives 10.
#[test]
fn beta_one_is_equal_units_not_equal_dollars() {
    let (qa, qb) = beta_neutral_qtys(1000.0, 100.0, 50.0, 1.0).unwrap();
    assert!((qa - 10.0).abs() < 1e-12);
    assert!((qb - 10.0).abs() < 1e-12, "equal UNITS: {qb}");
}

/// A non-positive or non-finite beta is not a hedge — refuse rather than invert
/// a leg behind the caller's back.
#[test]
fn non_positive_or_nonfinite_beta_is_refused() {
    assert!(beta_neutral_qtys(1000.0, 100.0, 50.0, 0.0).is_none());
    assert!(beta_neutral_qtys(1000.0, 100.0, 50.0, -1.5).is_none());
    assert!(beta_neutral_qtys(1000.0, 100.0, 50.0, f64::NAN).is_none());
}

#[test]
fn bad_prices_or_notional_are_refused() {
    assert!(beta_neutral_qtys(1000.0, 0.0, 50.0, 1.0).is_none());
    assert!(beta_neutral_qtys(1000.0, 100.0, 0.0, 1.0).is_none());
    assert!(beta_neutral_qtys(0.0, 100.0, 50.0, 1.0).is_none());
    assert!(beta_neutral_qtys(f64::INFINITY, 100.0, 50.0, 1.0).is_none());
}

/// Dollar-neutrality is NOT the same as beta-neutrality, and the difference
/// grows with beta — pinned so a future "simplification" back to a 50/50 split
/// fails loudly.
#[test]
fn beta_neutral_differs_from_dollar_neutral_when_beta_is_not_one() {
    let notional = 1000.0;
    let (qa, qb) = beta_neutral_qtys(notional, 100.0, 50.0, 3.0).unwrap();
    let dollar_neutral_qb = notional / 50.0; // what a 50/50 split would size
    assert!((qa - 10.0).abs() < 1e-12);
    assert!((qb - 30.0).abs() < 1e-12);
    assert!(
        (qb - dollar_neutral_qb).abs() > 1e-9,
        "beta-neutral {qb} must differ from dollar-neutral {dollar_neutral_qb}"
    );
}

// --- entry / exit rules ---

/// A band break alone is NOT enough: the expected convergence must also clear a
/// full round trip. This is the gate that makes the harness honest.
#[test]
fn entry_requires_both_a_band_break_and_a_cleared_cost_floor() {
    // |z| = 3 breaks a 2.0 band, but an edge of 1.0 is below a 40.0 round trip.
    assert!(!should_enter(3.0, 2.0, 1.0, Some(40.0)));
    // Same band break, edge now exceeds the round trip.
    assert!(should_enter(3.0, 2.0, 41.0, Some(40.0)));
    // Ample edge but the band is not broken.
    assert!(!should_enter(1.0, 2.0, 41.0, Some(40.0)));
}

/// An unknowable cost is never a free one — a missing/crossed book must block
/// entry rather than be treated as zero cost.
#[test]
fn entry_is_refused_when_the_cost_is_unknowable() {
    assert!(!should_enter(5.0, 2.0, 1e9, None));
}

#[test]
fn entry_is_refused_on_non_finite_inputs() {
    assert!(!should_enter(f64::NAN, 2.0, 100.0, Some(1.0)));
    assert!(!should_enter(5.0, 2.0, f64::NAN, Some(1.0)));
}

/// Exit is symmetric: reversion INTO the band, or a sign flip vs the entry z.
#[test]
fn exit_on_reversion_or_sign_flip() {
    assert!(should_exit(0.4, 2.5, 0.5), "reverted inside the exit band");
    assert!(!should_exit(1.2, 2.5, 0.5), "still outside the exit band");
    assert!(should_exit(-0.9, 2.5, 0.5), "sign flip vs the entry z");
    assert!(!should_exit(f64::NAN, 2.5, 0.5), "NaN holds rather than churns");
}

// --- z-score definition parity with the indicator seam ---

/// The strategy computes its z inline (it needs the sd for the edge estimate
/// anyway), so this pins that the inline definition matches
/// `vike_indicators::pairs::spread_zscore` EXACTLY. Without this gate the z the
/// strategy trades on could silently drift from the z the seam plots — a
/// population-vs-sample variance change would be invisible otherwise.
#[test]
fn inline_zscore_matches_the_indicator_seam() {
    use vike_indicators::PairIndicator;
    use vike_indicators::pairs::SpreadZscore;

    let period = 20usize;
    let beta = 1.0f64;
    let a: Vec<f64> = (0..80).map(|i| 100.0 + (i as f64 * 0.13).sin() * 6.0).collect();
    let b: Vec<f64> = (0..80).map(|i| 50.0 + (i as f64 * 0.09).cos() * 3.0).collect();

    let ind = SpreadZscore::with_params(&[period as f64, beta]);
    let seam = ind.vectorize_pair(&bars_from(&a), &bars_from(&b));
    let seam_z = &seam[0];

    let spreads: Vec<f64> = a.iter().zip(b.iter()).map(|(x, y)| x - beta * y).collect();
    for end in period..=spreads.len() {
        let (mean, sd) = rolling_mean_sd_last(&spreads[..end], period).unwrap();
        let mine = (spreads[end - 1] - mean) / sd;
        let theirs = seam_z[end - 1];
        // BIT-exact, not a tolerance: the inline kernel reproduces the seam's
        // running-sum recurrence, so any drift in definition OR summation order
        // fails here rather than hiding under an epsilon.
        assert_eq!(
            mine.to_bits(),
            theirs.to_bits(),
            "idx {}: inline z {mine} != seam z {theirs}",
            end - 1
        );
    }
}

// --- strategy wiring ---

/// A minimal `Broker` double: records orders and folds them into positions, so
/// the tests can assert on what the strategy actually ROUTED.
#[derive(Default)]
struct MockBroker {
    orders: Vec<(String, i32, f64)>,
    positions: std::collections::HashMap<String, f64>,
    empty: Vec<Bar>,
}

impl Broker for MockBroker {
    fn submit_market(&mut self, symbol: &str, side: i32, qty: f64) {
        self.orders.push((symbol.to_string(), side, qty));
        *self.positions.entry(symbol.to_string()).or_insert(0.0) += f64::from(side) * qty;
    }
    fn submit_limit(&mut self, _symbol: &str, _side: i32, _qty: f64, _price: f64) {}
    fn position(&self, symbol: &str) -> f64 {
        self.positions.get(symbol).copied().unwrap_or(0.0)
    }
    fn price(&self, _symbol: &str) -> f64 {
        0.0
    }
    fn equity(&self) -> f64 {
        0.0
    }
    fn bars(&self, _symbol: &str) -> &[Bar] {
        &self.empty
    }
    fn index(&self) -> usize {
        0
    }
    fn now(&self) -> i64 {
        0
    }
}

fn tagged_bar(sym: &str, close: f64) -> Bar {
    Bar {
        ts: 0,
        open: close,
        high: close,
        low: close,
        close,
        volume: 0.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: Some(sym.to_string()),
    }
}

/// A two-leg strategy must not act on one leg's bar: until BOTH legs advance the
/// spread would be evaluated against a STALE opposite leg. `on_bar` fires once
/// per symbol per step, so this is the core wiring contract.
#[test]
fn a_step_requires_both_legs_to_advance() {
    let mut s = PairsZScore { symbol_a: "A".into(), symbol_b: "B".into(), ..Default::default() };
    let mut br = MockBroker::default();
    for i in 0..5 {
        s.on_bar(&mut br, &tagged_bar("A", 100.0 + f64::from(i)));
    }
    assert_eq!(s.closes_a.len(), 0, "no step may complete on one leg alone");
    s.on_bar(&mut br, &tagged_bar("B", 50.0));
    assert_eq!(s.closes_a.len(), 1, "the step completes once B arrives");
    assert_eq!(s.closes_b.len(), 1);
}

/// Bars for an unrelated symbol must be ignored entirely.
#[test]
fn unknown_symbols_are_ignored() {
    let mut s = PairsZScore { symbol_a: "A".into(), symbol_b: "B".into(), ..Default::default() };
    let mut br = MockBroker::default();
    s.on_bar(&mut br, &tagged_bar("ZZZ", 1.0));
    s.on_bar(&mut br, &tagged_bar("A", 100.0));
    assert_eq!(s.closes_a.len(), 0, "an unrelated symbol must not complete a step");
}

/// Drive a diverging spread through the strategy twice — once with a book so
/// wide the round trip cannot be cleared, once with a thin book. THE COST GATE
/// IS THE ONLY DIFFERENCE, so this proves it is genuinely wired into the
/// decision path rather than merely unit-tested in isolation.
#[test]
fn the_cost_gate_actually_blocks_entry() {
    fn run(half_spread_bps: f64, taker_fee: f64) -> usize {
        let mut s = PairsZScore {
            symbol_a: "A".into(),
            symbol_b: "B".into(),
            period: 10,
            entry_z: 1.5,
            half_spread_bps,
            taker_fee,
            hold_intervals: 0.0,
            ..Default::default()
        };
        let mut br = MockBroker::default();
        // 10 flat steps to fill the window, then a large one-sided divergence.
        for i in 0..10 {
            s.on_bar(&mut br, &tagged_bar("A", 100.0 + f64::from(i % 2)));
            s.on_bar(&mut br, &tagged_bar("B", 50.0));
        }
        for _ in 0..3 {
            s.on_bar(&mut br, &tagged_bar("A", 130.0));
            s.on_bar(&mut br, &tagged_bar("B", 50.0));
        }
        br.orders.len()
    }

    let thin = run(0.5, 0.0);
    let wide = run(5_000.0, 0.05); // a 50% half-spread + 5% fee: nothing can clear it
    assert!(thin > 0, "a thin book with a big divergence should trade (got {thin} orders)");
    assert_eq!(wide, 0, "a prohibitive book must block entry entirely (got {wide} orders)");
}

/// Both legs must be routed on entry, in OPPOSITE directions, with the hedge leg
/// sized by beta — the beta-neutrality contract, checked end-to-end.
#[test]
fn entry_routes_two_opposite_legs_sized_by_beta() {
    let mut s = PairsZScore {
        symbol_a: "A".into(),
        symbol_b: "B".into(),
        period: 10,
        entry_z: 1.5,
        beta: 2.0,
        notional: 1000.0,
        half_spread_bps: 0.5,
        taker_fee: 0.0,
        hold_intervals: 0.0,
        ..Default::default()
    };
    let mut br = MockBroker::default();
    for i in 0..10 {
        s.on_bar(&mut br, &tagged_bar("A", 100.0 + f64::from(i % 2)));
        s.on_bar(&mut br, &tagged_bar("B", 50.0));
    }
    for _ in 0..3 {
        s.on_bar(&mut br, &tagged_bar("A", 130.0));
        s.on_bar(&mut br, &tagged_bar("B", 50.0));
    }
    assert!(br.orders.len() >= 2, "expected a two-leg entry, got {:?}", br.orders);
    let (sym_a, side_a, qty_a) = br.orders[0].clone();
    let (sym_b, side_b, qty_b) = br.orders[1].clone();
    assert_eq!(sym_a, "A");
    assert_eq!(sym_b, "B");
    assert_eq!(side_a, -side_b, "legs must be routed in opposite directions");
    // qty_a = notional/price_a = 1000/130; qty_b = beta * qty_a
    assert!((qty_b - 2.0 * qty_a).abs() < 1e-9, "hedge leg must be beta-scaled: {qty_a}/{qty_b}");
}

/// A rich spread (z > 0) is SOLD: short the dependent leg, long the hedge.
#[test]
fn a_rich_spread_is_sold() {
    let mut s = PairsZScore {
        symbol_a: "A".into(),
        symbol_b: "B".into(),
        period: 10,
        entry_z: 1.5,
        half_spread_bps: 0.5,
        taker_fee: 0.0,
        hold_intervals: 0.0,
        ..Default::default()
    };
    let mut br = MockBroker::default();
    for i in 0..10 {
        s.on_bar(&mut br, &tagged_bar("A", 100.0 + f64::from(i % 2)));
        s.on_bar(&mut br, &tagged_bar("B", 50.0));
    }
    // A jumps: the spread A - B is RICH, so A should be SHORTED.
    for _ in 0..3 {
        s.on_bar(&mut br, &tagged_bar("A", 130.0));
        s.on_bar(&mut br, &tagged_bar("B", 50.0));
    }
    assert!(!br.orders.is_empty(), "expected an entry");
    assert_eq!(br.orders[0].1, -1, "a rich spread must SHORT the dependent leg");
}

/// Defaults must resolve from an empty params table — the contract
/// `registry_lists_every_match_arm` enforces for every roster entry.
#[test]
fn from_params_resolves_with_an_empty_table() {
    let empty: Value = toml::from_str("").unwrap();
    let s = PairsZScore::from_params(&empty);
    assert_eq!(s.period, 20);
    assert!((s.entry_z - 2.0).abs() < 1e-12);
    assert!((s.beta - 1.0).abs() < 1e-12);
    assert!(s.symbol_a.is_empty(), "no symbol default — a two-leg trade needs both named");
}

/// Params actually override, and `period` is floored at 2 (a 1-bar window has no
/// dispersion).
#[test]
fn from_params_reads_overrides() {
    let t: Value = toml::from_str(
        r#"symbol_a = "ETH"
symbol_b = "BTC"
period = 50
entry_z = 2.5
beta = 1.7
"#,
    )
    .unwrap();
    let s = PairsZScore::from_params(&t);
    assert_eq!(s.symbol_a, "ETH");
    assert_eq!(s.symbol_b, "BTC");
    assert_eq!(s.period, 50);
    assert!((s.entry_z - 2.5).abs() < 1e-12);
    assert!((s.beta - 1.7).abs() < 1e-12);

    let floored: Value = toml::from_str("period = 1").unwrap();
    assert_eq!(PairsZScore::from_params(&floored).period, 2, "period floored at 2");
}
