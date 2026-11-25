//! `synthetic`'s bar tests: the bar invariants, determinism and the pinned stream, each model's law
//! (moments, the exact noiseless paths, the intrabar path's maximum and its joint range), the regime
//! path's ground truth, the bootstrap's blocks, and every refusal. The order book's tests are
//! `synthetic/order_book_tests.rs`.

use vike_marketdata::Bar;

use super::{
    SyntheticGrid, SyntheticModel, SyntheticRegime, block_bootstrap_bars, model_bars,
    regime_switching_path,
};

const HOUR: i64 = 3_600_000;
/// The intrabar sub-steps of every test that is not about them.
const STEPS: usize = 16;

fn grid(bars: usize) -> SyntheticGrid {
    SyntheticGrid { start_ms: 1_735_689_600_000, step_ms: HOUR, bars }
}

const GBM: SyntheticModel =
    SyntheticModel::GeometricBrownianMotion { drift: 0.0001, volatility: 0.01 };
const OU: SyntheticModel =
    SyntheticModel::OrnsteinUhlenbeck { mean_price: 100.0, reversion: 0.05, volatility: 0.01 };
const REGIMES: SyntheticModel = SyntheticModel::RegimeSwitching {
    regimes: [
        SyntheticRegime { drift: 0.0005, volatility: 0.005 },
        SyntheticRegime { drift: -0.001, volatility: 0.03 },
    ],
    switch_probability: [0.02, 0.1],
};
const JUMPS: SyntheticModel = SyntheticModel::JumpDiffusion {
    drift: 0.0001,
    volatility: 0.01,
    jump_intensity: 0.05,
    jump_mean: -0.02,
    jump_volatility: 0.04,
};
const MODELS: [SyntheticModel; 4] = [GBM, OU, REGIMES, JUMPS];

fn mean(xs: &[f64]) -> f64 {
    let mut total = 0.0;
    for x in xs {
        total += x;
    }
    total / xs.len() as f64
}

fn standard_deviation(xs: &[f64]) -> f64 {
    let m = mean(xs);
    let squares: Vec<f64> = xs.iter().map(|x| (x - m) * (x - m)).collect();
    mean(&squares).sqrt()
}

fn log_returns(bars: &[Bar]) -> Vec<f64> {
    bars.iter().map(|b| libm::log(b.close / b.open)).collect()
}

/// FNV-1a over the bits of every field a path carries.
fn fingerprint(bars: &[Bar]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bars {
        for word in [
            b.timestamp as u64,
            b.open.to_bits(),
            b.high.to_bits(),
            b.low.to_bits(),
            b.close.to_bits(),
            b.volume.to_bits(),
        ] {
            for byte in word.to_le_bytes() {
                hash = (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3);
            }
        }
    }
    hash
}

/// Every bar is a bar, on its grid, and opens at the close before it.
fn assert_bars(bars: &[Bar], grid: &SyntheticGrid, first_price: f64) {
    assert_eq!(bars.len(), grid.bars);
    let mut previous_close = first_price;
    for (i, b) in bars.iter().enumerate() {
        assert_eq!(b.timestamp, grid.start_ms + i as i64 * grid.step_ms, "bar {i}");
        assert!(b.low > 0.0 && b.high.is_finite(), "bar {i}: {b:?}");
        assert!(b.low <= b.open.min(b.close) && b.high >= b.open.max(b.close), "bar {i}: {b:?}");
        assert_eq!(b.open.to_bits(), previous_close.to_bits(), "bar {i} opens at the close before");
        previous_close = b.close;
    }
}

#[test]
fn every_model_bar_is_a_bar_on_its_grid_opening_at_the_close_before() {
    for model in MODELS {
        for steps in [1, STEPS] {
            let bars = model_bars(&model, &grid(5_000), 250.0, 3.0, steps, 11).unwrap();
            assert_bars(&bars, &grid(5_000), 250.0);
            assert!(bars.iter().all(|b| b.volume == 3.0), "{model:?}");
        }
    }
    assert_eq!(model_bars(&GBM, &grid(0), 250.0, 3.0, STEPS, 11).unwrap(), Vec::<Bar>::new());
}

