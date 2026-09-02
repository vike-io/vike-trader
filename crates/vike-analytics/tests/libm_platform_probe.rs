//! MEASUREMENT probe: which platform-libm calls actually diverge between Windows (MSVC CRT)
//! and Linux (glibc), and would substituting the portable `libm` crate move a value here.
//!
//! `sin`/`cos`/`exp`/`ln`/`powf` are NOT required by IEEE 754 to be correctly rounded, so each
//! platform's libm may return a different last bit. `sqrt` and `+ - * /` ARE required, so they
//! are excluded throughout.
//!
//! ⚠ **`powi` was excluded here too, on the ground that it "lowers to a multiply chain rather than
//! a call". THAT IS FALSE, and this probe asserted it without measuring it.** MSVC in the `dev`
//! profile — the profile `just t` and CI actually run — emits a `pow()` libcall:
//!
//! ```text
//!                          Linux dbg   Linux rel   Win dbg    Win rel
//!   (x - mean).powi(2)     ab72e59a    ab72e59a    2d464289   ab72e59a
//!   (x - mean) * (x - mean) ab72e59a   ab72e59a    ab72e59a   ab72e59a
//! ```
//!
//! It surfaced in `crates/vike-indicators`, where `hvol` still failed on Windows after all nine of
//! its libm sites were converted, and was identified by `powi(2)` hashing EQUAL to `powf(2.0)` in
//! exactly that one cell. `docs/decisions/0032-transcendentals-come-from-the-libm-crate-not-the-platform.md`
//! carries the amendment.
//!
//! ⚠ **And this crate had the defect, behind a green gate.** `crates/vike-analytics/src/stats.rs`
//! held three `(v - m).powi(2)` variance folds — two of them production, in `block_bootstrap_sharpe`
//! and `hansens_spa`. `stats` is a `pub mod` no other module names, so it sat OUTSIDE
//! `converted_functions_are_platform_invariant`'s pin: the gate was green while those folds stayed
//! profile-dependent on Windows. They are explicit multiplications now. The cure for `powi` is a
//! MULTIPLICATION, never `libm` — the operation is already correctly rounded; the problem was only
//! ever that the compiler was free to reach it through a libcall.
//!
//! ⚠ **CORRECTION — the paragraph above used to stop there, and its past tense read as though the
//! pin had been extended along with the source. It had not been.** The folds were fixed; the TABLE
//! was not touched. `PLATFORM_INVARIANT` still carried sixteen rows, not one of them `stats`, and
//! `production_hashes` did not so much as `use` the module — so the sentence "it sat OUTSIDE the
//! pin" described the state of the tree at the moment it was written just as accurately as it
//! described the state before. `stats` now has two rows, for the two folds that were actually
//! converted: `stats::block_bootstrap_sharpe` and `stats::hansens_spa`. The third fold —
//! `stats::newey_west_tstat`'s `centered.iter().map(|v| v * v)` — was already an explicit
//! multiplication and was never one of the two, which is why it gets no row.
//!
//! ⚠ **Be exact about what those two rows buy, because it is less than "a hole was leaking".**
//! `crates/vike-analytics/src/stats.rs`'s production path today contains NO platform-libm
//! primitive at all: only `sqrt` (IEEE 754 requires it correctly rounded, so it never diverged),
//! plain arithmetic, and an integer-only `StdRng` stream that draws no float. So these rows are a
//! REGRESSION tripwire — they catch the next edit that reaches for `.powi(2)` or `.powf(` in this
//! module — rather than the closing of a divergence that is moving numbers today. The one thing
//! they add beyond that is transitive coverage of `metrics::percentile`, which is reached by no
//! other row in the table. (That function was `pub(crate)` when this was written; it is `pub` now
//! that `crates/vike-backtest/src/bin/cheap_np_depth.rs`'s `pct` delegates to it instead of
//! carrying its own nearest-rank twin. The visibility was never what made it uncovered — no other
//! row reaches it either way.)
//!
//! Run it on BOTH platforms and diff the printed table:
//!
//! ```text
//! <cargo> test -p vike-analytics --test libm_platform_probe -- --ignored --nocapture
//! ```
//!
//! Reading the three columns:
//!
//! - `std` differing across platforms  => that domain genuinely diverges; a fix is warranted.
//! - `libm` MUST be equal across platforms — it is the portable reimplementation. A difference
//!   there would invalidate the whole premise, so it is printed as a check, not as data.
//! - `ndiff`/`maxulp` are std-vs-libm ON THIS BOX. `ndiff=0` means substituting `libm` is a
//!   BIT-FOR-BIT no-op here, i.e. the fix cannot move a pinned value on this platform.
//!
//! ⚠ Every input domain is generated with `libm` or with plain arithmetic ONLY. A generator
//! built from `f64::sin` (as `tests/window_pin.rs`'s `synth_bars` is) would make the INPUTS
//! platform-dependent and every downstream column would diverge for that reason alone.
//!
//! # MEASURED 2026-08-25 — Windows 11 / MSVC CRT against the CI box / glibc, rustc 1.96.0
//!
//! **PART A: all 19 domains diverge**, `sin`/`cos` controls included — so the probe is sensitive
//! enough for its own conclusions. Divergence is <=1 ulp everywhere (`maxulp=1`, no domain worse).
//! (One asymmetry worth keeping: `log10/small-int` reports `ndiff=0` on Windows and `ndiff=12122`
//! on Linux — MSVC agrees with the `libm` crate there and glibc does not. This crate has no
//! `log10` site, so it changed nothing here, but it is why "std" is not one behaviour.)
//!
//! **PART B: 3 of 14 functions diverged across platforms** over 600 curves each —
//! `signal_backtest::equity_from_pnl` (returns a vector, so every divergent `exp` survives),
//! `metrics::k_ratio` and `metrics::returns_skewness`. Ten of the remaining eleven reduce a whole
//! curve to ONE scalar, and a 1-ulp difference is usually rounded away before it reaches that
//! scalar. They were read as NOT OBSERVED TO DIVERGE, never as "cannot": they call the same
//! primitives Part A shows are unstable, so a different input can move them.
//!
//! The eleventh, **`overfit::pbo_cscv`, prints `distinct=1` and is therefore NOT MEASURED** — the
//! synthetic matrices here are monotone across trials, so every one of the 600 samples is the
//! same value and "identical on both platforms" says nothing about the function. It is left in
//! place reporting its own vacuity rather than removed, because a silently degenerate row that
//! reads as a pass is exactly what the `distinct` column exists to expose. Giving it a real
//! verdict needs a matrix whose per-trial ranks actually move.
//!
//! # WHAT WAS DONE ABOUT IT
//!
//! ⚠ This header used to end with a section titled "WHY NOTHING WAS SWITCHED TO `libm` ON THE
//! STRENGTH OF THIS". That verdict has been REVERSED, and the argument it rested on was not
//! wrong — it was incomplete. It said: on Linux, glibc disagrees with the `libm` CRATE across
//! every one of these domains too (0.07%-10% of samples), so a `f64::ln` -> `libm::log`
//! substitution does not merely make Windows agree with Linux, it MOVES the value on Linux, where
//! CI and live trading run. All of that is still true and still measured by the `ndiff` column.
//!
//! What it left out is that the status quo it was defending was not "Linux values are stable" but
//! "the equity curve depends on which box you ran it on". Between two options that both move
//! Linux values, one ends with a number that is the same everywhere and one does not. So every
//! transcendental in `vike-analytics` production code is now the `libm` crate, `sqrt` excepted
//! (IEEE 754 requires sqrt to be correctly rounded, so it never diverged). The cost — these
//! numbers no longer track CPython's platform libm, which is what the retired Python oracle was
//! computed with — is stated in `lib.rs`'s "Cross-platform determinism" section.
//!
//! Parts A and B remain pure MEASUREMENT and gate nothing. ⚠ This line went on to say "the gate
//! is `converted_functions_are_platform_invariant` below", singular, and that stopped being true
//! the day the SOURCE half joined it. Three tests below gate, none of them `#[ignore]`d:
//! [`converted_functions_are_platform_invariant`] (the hashes agree across platforms),
//! [`production_code_calls_libm_not_the_platform`] (no platform spelling in `src/`, which is what
//! covers the sites the corpus never drives), and
//! [`the_test_module_cut_excludes_only_the_test_module`] (the second one's notion of "production"
//! is not silently eating the file).