#[test]
fn one_seed_is_one_path_and_another_seed_is_another() {
    let source = model_bars(&GBM, &grid(300), 100.0, 1.0, STEPS, 5).unwrap();
    let paths = |seed| {
        let mut all: Vec<Vec<Bar>> = MODELS
            .iter()
            .map(|m| model_bars(m, &grid(200), 100.0, 1.0, STEPS, seed).unwrap())
            .collect();
        all.push(block_bootstrap_bars(&source, 7, &grid(200), 100.0, seed).unwrap());
        all
    };
    let (a, b, c) = (paths(42), paths(42), paths(43));
    assert_eq!(a, b);
    for (x, z) in a.iter().zip(&c) {
        assert_ne!(x, z);
    }
}

/// The stream itself, pinned: a change of the generator, of `rand_chacha`'s stream or of a `libm`
/// result moves this number. Captured on a Linux lane; never re-record it to pass a change, find what
/// moved. Re-pinned ONCE, as a reviewed change, when every model bar became one intrabar path (the
/// coordinator's ruling of 2026-11-16); the first capture, of independent bridge extremes, was
/// `869_139_360_865_126_741`.
#[test]
fn the_stream_is_pinned() {
    let source = model_bars(&GBM, &grid(300), 100.0, 1.0, STEPS, 5).unwrap();
    let mut all: Vec<Bar> = Vec::new();
    for model in MODELS {
        all.extend(model_bars(&model, &grid(64), 100.0, 1.0, STEPS, 42).unwrap());
    }
    all.extend(block_bootstrap_bars(&source, 7, &grid(64), 100.0, 42).unwrap());
    assert_eq!(fingerprint(&all), 4_307_821_423_775_014_396, "the synthetic stream moved");
}

#[test]
fn without_volatility_a_brownian_path_is_its_drift_alone() {
    let model = SyntheticModel::GeometricBrownianMotion { drift: 0.002, volatility: 0.0 };
    let bars = model_bars(&model, &grid(500), 100.0, 1.0, 1, 1).unwrap();
    for (i, b) in bars.iter().enumerate() {
        let expected = 100.0 * libm::exp(0.002 * (i + 1) as f64);
        assert!((b.close / expected - 1.0).abs() < 1e-12, "bar {i}: {} vs {expected}", b.close);
        // A rising bar with no noise spans exactly its open to its close.
        assert!((b.high / b.close - 1.0).abs() < 1e-14 && (b.low / b.open - 1.0).abs() < 1e-14);
    }
}

#[test]
fn a_brownian_log_return_has_the_models_mean_and_deviation() {
    let (drift, volatility) = (0.0003, 0.02);
    let model = SyntheticModel::GeometricBrownianMotion { drift, volatility };
    let returns = log_returns(&model_bars(&model, &grid(40_000), 100.0, 1.0, 4, 3).unwrap());
    let standard_error = volatility / (returns.len() as f64).sqrt();
    let expected = drift - volatility * volatility / 2.0;
    assert!((mean(&returns) - expected).abs() < 4.0 * standard_error, "{}", mean(&returns));
    assert!((standard_deviation(&returns) / volatility - 1.0).abs() < 0.02);
}

/// A bar's high is its intrabar path's maximum over `n` points, whose mean is, to first order
/// (Siegmund), `volatility * (sqrt(2 / pi) + zeta(1/2) / sqrt(2 pi n))`: 0.7251 volatility at
/// `n = 64`, clearly under the continuous 0.7979; the low mirrors it.
#[test]
fn the_intrabar_extremes_have_the_grid_maximums_mean() {
    let volatility = 0.01;
    let model = SyntheticModel::GeometricBrownianMotion {
        drift: volatility * volatility / 2.0,
        volatility,
    };
    let bars = model_bars(&model, &grid(40_000), 100.0, 1.0, 64, 9).unwrap();
    let ups: Vec<f64> = bars.iter().map(|b| libm::log(b.high / b.open) / volatility).collect();
    let downs: Vec<f64> = bars.iter().map(|b| libm::log(b.open / b.low) / volatility).collect();
    let zeta_half = -1.460_354_508_809_586_8;
    let tau = std::f64::consts::TAU;
    let expected = (2.0 / std::f64::consts::PI).sqrt() + zeta_half / (tau * 64.0).sqrt();
    for got in [mean(&ups), mean(&downs)] {
        assert!((got / expected - 1.0).abs() < 0.025, "{got} vs {expected}");
    }
}