use std::hint::black_box;

use vike_analytics::{benchmark, metrics, overfit, signal_backtest, stats};

/// Dense enough to catch a rare last-bit disagreement.
const N: usize = 200_001;

/// FNV-1a over `f64::to_bits`, with NaN canonicalized: a NaN's sign and payload are not pinned
/// by the architecture, so hashing them raw would report a divergence that is not one.
struct Fnv(u64);

impl Fnv {
    fn new() -> Self {
        Fnv(0xcbf2_9ce4_8422_2325)
    }
    fn push(&mut self, v: f64) {
        let bits = if v.is_nan() { 0x7ff8_0000_0000_0000u64 } else { v.to_bits() };
        for b in bits.to_le_bytes() {
            self.0 ^= b as u64;
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    fn hex(&self) -> String {
        format!("{:016x}", self.0)
    }
}

/// Bit pattern remapped so that adjacent f64 values are adjacent integers (ULP distance).
fn ord(x: f64) -> i64 {
    let b = x.to_bits() as i64;
    if b < 0 {
        i64::MIN.wrapping_sub(b)
    } else {
        b
    }
}

fn ulp(a: f64, b: f64) -> u64 {
    if a == b || (a.is_nan() && b.is_nan()) {
        0
    } else if !a.is_finite() || !b.is_finite() {
        u64::MAX
    } else {
        ord(a).wrapping_sub(ord(b)).unsigned_abs()
    }
}

/// Sweep one (function, domain) pair: hash `std`, hash `libm`, and count where they disagree.
fn sweep(
    label: &str,
    dom: impl Fn(f64) -> f64,
    s_f: impl Fn(f64) -> f64,
    l_f: impl Fn(f64) -> f64,
) {
    let (mut hs, mut hl) = (Fnv::new(), Fnv::new());
    let (mut ndiff, mut maxulp) = (0u64, 0u64);
    for i in 0..N {
        let x = dom(i as f64 / (N - 1) as f64);
        let s = s_f(black_box(x));
        let l = l_f(black_box(x));
        hs.push(s);
        hl.push(l);
        let u = ulp(s, l);
        if u != 0 {
            ndiff += 1;
            maxulp = maxulp.max(u);
        }
    }
    println!(
        "  {label:<26} std={}  libm={}  ndiff={ndiff}/{N} maxulp={maxulp}",
        hs.hex(),
        hl.hex()
    );
}

/// Platform-invariant equity curve: plain arithmetic over an LCG, no transcendental anywhere.
///
/// ⚠ Seeded and vol-parameterised deliberately. Most functions below reduce a whole curve to ONE
/// scalar, and a single 1-ulp difference deep in the fold is usually rounded away before it
/// reaches that scalar — so a SINGLE curve reports "identical" with high probability even when
/// the site can diverge. The measurement is only worth anything over many curves.
fn synth_equity(n: usize, seed: u64, vol: f64) -> Vec<f64> {
    let mut s = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15).wrapping_add(0x2545_f491_4f6c_dd1d);
    let mut v = 1.0f64;
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let u = (s >> 11) as f64 / (1u64 << 53) as f64; // exact: 53-bit int / 2^53
        v *= 1.0 + (u - 0.495) * vol;
        out.push(v);
    }
    out
}

/// Volatility regimes swept beside every seed — a quiet curve and a violent one exercise very
/// different `powf`/`ln` arguments.
const VOLS: [f64; 3] = [0.004, 0.02, 0.09];
/// Curves per regime. 200 x 3 = 600 samples per metric; at the ~10% per-call divergence rate
/// Part A measures, a site that CAN diverge is caught with overwhelming probability.
const SEEDS: u64 = 200;

#[test]
#[ignore = "measurement probe — run explicitly on each platform and diff the output"]
fn libm_platform_probe() {
    println!("\n=== PART A — primitives over each production site's real argument domain ===");

    // --- ln ------------------------------------------------------------------------------
    // momentum.rs batch_fisher: ((1+v)/(1-v)).ln(), v clamped to [-0.999, 0.999].
    sweep(
        "ln/fisher-ratio",
        |t| {
            let v = -0.999 + 1.998 * t;
            (1.0 + v) / (1.0 - v)
        },
        f64::ln,
        libm::log,
    );
    // volatility.rs batch_hvol + pairs.rs batch_correl_log + fair_value/vol: (p[i]/p[i-1]).ln().
    sweep("ln/price-ratio", |t| 0.5 + 1.5 * t, f64::ln, libm::log);
    // ...and the NARROW sub-domain a real bar-to-bar ratio actually occupies. Split out because
    // the wide sweep above says nothing about how the two libms behave right at 1.0, where `ln`
    // is best-conditioned — and that is where every log-return in this workspace lives.
    sweep("ln/price-ratio-near1", |t| 0.99 + 0.02 * t, f64::ln, libm::log);
    // pairs.rs batch_spread(log) + metrics.rs k_ratio: .ln() of a raw price / equity level.
    sweep("ln/price-level", |t| libm::pow(10.0, -3.0 + 9.0 * t), f64::ln, libm::log);
    // overfit.rs pbo_cscv logits + mm fairvalue: (omega/(1-omega)).ln().
    sweep(
        "ln/odds",
        |t| {
            let p = 1e-6 + t * (1.0 - 2e-6);
            p / (1.0 - p)
        },
        f64::ln,
        libm::log,
    );
    // overfit.rs normal_inv_cdf tails: p.ln() and (1-p).ln() for p below/above Acklam's break.
    sweep("ln/acklam-tail", |t| 1e-12 + t * 0.02425, f64::ln, libm::log);

    // --- log10 ---------------------------------------------------------------------------
    // volatility.rs batch_chop: (period as f64).log10() — a SMALL INTEGER argument.
    sweep("log10/small-int", |t| (2.0 + (t * 198.0).floor()).floor(), f64::log10, libm::log10);
    // volatility.rs batch_chop: (sum_tr/rng).log10().
    sweep("log10/tr-ratio", |t| 1.0 + t * 1.0e4, f64::log10, libm::log10);

    // --- exp -----------------------------------------------------------------------------
    // overlap.rs batch_alma Gaussian window: (-(k-m)^2 / (2 s^2)).exp(), argument <= 0.
    sweep("exp/alma-window", |t| -50.0 * t, f64::exp, libm::exp);
    // overfit.rs normal_inv_cdf Halley step: (x*x/2).exp().
    sweep("exp/halley", |t| t * 32.0, f64::exp, libm::exp);
    // signal_backtest.rs equity_from_pnl: acc.exp() over a cumulative log-return.
    sweep("exp/equity-acc", |t| -10.0 + 20.0 * t, f64::exp, libm::exp);

    // --- powf ----------------------------------------------------------------------------
    // metrics.rs variance/sortino/ulcer: .powf(2.0) — an exact small integer exponent.
    sweep("powf/x^2", |t| t * 1.0e4, |x| x.powf(2.0), |x| libm::pow(x, 2.0));
    sweep("powf/x^3", |t| -50.0 + t * 100.0, |x| x.powf(3.0), |x| libm::pow(x, 3.0));
    sweep("powf/x^4", |t| -50.0 + t * 100.0, |x| x.powf(4.0), |x| libm::pow(x, 4.0));
    // metrics.rs/benchmark.rs cagr: growth.powf(periods_per_year / n) — a FRACTIONAL exponent.
    sweep("powf/cagr-frac", |t| 0.1 + t * 9.9, |x| x.powf(0.0037), |x| libm::pow(x, 0.0037));
    sweep("powf/cagr-exp", |t| 1.0e-4 + t * 2.0, |e| 3.7f64.powf(e), |e| libm::pow(3.7, e));

    // --- atan ----------------------------------------------------------------------------
    // statistics.rs batch_linearreg_angle: b.atan() on an OLS slope.
    sweep("atan/ols-slope", |t| -1.0e3 + t * 2.0e3, f64::atan, libm::atan);

    // --- the CONTROL: sin/cos are ALREADY KNOWN to diverge (measured on both boxes). If these
    //     two agree across platforms, the probe itself is broken and every other row is void.
    sweep("sin/CONTROL", |t| -6.3 + t * 12.6, f64::sin, libm::sin);
    sweep("cos/CONTROL", |t| -6.3 + t * 12.6, f64::cos, libm::cos);

    println!("\n=== PART B — the real vike-analytics production functions ===");
    println!(
        "    ({} curves each: {SEEDS} seeds x {} vol regimes)",
        SEEDS as usize * VOLS.len(),
        VOLS.len()
    );
    for r in production_hashes() {
        println!("  {:<40} {}  n={} distinct={}", r.label, r.hash, r.n, r.distinct);
    }
    println!();
}

/// One production function's verdict over the whole corpus.
struct Row {
    label: &'static str,
    hash: String,
    /// How many samples were folded in.
    n: usize,
    /// ⚠ NON-DEGENERACY GUARD. A function that early-returns a constant (0.0, NaN) for every
    /// curve hashes the SAME on both platforms and would be read as "does not diverge" — a
    /// vacuous pass. This is how many distinct finite values the samples took, so a row reading
    /// `distinct=1` means that row's verdict is worthless, not reassuring.
    distinct: usize,
}