/// High and low come from ONE path, so the range has the Brownian range's law: `E[R^2] = 4 ln 2`
/// (Feller) in units of the bar's volatility, where two independent extremes would give
/// `2 + 4 / pi` (3.27). At 2048 sub-steps the grid takes about 0.08 off it.
#[test]
fn a_bars_high_and_low_come_from_one_path() {
    let volatility = 0.01;
    let model = SyntheticModel::GeometricBrownianMotion {
        drift: volatility * volatility / 2.0,
        volatility,
    };
    let bars = model_bars(&model, &grid(6_000), 100.0, 1.0, 2_048, 10).unwrap();
    let squares: Vec<f64> = bars
        .iter()
        .map(|b| {
            let range = libm::log(b.high / b.low) / volatility;
            range * range
        })
        .collect();
    let joint = 4.0 * libm::log(2.0);
    let got = mean(&squares);
    assert!((got - joint).abs() < 0.2 && got < 3.0, "E[R^2] {got} vs {joint}");
}

#[test]
fn without_noise_an_ornstein_uhlenbeck_path_decays_to_its_mean_exactly() {
    let reversion = 0.1;
    let model = SyntheticModel::OrnsteinUhlenbeck { mean_price: 100.0, reversion, volatility: 0.0 };
    // One sub-step is the bar's own transition; eight compose to it, within 1 600 rounded multiplies.
    for (steps, allowed) in [(1, 1e-12), (8, 1e-10)] {
        let bars = model_bars(&model, &grid(200), 200.0, 1.0, steps, 1).unwrap();
        for (i, b) in bars.iter().enumerate() {
            let expected = libm::log(2.0) * libm::exp(-reversion * (i + 1) as f64);
            let got = libm::log(b.close / 100.0);
            assert!(
                (got - expected).abs() < allowed,
                "{steps} steps, bar {i}: {got} vs {expected}"
            );
        }
    }
}

#[test]
fn an_ornstein_uhlenbeck_path_reverts_to_its_mean_price() {
    // Stationary deviation 0.01 / sqrt(2 * 0.05) ~ 0.032, decorrelated every ~20 bars: the mean of
    // 40 000 log prices sits within ~0.002 of ln(100).
    let bars = model_bars(&OU, &grid(40_000), 300.0, 1.0, 4, 4).unwrap();
    let logs: Vec<f64> = bars[200..].iter().map(|b| libm::log(b.close)).collect();
    assert!((mean(&logs) - libm::log(100.0)).abs() < 0.01, "{}", mean(&logs));
    let stationary = 0.01 / (2.0_f64 * 0.05).sqrt();
    assert!((standard_deviation(&logs) / stationary - 1.0).abs() < 0.1);
}

#[test]
fn a_certain_switch_alternates_the_regimes_and_none_keeps_the_first() {
    let regimes = [
        SyntheticRegime { drift: 0.01, volatility: 0.0 },
        SyntheticRegime { drift: -0.02, volatility: 0.0 },
    ];
    let always = SyntheticModel::RegimeSwitching { regimes, switch_probability: [1.0, 1.0] };
    let never = SyntheticModel::RegimeSwitching { regimes, switch_probability: [0.0, 0.0] };
    let alternating = log_returns(&model_bars(&always, &grid(50), 100.0, 1.0, 2, 2).unwrap());
    for (i, r) in alternating.iter().enumerate() {
        let expected = if i % 2 == 0 { 0.01 } else { -0.02 };
        assert!((r - expected).abs() < 1e-12, "bar {i}: {r}");
    }
    for r in log_returns(&model_bars(&never, &grid(50), 100.0, 1.0, 2, 2).unwrap()) {
        assert!((r - 0.01).abs() < 1e-12, "{r}");
    }
}