/// Fold every production function below over the corpus and hash each one's outputs.
///
/// Shared by the printing probe above and by `converted_functions_are_platform_invariant` below,
/// so the measurement and the gate can never drift apart: the gate pins exactly the hashes the
/// probe prints.
fn production_hashes() -> Vec<Row> {
    // One sample list per function, collected over every (seed, vol) curve. Kept as values
    // rather than folded straight into a hash so `distinct` can be reported — see the note there.
    let mut h = [(); 18].map(|_| Vec::<f64>::new());
    for seed in 0..SEEDS {
        for (vi, &vol) in VOLS.iter().enumerate() {
            let eq = synth_equity(2000, seed, vol);
            let bench = synth_equity(2000, seed + 7919, vol);
            let pnl: Vec<f64> = eq.windows(2).map(|w| (w[1] - w[0]) / w[0] * 0.1).collect();

            // equity_from_pnl returns a VECTOR, so fold every element in.
            for v in signal_backtest::equity_from_pnl(&pnl) {
                h[0].push(v);
            }
            h[1].push(metrics::cagr(&eq, 365.0));
            h[2].push(metrics::k_ratio(&eq));
            h[3].push(metrics::ulcer_index(&eq));
            h[4].push(metrics::sortino(&eq, 365.0));
            h[5].push(metrics::returns_skewness(&eq));
            h[6].push(metrics::returns_kurtosis(&eq));
            h[7].push(benchmark::r_squared(&eq, &bench));
            h[8].push(benchmark::alpha(&eq, &bench, 365.0, 0.02));
            h[9].push(benchmark::treynor_ratio(&eq, &bench, 365.0, 0.02));

            let m = overfit::sharpe_moments(&eq);
            h[10].push(overfit::probabilistic_sharpe_ratio(
                m.sr_per_obs,
                m.n_obs,
                0.0,
                m.skew,
                m.kurt,
            ));
            h[11].push(overfit::expected_max_sharpe(0.1 + vi as f64, 12 + seed as usize));
            let trials: Vec<f64> =
                synth_equity(50, seed + 104_729, vol).iter().map(|v| v - 1.0).collect();
            h[12].push(overfit::deflated_sharpe_ratio(
                m.sr_per_obs,
                &trials,
                m.n_obs,
                m.skew,
                m.kurt,
            ));
            // T observations x N trials — T must be large relative to `n_splits`.
            let base = synth_equity(240, seed, vol);
            let matrix: Vec<Vec<f64>> =
                base.iter().map(|v| (0..8).map(|k| v - 1.0 + k as f64 * 0.001).collect()).collect();
            h[13].push(overfit::pbo_cscv(&matrix, 6));

            // ⚠ The two rows the ORIGINAL 14 did not cover, added because they are the ones a
            // consumer actually depends on: `metrics::sharpe` is the headline number of the
            // author's cohort study, and `metrics::mean_variance` is the shared core it and five
            // other metrics fold through. Neither reduces to any row above — `sortino` has its own
            // downside fold and `risk_return_ratio` is unannualized — so before this they were
            // converted but UNMEASURED.
            h[14].push(metrics::sharpe(&eq, 365.0));
            let rets = metrics::returns(&eq);
            h[15].push(metrics::mean_variance(rets.iter().copied(), 1.0).1);

            // ⚠ The two `stats` rows — the module this file's own header names and this table did
            // not contain. See the header's CORRECTION paragraph for why they were missing and for
            // the honest statement of what pinning them buys.
            //
            // `block_bootstrap_sharpe` gets a 400-bar slice rather than the whole 1999-bar series,
            // and 64 resamples rather than the hundreds a real confidence interval would use,
            // because its cost is O(resamples x bars) inside a 600-curve loop and this test is NOT
            // `#[ignore]`d — it runs on every PR and on the Windows dev box. Both returned
            // percentiles are folded in: they are two independent CONTINUOUS values per curve, so
            // this row's `distinct` count is ~1200 over 1200 samples and its non-vacuity is not in
            // question the way `hansens_spa`'s below is.
            let (boot_lo, boot_hi) =
                stats::block_bootstrap_sharpe(&pnl[..400], 20, 64, 365.0, seed);
            h[16].push(boot_lo);
            h[16].push(boot_hi);

            // ⚠ `hansens_spa` returns `at_or_above as f64 / n_resamples as f64`, so its value set
            // is bounded by `n_resamples + 1`. It is the ONLY function in this table whose output
            // is discrete, which makes the `distinct > 100` non-vacuity assert in
            // `converted_functions_are_platform_invariant` a real constraint on the CORPUS rather
            // than a formality. Two deliberate choices answer it, and neither touches the assert:
            //
            //   1. The columns carry NO systematic drift. Each is one `synth_equity` curve's
            //      per-bar return MINUS an independent second curve's, so the `+0.005 * vol`
            //      per-bar drift `synth_equity` builds in (its `u - 0.495` offset) cancels and
            //      what remains is sampling noise. That places the input under this test's own
            //      null — every candidate has zero expected PnL — where `obs_max` is a draw from
            //      the same law the recentred bootstrap redraws, so the p-value is approximately
            //      UNIFORM on [0, 1]. Feeding it the drifting `eq`-derived pnl instead would push
            //      `obs_max` clean outside the null's bulk and every one of the 600 samples would
            //      report the same 0.0 or 1.0: a vacuous row wearing a pass, which is exactly the
            //      failure `overfit::pbo_cscv` already demonstrates below.
            //   2. `n_resamples` VARIES with the seed (120..320), so the discrete grid the p-value
            //      lands on differs per seed. Two samples that count the same share of draws still
            //      land on different rationals.
            //
            // ⚠ That is ARGUED, not MEASURED — it was written on a box that could not run the
            // test. If this row reports `distinct` at or below 100, the fix is a corpus that
            // spreads the p-value further (more resamples, more columns, a longer series), NEVER a
            // lowered threshold: the threshold is the only thing standing between this row and a
            // hash that agrees across platforms because it is constant.
            let spa_cols: Vec<Vec<f64>> = (0..3u64)
                .map(|j| {
                    let a = synth_equity(97, seed + 5_003 * (j + 1), vol);
                    let b = synth_equity(97, seed + 5_003 * (j + 1) + 2_711, vol);
                    a.windows(2)
                        .zip(b.windows(2))
                        .map(|(x, y)| (x[1] - x[0]) / x[0] - (y[1] - y[0]) / y[0])
                        .collect()
                })
                .collect();
            h[17].push(stats::hansens_spa(&spa_cols, 120 + seed as usize, 8, seed));
        }
    }

    LABELS
        .iter()
        .zip(h.iter())
        .map(|(&label, acc)| {
            let mut acc_h = Fnv::new();
            for &v in acc.iter() {
                acc_h.push(v);
            }
            let mut seen: Vec<u64> =
                acc.iter().filter(|v| v.is_finite()).map(|v| v.to_bits()).collect();
            seen.sort_unstable();
            seen.dedup();
            Row { label, hash: acc_h.hex(), n: acc.len(), distinct: seen.len() }
        })
        .collect()
}

const LABELS: [&str; 18] = [
    "signal_backtest::equity_from_pnl (exp)",
    "metrics::cagr (powf)",
    "metrics::k_ratio (ln + powf)",
    "metrics::ulcer_index (powf)",
    "metrics::sortino (powf)",
    "metrics::returns_skewness (powf)",
    "metrics::returns_kurtosis (powf)",
    "benchmark::r_squared (powf)",
    "benchmark::alpha (powf)",
    "benchmark::treynor_ratio (powf)",
    "overfit::probabilistic_sharpe_ratio",
    "overfit::expected_max_sharpe (ln+exp)",
    "overfit::deflated_sharpe_ratio",
    "overfit::pbo_cscv (ln)",
    "metrics::sharpe (powf via mean_variance)",
    "metrics::mean_variance variance (powf)",
    "stats::block_bootstrap_sharpe (variance fold)",
    "stats::hansens_spa (variance fold)",
];

// =================================================================================================
// PART C — the GATE. Part A and Part B above only MEASURE; nothing about them fails a build.
// =================================================================================================

/// The hash every function in [`LABELS`] must produce, on EVERY platform.
///
/// ⚠ These are not "the Linux numbers" — that is the whole point. Each hex literal was RECORDED BY
/// RUNNING this file on Linux (the CI box/glibc) and on Windows (MSVC CRT) after the `libm` conversion,
/// and the two runs agreed bit-for-bit; the value below is that agreed hash. Before the conversion
/// three of them did NOT agree — `signal_backtest::equity_from_pnl`, `metrics::k_ratio` and
/// `metrics::returns_skewness` — which is the defect this gate exists to keep fixed.
///
/// ⚠ **Re-record by RUNNING, never by editing a digit.** If a row goes red, the question is which
/// platform moved and why; a hash edited to match one box silently un-does the fix on the other.
///
/// ⚠ **A row may be added spelled `"RECORD"`, and that is a PLACEHOLDER which fails by
/// construction.** The convention exists because rows are sometimes written on a box that cannot
/// run cargo, and a plausible-looking hex literal invented there would be indistinguishable from a
/// recorded one while pinning nothing. `"RECORD"` cannot match any FNV output, so the gate stays
/// red until a human runs this file on Linux AND on Windows, confirms the two runs agree
/// bit-for-bit, and pastes the agreed hash in. A red row spelled `"RECORD"` is an UNFINISHED
/// recording, not a regression — that distinction is why the placeholder is a word rather than a
/// wrong number.
///
/// The two `stats` rows below were added that way on 2026-08-25 and have since been recorded:
/// Windows/MSVC `dev` first, then confirmed unchanged on Linux/glibc `dev`. No placeholder
/// remains.
const PLATFORM_INVARIANT: [(&str, &str); 18] = [
    ("signal_backtest::equity_from_pnl (exp)", "9a4a4f5f88ff6d09"),
    ("metrics::cagr (powf)", "6f5a4e3da73d1d54"),
    ("metrics::k_ratio (ln + powf)", "5de8d55036ff8506"),
    ("metrics::ulcer_index (powf)", "1e723b270aaf2839"),
    ("metrics::sortino (powf)", "96396649db198650"),
    ("metrics::returns_skewness (powf)", "33deffdb709e8090"),
    ("metrics::returns_kurtosis (powf)", "52e459f054c24eda"),
    ("benchmark::r_squared (powf)", "3f9cc2357d213960"),
    ("benchmark::alpha (powf)", "dcd03fa02e832ea3"),
    ("benchmark::treynor_ratio (powf)", "8a2741efe37bc8cf"),
    ("overfit::probabilistic_sharpe_ratio", "64666a57a0d95483"),
    ("overfit::expected_max_sharpe (ln+exp)", "de5ba314cd70fe40"),
    ("overfit::deflated_sharpe_ratio", "61f087606ada10e3"),
    ("overfit::pbo_cscv (ln)", "d0549f149dd63a25"),
    ("metrics::sharpe (powf via mean_variance)", "8fcceef2b709c2bc"),
    ("metrics::mean_variance variance (powf)", "b8cff1173f04050b"),
    ("stats::block_bootstrap_sharpe (variance fold)", "e141b501dbbb88e4"),
    ("stats::hansens_spa (variance fold)", "6706b4322af784c1"),
];

/// The cross-platform proof, as a GATE rather than a diff a human has to remember to run.
///
/// This is NOT `#[ignore]`d: CI runs it on Linux every PR, and `just t vike-analytics` runs it on
/// the Windows dev box. Both compare against the same committed table, so the two platforms are
/// held equal without anyone having to diff two terminal windows.
///
/// ⚠ `overfit::pbo_cscv` is pinned here like the rest but its row proves almost nothing: the
/// synthetic matrices are monotone across trials, so it reports `distinct=1` and its hash would
/// match on two platforms even if the function were wildly unstable. It is pinned anyway (a
/// pinned constant is still a regression tripwire for a REFACTOR) and its vacuity is asserted
/// below so the weakness stays measured instead of forgotten.
#[test]
fn converted_functions_are_platform_invariant() {
    let got = production_hashes();
    assert_eq!(got.len(), PLATFORM_INVARIANT.len(), "row count drifted from the pinned table");

    let mut bad = Vec::new();
    for (r, (want_label, want_hash)) in got.iter().zip(PLATFORM_INVARIANT.iter()) {
        assert_eq!(&r.label, want_label, "label order drifted from the pinned table");
        if r.hash != *want_hash {
            bad.push(format!("  {:<40} want={want_hash} got={}", r.label, r.hash));
        }
    }
    assert!(
        bad.is_empty(),
        "vike-analytics output moved away from the platform-invariant pin:\n{}\n\n\
         This is either (a) an intended numeric change — then re-record by RUNNING this test on \
         BOTH Windows and Linux and pinning the hash they AGREE on, or (b) a transcendental that \
         went back to the platform's libm (`powf`/`ln`/`exp` instead of `libm::pow`/`log`/`exp`), \
         in which case the two platforms will NOT agree and the fix is in the source, not here.",
        bad.join("\n")
    );

    // The non-degeneracy half: a hash that matches because every sample is the same value is a
    // vacuous pass. Assert the corpus is actually exercising these functions, and pin the ONE
    // row that is known not to be (see the doc above) so its weakness cannot quietly spread.
    for r in &got {
        if r.label == "overfit::pbo_cscv (ln)" {
            assert_eq!(
                r.distinct, 1,
                "pbo_cscv became non-degenerate ({} distinct values) — that is an IMPROVEMENT, \
                 but the doc on PLATFORM_INVARIANT and the probe's module header both say this \
                 row is vacuous, so update them rather than deleting this assert",
                r.distinct
            );
            continue;
        }
        assert!(
            r.distinct > 100,
            "{}: only {} distinct values over {} samples — this row's verdict is vacuous",
            r.label,
            r.distinct,
            r.n
        );
    }
}