/// The regime path is the same bars as `model_bars` plus the regime each was drawn in, which a
/// noiseless model shows on every bar's return; the chain spends its stationary share
/// `p(leave 1) / (p(leave 0) + p(leave 1))` = 0.4 of the time in regime 0 (the drifts are chosen to
/// cancel at that share, so the log price stays where rounding is small).
#[test]
fn a_regime_path_returns_the_regime_of_every_bar() {
    let model = SyntheticModel::RegimeSwitching {
        regimes: [
            SyntheticRegime { drift: 0.015, volatility: 0.0 },
            SyntheticRegime { drift: -0.01, volatility: 0.0 },
        ],
        switch_probability: [0.3, 0.2],
    };
    let path = regime_switching_path(&model, &grid(20_000), 100.0, 1.0, STEPS, 13).unwrap();
    assert_eq!(path.bars, model_bars(&model, &grid(20_000), 100.0, 1.0, STEPS, 13).unwrap());
    assert_eq!((path.regimes.len(), path.regimes[0]), (20_000, 0));
    for (i, (r, regime)) in log_returns(&path.bars).iter().zip(&path.regimes).enumerate() {
        let expected = if *regime == 0 { 0.015 } else { -0.01 };
        assert!((r - expected).abs() < 1e-12, "bar {i} in regime {regime}: {r}");
    }
    let share = path.regimes.iter().filter(|r| **r == 0).count() as f64 / 20_000.0;
    assert!((share - 0.4).abs() < 0.03, "{share}");
    let noisy = regime_switching_path(&REGIMES, &grid(500), 100.0, 1.0, STEPS, 7).unwrap();
    assert_eq!(noisy.bars, model_bars(&REGIMES, &grid(500), 100.0, 1.0, STEPS, 7).unwrap());
    let why = regime_switching_path(&GBM, &grid(10), 100.0, 1.0, STEPS, 0).unwrap_err();
    assert!(why.contains("needs a RegimeSwitching model"), "{why}");
}

#[test]
fn without_jumps_a_jump_diffusion_is_the_brownian_path_of_its_seed() {
    let jumpless = SyntheticModel::JumpDiffusion {
        drift: 0.0001,
        volatility: 0.01,
        jump_intensity: 0.0,
        jump_mean: 0.5,
        jump_volatility: 0.5,
    };
    assert_eq!(
        model_bars(&jumpless, &grid(500), 100.0, 1.0, STEPS, 8).unwrap(),
        model_bars(&GBM, &grid(500), 100.0, 1.0, STEPS, 8).unwrap()
    );
}

#[test]
fn jumps_arrive_at_their_intensity_and_carry_their_size() {
    let model = SyntheticModel::JumpDiffusion {
        drift: 0.0,
        volatility: 0.0,
        jump_intensity: 0.1,
        jump_mean: 0.05,
        jump_volatility: 0.0,
    };
    let bars = model_bars(&model, &grid(40_000), 100.0, 1.0, 4, 6).unwrap();
    let counts: Vec<f64> = log_returns(&bars)
        .iter()
        .map(|r| {
            let count = (r / 0.05).round();
            assert!((r - count * 0.05).abs() < 1e-9, "a bar moves by whole jumps: {r}");
            count
        })
        .collect();
    assert!((mean(&counts) - 0.1).abs() < 0.01, "{}", mean(&counts));
}

fn source_bars() -> Vec<Bar> {
    (0..40)
        .map(|i| {
            let close = 100.0 + libm::sin(i as f64 * 0.9) * 4.0 + i as f64 * 0.3;
            let open = close - 0.5 + (i % 3) as f64 * 0.4;
            let (high, low) = (open.max(close) + 0.7, open.min(close) - 0.6);
            Bar::ohlcv(i * HOUR, open, high, low, close, 10.0 + i as f64)
        })
        .collect()
}

/// `[gap, high / open, low / open, close / open, volume]` of every bar after the first, chained from
/// `first`.
fn shapes(bars: &[Bar], first: f64) -> Vec<[f64; 5]> {
    let mut previous = first;
    bars.iter()
        .map(|b| {
            let shape =
                [b.open / previous, b.high / b.open, b.low / b.open, b.close / b.open, b.volume];
            previous = b.close;
            shape
        })
        .collect()
}

fn same_shape(a: &[f64; 5], b: &[f64; 5]) -> bool {
    a.iter().zip(b).all(|(x, y)| (x / y - 1.0).abs() < 1e-12)
}

#[test]
fn a_bootstrap_of_one_whole_block_replays_the_source() {
    let source = source_bars();
    let replay = grid(source.len() - 1);
    let bars =
        block_bootstrap_bars(&source, source.len() - 1, &replay, source[0].close, 1).unwrap();
    assert_eq!(bars.len(), source.len() - 1);
    for (i, (got, want)) in bars.iter().zip(&source[1..]).enumerate() {
        assert_eq!(got.timestamp, replay.start_ms + i as i64 * HOUR);
        for (g, w) in [(got.open, want.open), (got.high, want.high), (got.low, want.low)] {
            assert!((g / w - 1.0).abs() < 1e-12, "bar {i}: {g} vs {w}");
        }
        assert!((got.close / want.close - 1.0).abs() < 1e-12, "bar {i}");
        assert_eq!(got.volume, want.volume);
    }
}