/// The `f64` methods whose last bit is the PLATFORM's business rather than IEEE 754's, as bare
/// names. [`banned_needles`] renders each into the spellings that reach it.
///
/// ⚠ `.powi(` is banned at EVERY arity, and the narrower `.powi(2)` this list used to carry is the
/// interesting history. `powi` is a `pow()` libcall in an MSVC `dev` build, so no arity of it is
/// portable — but the cure is a MULTIPLICATION rather than `libm`, and the entry was kept to the
/// square because a hand-written chain for `powi(3)` and up was believed to MOVE values by fixing
/// an association order LLVM picks itself.
///
/// That belief was half right, and the wrong half is why the ban stayed too narrow. LLVM lowers a
/// constant-exponent `powi` by BINARY EXPONENTIATION, so the value-preserving spelling of `x⁴` is
/// `(x²)²` — not the left-to-right `x * x * x * x` the argument had in mind, which genuinely does
/// round differently. Written as the square of the square it moves nothing, exactly as `powi(2)`
/// did. `crates/vike-indicators/src/math.rs`'s `cube` / `quart` are that spelling, and
/// `libm_cross_platform_pin`'s committed table not moving when they landed is the evidence. So the
/// arity carve-out had no argument left to stand on and is gone.
///
/// This crate reaches zero either way — its one `.powi(` sits inside a `#[cfg(test)]` module, so
/// widening the entry cost nothing HERE and buys the next edit, which has no such luck.
///
/// ⚠ The inverse-trig and hyperbolic family (`atan` .. `hypot`) joined for the same reason and at
/// the same zero cost: `crates/vike-analytics/src` contains not one call to any of them today, so
/// that widening changed no verdict — it removed a gap the next edit could walk into. They belong
/// on the list on the merits, not by association: none of `atan`, `asin`, `acos`, `sinh`, `cosh`,
/// `tanh`, `cbrt` or `hypot` is required by IEEE 754 to be correctly rounded, so each is a
/// platform-libm call whose last bit is MSVC's or glibc's business. ⚠ Of the nine, exactly ONE is
/// measured rather than argued — Part A's `atan/ols-slope` row sweeps `f64::atan` against
/// `libm::atan` and reports the divergence. The other eight rest on the IEEE 754 argument alone,
/// which is the same argument the original thirteen entries rest on for every domain Part A does
/// not sweep either.
///
/// ⚠ **WIDENED 2026-08-26 from 22 names to 26** — `asinh`, `acosh`, `atanh`, `sin_cos`. Not one of
/// the four was reachable from any of the original 22 by substring, which is the only way this
/// list has ever gained coverage, and the claim was checked needle by needle rather than assumed.
/// Every pattern [`banned_needles`] builds ends in a `(` immediately after the name, so `.asin(`
/// cannot match `.asinh(` (an `h` sits where the paren must be) and `.acos(`/`.atan(` cannot match
/// `.acosh(`/`.atanh(` for the same reason. Every pattern also starts with a `.` or a `::`
/// immediately before the name, so `.sinh(`/`.cosh(`/`.tanh(` cannot match `.asinh(`/`.acosh(`/
/// `.atanh(` — the character before `sinh` there is an `a`, not a dot. `.sin_cos(` escapes `.sin(`
/// by the paren rule and `.cos(` by the dot rule at once. None of the four appears anywhere under
/// this crate's `src/`, so again the widening changed no verdict.
///
/// ⚠ `to_degrees` / `to_radians` are deliberately ABSENT despite reading like trig. They are not
/// libm calls: std implements each as a single multiplication by a constant, and a multiply is
/// required by IEEE 754 to be correctly rounded, so they are already platform-invariant for the
/// same reason `sqrt` is. Banning them would cost a real conversion (there is no `libm` equivalent
/// to convert TO) and buy nothing.
const BANNED_FNS: [&str; 26] = [
    "powf", "ln", "log", "log2", "log10", "exp", "exp2", "exp_m1", "ln_1p", "sin", "cos", "tan",
    "powi", "atan", "atan2", "asin", "acos", "sinh", "cosh", "tanh", "cbrt", "hypot", "asinh",
    "acosh", "atanh", "sin_cos",
];

/// Every [`BANNED_FNS`] name in BOTH spellings that reach the platform's libm.
///
/// ⚠ **The UFCS half is new as of 2026-08-26, and its absence was a hole rather than a tidiness
/// point.** This gate banned `.exp(` and nothing else, so `f64::exp(x)` — the same inherent
/// method, called through its fully-qualified path, compiling to the same libcall — matched
/// NOTHING and would have walked straight past a gate whose entire subject it is. Neither spelling
/// is more correct Rust than the other and rustfmt does not rewrite one into the other, so which
/// one an author reaches for is a coin flip. The `f64::NAME(` needle covers
/// `std::primitive::f64::NAME(` and `core::primitive::f64::NAME(` for free, because both END with
/// exactly that pattern.
///
/// ⚠ **What it still cannot see, stated rather than implied:** `<f64>::exp(x)` (the angle-bracket
/// spelling — `f64>::exp(` is what appears, and no needle here ends in `>`), a call reached
/// through a generic bound (`T: Float`, `num_traits::Float::exp`), and a call split across two
/// lines by a `max_width` wrap. None of the three occurs under this crate's `src/` today. A list
/// of spellings is an enumeration, and an enumeration is never a proof.
fn banned_needles() -> Vec<String> {
    BANNED_FNS.iter().flat_map(|f| [format!(".{f}("), format!("f64::{f}(")]).collect()
}

/// Every `#[cfg(test)]` item's line range in one file, as `(start, end, closed)` — `start`
/// inclusive, `end` EXCLUSIVE, both zero-based; `closed` says whether the item's end was actually
/// located rather than assumed, so the one silent failure mode can be asserted on.
///
/// The end is found by INDENTATION rather than by counting braces, and that is a deliberate refusal
/// to put a Rust lexer inside a gate: this workspace's test modules are full of `format!("{}")` and
/// of assert messages continued across lines with a trailing `\`, so a naive brace count is wrong
/// on exactly the files that matter. Indentation is exact here for a structural reason — `cargo fmt
/// --check` is CI's FIRST gate, so every file in the tree is rustfmt output, and rustfmt closes a
/// block with a `}` alone on a line at the block's own indentation. Measured 2026-08-26: all 52
/// `#[cfg(test)]` markers under the three gated `src/` trees sit at column 0, and each is followed
/// either by a line ending in `{` or by a `;` (the `mod NAME;` declaration form, which exists in
/// `crates/vike-mm/src/lib.rs` and is why the `;` arm is not written for a hypothetical).
///
/// Two ways it can be wrong, and only one is quiet. A line inside the item that IS the closing
/// pattern — a `}` at the item's indentation inside a multi-line string, or inside a
/// `#[rustfmt::skip]` block — ends the range early, the scan resumes over test code, and test code
/// trips the ban loudly. No closing pattern at all runs the range to EOF, which is the old
/// `break`'s behaviour and is silent; that is what `closed` is for.
///
/// ⚠ This is a THIRD copy of a shape that also lives in
/// `crates/vike-indicators/tests/libm_platform_probe.rs` and
/// `crates/vike-mm/src/platform_probe.rs`. Sharing it would mean a crate all three can depend on
/// from a test target, which none of them has and which would drag a new edge into the layer
/// graph for eleven lines of string handling — these three gates are already deliberate triplicates
/// (each crate holds its own, because a gate that lives elsewhere is a gate a crate can be moved
/// out from under). If a fourth appears, that is the moment to reconsider.
fn cfg_test_ranges(lines: &[&str]) -> Vec<(usize, usize, bool)> {
    let mut items = Vec::new();
    let mut i = 0usize;
    while i < lines.len() {
        let trimmed = lines[i].trim_start();
        // Prose FIRST, for the reason the gate's doc gives: a doc comment naming the attribute in
        // backticks must not be able to open a range.
        if trimmed.starts_with("//") || !trimmed.starts_with("#[cfg(test)]") {
            i += 1;
            continue;
        }
        let mut close = " ".repeat(lines[i].len() - trimmed.len());
        close.push('}');
        let mut opened = false;
        let mut end = lines.len();
        let mut closed = false;
        for (j, line) in lines.iter().enumerate().skip(i) {
            let tail = line.trim_end();
            if !opened {
                // The header may span lines (further attributes, the `mod` on the next line).
                if tail.ends_with('{') {
                    opened = true;
                } else if tail.ends_with(';') {
                    end = j + 1;
                    closed = true;
                    break;
                }
                continue;
            }
            if tail == close {
                end = j + 1;
                closed = true;
                break;
            }
        }
        items.push((i, end, closed));
        i = end;
    }
    items
}

/// The RANGE walk, proved on a synthetic file because this crate's tree cannot prove it.
///
/// ⚠ **Non-vacuity for a fix that changes nothing here, which is the only kind of non-vacuity
/// available.** No file under `crates/vike-analytics/src` puts a test module in the middle, so a
/// tree-shaped assertion — "some production line below a marker was scanned" — would be FALSE
/// today and could only be written by weakening it into something that proves nothing. The
/// mechanism is held here instead: production before a test module, a banned call inside it,
/// production after it, and a banned call in each production half. It also covers the `;` header
/// form, which no brace-matching walk would handle by accident.
#[test]
fn the_test_module_cut_excludes_only_the_test_module() {
    // Every fixture row is a string literal, so the needles inside them are data rather than
    // calls; this file is under `tests/` and is never scanned by the gate below, which scans
    // `src/` only.
    let file = [
        "//! House style: naïve f64 folds, pure, in-file `#[cfg(test)]`.",
        "fn shipped_before() -> f64 {",
        "    x.exp()",
        "}",
        "",
        "#[cfg(test)]",
        "mod first_tests {",
        "    fn fixture() -> f64 {",
        "        y.ln()",
        "    }",
        "}",
        "",
        "fn shipped_after() -> f64 {",
        "    f64::powf(z, 2.0)",
        "}",
        "",
        "#[cfg(test)]",
        "mod tests;",
        "",
        "fn shipped_last() -> f64 {",
        "    w.sin_cos().0",
        "}",
    ];
    let items = cfg_test_ranges(&file);
    assert_eq!(items.len(), 2, "both `#[cfg(test)]` items must be found: {items:?}");
    assert_eq!(items[0], (5, 11, true), "the braced module spans its own lines only");
    assert_eq!(items[1], (16, 18, true), "the `mod tests;` declaration ends at its semicolon");
    // The control arm: the module doc on line 1 names the attribute in backticks. If prose could
    // open a range, `items[0]` would start at 0 and every assertion here would still pass while
    // the gate scanned nothing at all.
    assert_eq!(items[0].0, 5, "a `#[cfg(test)]` inside a comment must not open a range");

    let banned = banned_needles();
    let hits: Vec<usize> = file
        .iter()
        .enumerate()
        .filter(|(i, line)| {
            !items.iter().any(|&(s, e, _)| *i >= s && *i < e)
                && !line.trim_start().starts_with("//")
                && banned.iter().any(|p| line.contains(p.as_str()))
        })
        .map(|(i, _)| i)
        .collect();
    assert_eq!(
        hits,
        [2, 13, 20],
        "the scan must see the production call BEFORE the test module (line 3), the one AFTER it \
         (line 14), and the one after the `mod tests;` declaration (line 21) — and must NOT see \
         the fixture call on line 9. A `break` at the first marker sees only line 3. \
         Saw (zero-based): {hits:?}"
    );
}