#[test]
fn a_bootstrap_chains_whole_blocks_of_the_source_shapes() {
    let source = source_bars();
    let source_shapes = shapes(&source[1..], source[0].close);
    let block = 5;
    let path = grid(103);
    let bars = block_bootstrap_bars(&source, block, &path, 50.0, 12).unwrap();
    assert_eq!(bars.len(), 103);
    for b in &bars {
        assert!(b.low > 0.0 && b.low <= b.open.min(b.close) && b.high >= b.open.max(b.close));
    }
    for drawn in shapes(&bars, 50.0).chunks(block) {
        let found = source_shapes
            .windows(drawn.len())
            .any(|run| run.iter().zip(drawn).all(|(a, b)| same_shape(a, b)));
        assert!(found, "a drawn block is a contiguous run of the source: {drawn:?}");
    }
}

#[test]
fn every_bad_parameter_is_refused_in_words() {
    let refused = |model: SyntheticModel, words: &str| {
        let e = model_bars(&model, &grid(10), 100.0, 1.0, STEPS, 0).unwrap_err();
        assert!(e.contains(words), "{e}");
    };
    refused(SyntheticModel::GeometricBrownianMotion { drift: 0.0, volatility: -0.1 }, "volatility");
    refused(SyntheticModel::GeometricBrownianMotion { drift: f64::NAN, volatility: 0.1 }, "drift");
    refused(
        SyntheticModel::OrnsteinUhlenbeck { mean_price: 0.0, reversion: 0.1, volatility: 0.1 },
        "mean_price",
    );
    refused(
        SyntheticModel::OrnsteinUhlenbeck { mean_price: 1.0, reversion: -0.1, volatility: 0.1 },
        "reversion",
    );
    refused(
        SyntheticModel::RegimeSwitching {
            regimes: [SyntheticRegime { drift: 0.0, volatility: 0.1 }; 2],
            switch_probability: [0.5, 1.5],
        },
        "switch_probability[1]",
    );
    refused(
        SyntheticModel::JumpDiffusion {
            drift: 0.0,
            volatility: 0.1,
            jump_intensity: 101.0,
            jump_mean: 0.0,
            jump_volatility: 0.1,
        },
        "jump_intensity",
    );
    // The path itself leaving f64: refused at the bar, never returned as infinity.
    refused(SyntheticModel::GeometricBrownianMotion { drift: 800.0, volatility: 0.0 }, "bar 0");

    let e = model_bars(&GBM, &SyntheticGrid { step_ms: 0, ..grid(10) }, 100.0, 1.0, STEPS, 0);
    assert!(e.unwrap_err().contains("step"));
    let late = SyntheticGrid { start_ms: i64::MAX - HOUR, ..grid(3) };
    let e = model_bars(&GBM, &late, 100.0, 1.0, STEPS, 0);
    assert!(e.unwrap_err().contains("representable"));
    assert!(model_bars(&GBM, &grid(10), 0.0, 1.0, STEPS, 0).unwrap_err().contains("first price"));
    assert!(model_bars(&GBM, &grid(10), 100.0, -1.0, STEPS, 0).unwrap_err().contains("volume"));
    let e = model_bars(&GBM, &grid(10), 100.0, 1.0, 0, 0).unwrap_err();
    assert!(e.contains("at least one intrabar step"), "{e}");

    let source = source_bars();
    let e = block_bootstrap_bars(&source, 0, &grid(10), 100.0, 0).unwrap_err();
    assert!(e.contains("block length"), "{e}");
    let e = block_bootstrap_bars(&source[..5], 5, &grid(10), 100.0, 0).unwrap_err();
    assert!(e.contains("at least 6 source bars"), "{e}");
    let mut broken = source.clone();
    broken[3].high = broken[3].close - 1.0;
    let e = block_bootstrap_bars(&broken, 2, &grid(10), 100.0, 0).unwrap_err();
    assert!(e.contains("source bar 3"), "{e}");
}