/// The SOURCE half of the same rule: production code in this crate may not call `f64`'s inherent
/// transcendentals at all.
///
/// The hash gate above proves today's outputs agree; this one stops the next edit from
/// reintroducing a platform call in a spot the corpus happens not to reach. `sqrt` is
/// deliberately absent from the banned set — IEEE 754 requires it to be correctly rounded, so it
/// is already platform-invariant (`lib.rs`'s "Cross-platform determinism" section).
///
/// Scope: each source file MINUS its `#[cfg(test)]` item RANGES. Test code inside one builds
/// expected values with whatever is convenient and is not shipped.
///
/// ⚠ **RANGES as of 2026-08-26 — it used to `break` at the first marker, and a `break` is not the
/// same rule.** A `break` says "the shipped half of a file ends at its first test module", which is
/// true only when the test module is LAST. For a file with a test module in the MIDDLE it discards
/// every production line below it, silently, while still reporting a confident zero. Measured: no
/// file under `crates/vike-analytics/src` has a mid-file test module today, so this change moves
/// NOTHING here and no verdict differs — it is a trap removed, not a finding. The crate where the
/// same `break` was actively wrong is `vike-mm`, whose in-`src` twin
/// (`crates/vike-mm/src/platform_probe.rs`'s `cfg_test_ranges`) carries the measurement: 1,612
/// production lines discarded across five files, two `libm` sites among them, and a module doc
/// that had copied the resulting undercount into prose. [`cfg_test_ranges`] is the shared shape;
/// [`the_test_module_cut_excludes_only_the_test_module`] is the non-vacuity arm, and it is a
/// FIXTURE rather than a tree measurement precisely because this tree cannot exercise the case.
///
/// ⚠ **The cut is LINE BY LINE, and it used to be a whole-file `text.find("#[cfg(test)]")`.** That
/// spelling truncates at the first occurrence of the string ANYWHERE in the file — inside a doc
/// comment included — and the `//`-skipping filter below could not rescue it, because that filter
/// runs on the already-truncated text. A single house-style module doc reading ``pure, in-file
/// `#[cfg(test)]` `` at the top of a file would have cut the entire file away and reported a
/// confident zero for it. That is not hypothetical: `crates/vike-mm/src/xemm/pricing.rs` and its
/// three siblings carry exactly that line today. It is LATENT here only because no file under
/// `crates/vike-analytics/src` happens to mention the marker in prose — which is a fact about this
/// crate this month, not a property anything holds. Testing the comment filter FIRST, per line,
/// puts it ahead of the cut, so prose can neither trip the ban nor blind the scan.
///
/// ⚠ **And a marker is recognised by `starts_with`, not equality.** `#[cfg(test)] mod tests {` on
/// one line is a spelling this workspace uses; an equality test walks straight past it and keeps
/// scanning test code as though it were shipped, which produces the opposite failure — a false
/// POSITIVE naming a `.sin(` that never ships.
///
/// ⚠ **It RECURSES, and that is insurance rather than need.** `crates/vike-analytics/src` is flat
/// today — 17 files, zero subdirectories — so a flat `read_dir` (what this used) sees everything
/// and the recursion changes no verdict. The `crates/vike-indicators` twin is the same gate over a
/// crate that DOES nest, and there the flat spelling would have missed `statistics.rs` and
/// `overlap.rs`, the two files it exists to hold converted. The first `src/<subdir>/` added here
/// would inherit that failure silently: nothing goes red, the gate simply certifies a directory it
/// never opened. Copying the twin's stack walk costs six lines and removes that future.
#[test]
fn production_code_calls_libm_not_the_platform() {
    let banned = banned_needles();
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut found = Vec::new();
    let mut scanned: Vec<String> = Vec::new();
    // A `#[cfg(test)]` item whose end could not be located swallows the rest of its file exactly
    // the way the old `break` did. That is the one failure of the range walk that would be QUIET,
    // so it is collected and asserted rather than absorbed.
    let mut unclosed: Vec<String> = Vec::new();
    let mut stack = vec![src];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("crate has a src/ directory") {
            let path = entry.expect("readable dir entry").path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            let text = std::fs::read_to_string(&path).expect("readable source file");
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            scanned.push(name.clone());
            // Production == everything OUTSIDE a `#[cfg(test)]` item. Every file in this crate
            // keeps its `#[cfg(test)] mod tests` last, so today that is the same set the old
            // `break` produced — see this test's doc for why the rule changed anyway. The two
            // filters are ORDERED, and the order is the fix: comments are dropped FIRST, so a
            // marker mentioned in prose can neither blind the scan nor trip the ban.
            let lines: Vec<&str> = text.lines().collect();
            let test_items = cfg_test_ranges(&lines);
            for &(start, _, closed) in &test_items {
                if !closed {
                    unclosed.push(format!("  {name}:{}", start + 1));
                }
            }
            for (i, line) in lines.iter().enumerate() {
                if test_items.iter().any(|&(start, end, _)| i >= start && i < end) {
                    continue; // inside a `#[cfg(test)]` item — not shipped
                }
                let code = line.trim_start();
                if code.starts_with("//") {
                    continue; // a doc comment naming `powf` is prose, not a call
                }
                for pat in &banned {
                    if line.contains(pat.as_str()) {
                        found.push(format!("  {name}:{} {}", i + 1, line.trim()));
                    }
                }
            }
        }
    }
    assert!(
        unclosed.is_empty(),
        "a `#[cfg(test)]` item's closing line could not be located, so everything below it went \
         UNSCANNED — the exact hole the old `break` left, wearing a different cause. See \
         `cfg_test_ranges`: the end is the item's own indentation followed by a lone `}}`, or a \
         `;` for a `mod NAME;` declaration.\n{}",
        unclosed.join("\n")
    );
    // Non-vacuity, in two halves, because an empty `found` is indistinguishable from a scan that
    // opened nothing. The NAMED files are the sharper half — the three that carry the code this
    // gate exists to hold converted: `metrics.rs` (eleven `libm::pow`, two `libm::log`, one
    // `libm::exp`), `overfit.rs` (`libm::erfc`/`log`/`exp`) and `stats.rs` (no libm call at all,
    // and precisely therefore the file where a `.powi(2)` would slide back in unnoticed — it is
    // where the two folds `PLATFORM_INVARIANT` now pins live). If any of the three went unopened,
    // this gate proved nothing about the code it exists for. The count floor is the blunt half: it
    // catches an extension or directory filter that quietly stopped matching, without naming a
    // file the author must then keep in step. (The twin in `crates/vike-indicators` names files
    // only, because its risk is specifically a walk that stops at the top level; this crate is
    // flat, so both halves are cheap and neither is redundant with the other.)
    for must in ["metrics.rs", "overfit.rs", "stats.rs"] {
        assert!(
            scanned.iter().any(|s| s == must),
            "the scan never opened {must}, so an empty result proves nothing. Scanned: {scanned:?}"
        );
    }
    assert!(
        scanned.len() >= 10,
        "expected to scan the whole crate, only saw {} files: {scanned:?}",
        scanned.len()
    );
    assert!(
        found.is_empty(),
        "production code must call the `libm` CRATE, not `f64`'s platform-libm methods \
         (`libm::pow`/`libm::log`/`libm::exp`), so a value does not depend on which box ran it \
         — see `lib.rs`'s \"Cross-platform determinism\" section. BOTH spellings are banned, \
         `x.exp()` and `f64::exp(x)`: they are the same inherent method and the same libcall, and \
         a list naming only the first is a list with a hole in it (see `banned_needles`):\n{}",
        found.join("\n")
    );
}
