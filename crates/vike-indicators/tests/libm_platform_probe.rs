//! MEASUREMENT probe **and** the committed cross-platform GATE for the indicators whose value
//! passes through a transcendental (`ln`, `log10`, `exp`, `atan`).
//!
//! IEEE 754 requires `+ - * /` and `sqrt` to be correctly rounded; it does NOT require the
//! transcendental functions to be, so two C runtimes may disagree in the last bit.
//!
//! ⚠ This header used to add "`powi` is excluded because it lowers to a multiply chain, not a libm
//! call" — and the measurement further down this same file REFUTES it: on MSVC in a `dev` build
//! `llvm.powi` becomes the CRT's `pow()`. The exclusion is gone, `powi` is banned at every arity,
//! and the sentence is kept here as a correction rather than deleted, because it is the assumption
//! that hid the defect and it reads as obviously true.
//!
//! This file now holds FOUR tests — the header said TWO until the SOURCE gate joined them and
//! THREE until that gate's own cut got a non-vacuity arm. The difference between the first two is
//! the difference between measuring a problem and holding it closed; the third is what stops the
//! other two from certifying a tree that drifted out from under them; the fourth is what stops the
//! third from certifying a file it never actually read to the end:
//!
//! - [`libm_platform_probe`] — `#[ignore]`d, prints a table, asserts nothing. Run it on both
//!   boxes and diff. It is how the divergence below was found and how a future one is
//!   characterised.
//! - [`libm_cross_platform_pin`] — an ORDINARY test, so every `just t vike-indicators`, every CI
//!   roster run and every dev-box run executes it. It hashes the same functions over a fixed
//!   sweep and asserts the hashes equal [`PINNED`]. Those constants are one committed set for
//!   ALL platforms, which is the whole claim: Windows and Linux compute the same bits.
//! - [`production_code_calls_libm_not_the_platform`] — the SOURCE half, and the only one of the
//!   three that speaks for code the corpus never drives. A hash gate can only assert about the
//!   call sites its sweep reaches; this one reads every production line under `src/` and refuses
//!   the platform spellings outright, so a site the sweep happens to miss cannot quietly revert
//!   to `f64::ln`.
//! - [`the_test_module_cut_excludes_only_the_test_module`] — the SOURCE gate's cut, proved on a
//!   synthetic file. The gate decides what "production" means by excluding each `#[cfg(test)]`
//!   item's line RANGE, and a cut that silently ate too much would leave the gate reporting a
//!   confident zero over a region it never opened. No file in this crate exercises the hard case
//!   (a test module in the MIDDLE), so a tree measurement could not hold it — hence the fixture.
//!
//! ```text
//! <cargo> test -p vike-indicators --test libm_platform_probe -- --ignored --nocapture
//! ```
//!
//! ⚠ The bar generator here is PURE ARITHMETIC over an LCG — deliberately not the `.sin()`/
//! `.cos()` mix that `tests/window_pin.rs`'s `synth_bars` uses. `sin`/`cos` are themselves
//! platform-dependent (measured), so generating inputs with them would make every column below
//! diverge because of the INPUTS rather than because of the indicator under test.
//!
//! # MEASURED 2026-08-25 — Windows 11 / MSVC CRT against the CI box / glibc, rustc 1.96.0
//!
//! Against the PLATFORM's libm, EVERY indicator here diverged: `fisher`, `hvol`, `chop`,
//! `linearreg_angle`, `pair::spread`, `pair::correl_log` on the seed sweep, and `alma` on the
//! PARAMETER grid.
//!
//! ⚠ Two traps this file exists to have already fallen into, both of which produced a confident
//! "identical" that a wider sweep then refuted:
//!
//! 1. **One series is not a measurement.** With a single 3000-bar series, `fisher` and `hvol`
//!    both reported identical hashes on the two platforms. They diverge across 180 series. These
//!    indicators reduce a window to one value per bar, so most 1-ulp differences are rounded away
//!    and a single sample lands on "same" far more often than the underlying instability implies.
//! 2. **ALMA's inputs are its PARAMETERS, not its data.** Its Gaussian weights are computed from
//!    `(period, offset, sigma)` alone, so the seed sweep exercises exactly as many `exp` calls as
//!    a single series does. Both default-ish parameter sets agree on both platforms; the registry
//!    grid does not.
//!
//! # ⚠ THE VERDICT IS `docs/decisions/0032`, NOT THIS FILE
//!
//! This file used to close by saying the measurement justified NO production change, on the
//! `vike-analytics` twin's argument that the `libm` CRATE disagrees with glibc too, so a
//! substitution does not merely make Windows agree with Linux — it MOVES the value on Linux, where
//! CI and live trading run.
//!
//! `docs/decisions/0032-transcendentals-come-from-the-libm-crate-not-the-platform.md` is the
//! record that settles it, and it is `accepted`: production code calls the `libm` crate wherever
//! IEEE 754 does not require correct rounding, per-crate, each PR re-recording what it moves. The
//! cost above is real and 0032 states it as the price rather than denying it. **Do not re-derive
//! that argument here** — the record carries it, including the condition that would reopen it.
//!
//! What belongs in THIS file is the part 0032 could not know: what the conversion actually cost
//! *this crate*.
//!
//! ⚠ **It cost nothing. NOTHING was re-recorded, and that was checked rather than assumed** —
//! which matters, because 0032's stop condition is a move to an ORACLE-pinned value.
//! `tests/parity.rs`, `tests/pairs_parity.rs` and `tests/feature_parity.rs` are all
//! SELF-CONSISTENCY gates (`on_bar` == `vectorize`, bit for bit), so both sides move together and
//! stay equal; `tests/fixtures/window_pin.tsv` pins `var` and `zscore`, which touch no
//! transcendental; and the frozen Python-oracle export in the repo-root `fixtures/` carries no
//! indicator output at all. The single absolute literal anywhere in the path is `parity.rs`'s
//! `("hvol", Some(0.0))`, and it is the exactly-zero log return of a constant window, which
//! `libm::log(1.0)` also returns exactly. So no value the oracle pins was touched, and the STOP
//! that 0021 and 0032 both describe was never reached.
//!
//! # ⚠ THE SECOND DEFECT THIS PIN FOUND, WHICH IS NOT A PLATFORM-LIBM PROBLEM AT ALL
//!
//! With every transcendental site converted, `hvol` STILL failed this pin on Windows — and
//! the cause turned out to be `f64::powi`, which both this file and its `vike-analytics` twin had
//! excluded from scope on the stated grounds that it "lowers to a multiply chain, not a libm
//! call". **On MSVC in a `dev` build, that is false.**
//!
//! ⚠ That sentence read "all seven transcendental sites" and the number was doing two jobs badly.
//! SEVEN is the count of INDICATORS — `fisher`, `hvol`, `chop`, `alma`, `linearreg_angle`,
//! `pair::spread`, `pair::correl_log`, exactly the rows named under the MEASURED heading above.
//! The count of CALL sites is ten, because `chop`, `pair::spread` and `pair::correl_log` each make
//! two. Neither number is worth trusting from prose:
//! `git grep -n 'libm::' -- crates/vike-indicators/src` is the derivation — it also lands on the
//! handful of module-doc lines that NAME `libm::`, so the calls are the hits sitting inside an
//! expression — and the bullet lists on [`SINGLE`] and [`PAIRED`] are transcribed from it.
//!
//! Staged FNV-1a hashes of `batch_hvol`'s pipeline over 48 log-return series, run on all four
//! (platform, profile) combinations:
//!
//! ```text
//!                            Linux debug  Linux release  Windows debug  Windows release
//!   closes (LCG, exact)         91f3429d      91f3429d       91f3429d        91f3429d
//!   v[i]/v[i-1] ratios          e55f2a56      e55f2a56       e55f2a56        e55f2a56
//!   libm::log(ratio)            f442aca0      f442aca0       f442aca0        f442aca0
//!   f64::ln(ratio)              3c2e1319      3c2e1319       607d5692        607d5692
//!   var via (x-mean).powi(2)    ab72e59a      ab72e59a       2d464289        ab72e59a
//!   var via (x-mean)*(x-mean)   ab72e59a      ab72e59a       ab72e59a        ab72e59a
//!   var via (x-mean).powf(2.0)  ab72e59a      ab72e59a       2d464289        ab72e59a
//!   var via libm::pow(.., 2.0)  ab72e59a      ab72e59a       ab72e59a        ab72e59a
//!   batch_hvol output           3a6baf18      3a6baf18       ebcf37a5        3a6baf18
//! ```
//!
//! Three readings, and the third is the one that cost the time:
//!
//! 1. **`libm::log` is bit-identical in all four cells** — the conversion does what it claims.
//! 2. **`f64::ln` splits by PLATFORM** — the defect being fixed, reproduced independently here.
//! 3. **`powi(2)` splits by PROFILE, on one platform only**, and in exactly that cell it hashes
//!    EQUAL to `powf(2.0)`. That equality is the identification: MSVC's `dev` build lowers
//!    `llvm.powi` to the CRT's `pow()`, a transcendental IEEE 754 does not require to be correctly
//!    rounded, while `release` expands it to the plain multiply everything else already computes.
//!
//! So a `powi` is not the "free" multiply its exclusion assumed, and `dev` is the profile
//! `just t vike-indicators` and CI's roster lane run. `crates/vike-indicators/src/math.rs`'s `sq`
//! is the cure and carries the rule; every `.powi(2)` in this crate now routes through it, which
//! moves no value in any of the other three cells.
//!
//! ⚠ **The `.powi(3)` / `.powi(4)` residual this file used to declare is CLOSED, and the reason it
//! was declared is worth keeping.** `batch_skew`, `batch_kurtosis` and `batch_mcginley` were left
//! calling `powi` on the argument that a hand-written multiply chain fixes an association order
//! LLVM chooses for itself, so converting them would MOVE values on every platform rather than
//! only on the broken one.
//!
//! ⚠ **That third name is a CORRECTION, and it is the one that made the pin below wrong.** #1538
//! converted FIVE call sites in THREE functions — two `cube`s in
//! `crates/vike-indicators/src/indicators/statistics.rs`'s `batch_skew`, two `quart`s in the same
//! file's `batch_kurtosis`, and the single `quart` in
//! `crates/vike-indicators/src/indicators/overlap.rs`'s `batch_mcginley`, whose
//! `period as f64 * quart(ratio)` is the fifth. #1538, #1540 and `docs/decisions/0032`'s first
//! amendment all called that third function "the T3 ratio" instead.
//! `crates/vike-indicators/src/indicators/overlap.rs`'s `batch_t3` contains no `powi`, no `cube`
//! and no `quart` at all — it is three generalized-DEMA passes of `(1 + v) * e1 - v * e2` and
//! nothing else. The misidentification is written down rather than quietly fixed because of what
//! it cost: `quart`'s second caller went unpinned while a row named `t3` sat in [`PINNED`] being
//! read as its coverage by #1538's commit message, #1540's, and `docs/decisions/0032` alike.
//!
//! Half of that residual argument is true, and it is the half that misleads. LLVM lowers a
//! constant-exponent `powi` by BINARY EXPONENTIATION: `x⁴` is `(x²)²`, not the running product
//! `((x·x)·x)·x`. The chain the argument imagined really does round differently — three roundings
//! against two — which is why the conversion looked unsafe. Spelled as the square of the square it
//! moves nothing, exactly as `powi(2)` did, and `crates/vike-indicators/src/math.rs`'s `cube` and
//! `quart` are that spelling: on the platforms whose LLVM already expanded the call the conversion
//! is a no-op, while the MSVC `dev` cell it was a libcall in is now a multiply like every other
//! cell.
//!
//! ⚠ **The evidence for that "moves nothing" is NOT the `PINNED` table failing to move, and this
//! passage used to say it was.** When the claim was made, that table's twelve rows were
//! `fisher` / `hvol` / `chop` / `alma` / `linearreg_angle` / `pair::*`, and not one of them
//! reaches `skew`, `kurtosis` or `mcginley` — so an unmoved table proved only that nothing ALREADY
//! COVERED had broken, which is a strictly weaker sentence than the one it was quoted for. The
//! real evidence is the rows #1540 ADDED: `skew[20.0]` and `kurtosis[20.0]` were RECORDED on
//! Windows/MSVC `dev` — the one profile that lowers `llvm.powi` to the CRT's `pow()`, and
//! therefore the only place the defect was ever visible — and then passed UNCHANGED on
//! Linux/glibc `dev`. One committed table, both platforms, over the converted call sites
//! themselves. `mcginley[14.0]` is the fifth site's row and was recorded the same way on
//! 2026-08-25: Windows/MSVC `dev` first, then unchanged on Linux/glibc `dev`.
//!
//! ⚠ The workspace-wide question this file could not answer is answered in
//! `docs/decisions/0032-transcendentals-come-from-the-libm-crate-not-the-platform.md`'s second
//! amendment, which is the AUTHORITY for it and is deliberately not restated here. The part that
//! belongs to this crate: `vike-analytics` reaches zero (its one `.powi(` sits below a
//! `#[cfg(test)]` marker) and both crates' source gates now ban the spelling at EVERY arity.
//!
//! ⚠ This paragraph used to close with "the one surviving `powi` outside them is
//! `crates/vike-backtest/src/search.rs`'s `2f64.powi(4)`", and that claim was wrong twice over. It
//! is kept visible because it is exactly the shape of statement this whole file exists to
//! distrust — a confident count, written from one row of a table. First, 0032's survey sorts the
//! remaining sites into THREE classes and the power-of-two literal is a single row of the first
//! one; the class that actually matters is the RUNTIME-exponent one, which the `cube`/`quart`
//! cure cannot reach at all and which includes venue-bridge production code computing price and
//! size decimals. Second, `search.rs`'s own `2f64.powi(4)` sits BELOW that file's `#[cfg(test)]`
//! marker — it is test code, which no source gate in the tree scans anyway (measured 2026-08-25).
//! Its disposition is unchanged and still right: a power of two is exact in binary floating point,
//! so every implementation returns exactly `16.0`, and converting it would only cost the next
//! reader the same investigation. But "cannot diverge" was never the same sentence as "is the only
//! one left".
use vike_indicators::{make_with, pair_make_with};
use vike_model::Bar;

/// FNV-1a over `f64::to_bits`, NaN canonicalized — every indicator below emits NaN warm-up
/// values, and a NaN's sign/payload is not architecturally pinned.
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

/// Deterministic bars from an LCG, built with `+ - * /` only so the INPUTS are bit-identical on
/// every platform. `u` is exact: a 53-bit integer divided by 2^53.
///
/// ⚠ Seeded and vol-parameterised deliberately. Most of these indicators reduce a window to one
/// value per bar, so a single 1-ulp difference is often rounded away — ONE series can report
/// "identical" for a site that does diverge on other data. Sweep, do not sample.
fn synth_bars(n: usize, seed: u64, vol: f64) -> Vec<Bar> {
    let mut s = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15).wrapping_add(0x2545_f491_4f6c_dd1d);
    let mut next = move || {
        s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (s >> 11) as f64 / (1u64 << 53) as f64
    };
    let mut bars = Vec::with_capacity(n);
    let mut close = 100.0f64;
    for i in 0..n {
        let open = close;
        close = open * (1.0 + (next() - 0.5) * vol);
        let jitter = next() * 0.5;
        bars.push(Bar {
            ts: i as i64 * 3_600_000,
            open,
            high: open.max(close) + jitter,
            low: open.min(close) - jitter,
            close,
            volume: 1000.0 + next() * 500.0,
            funding: None,
            bid: None,
            ask: None,
            symbol: None,
        });
    }
    bars
}

/// Hash one row's collected values, and report how many DISTINCT finite values they took.
///
/// ⚠ NON-DEGENERACY GUARD. An indicator that returns all-NaN (or one constant) for the params it
/// was given hashes the SAME on both platforms and would read as "does not diverge" — a vacuous
/// pass. A row printing `distinct=1` has no verdict; it has a bug in how it was driven. The
/// analytics twin caught a real one this way (`overfit::pbo_cscv`).
fn report(label: String, vals: &[f64]) {
    let mut h = Fnv::new();
    for &v in vals {
        h.push(v);
    }
    let mut seen: Vec<u64> = vals.iter().filter(|v| v.is_finite()).map(|v| v.to_bits()).collect();
    seen.sort_unstable();
    seen.dedup();
    println!("  {label:<32} {}  n={} distinct={}", h.hex(), vals.len(), seen.len());
}

/// Volatility regimes swept beside every seed.
const VOLS: [f64; 3] = [0.004, 0.02, 0.09];

/// The single-series rows. The TRANSCENDENTAL ones each name the converted call site their
/// indicator reaches, below; the `powi` rows that joined afterwards carry their own mapping in the
/// array itself, because one of them turned out to cover nothing:
///
/// - `crates/vike-indicators/src/indicators/momentum.rs`'s `batch_fisher` ->
///   `libm::log((1 + value) / (1 - value))`
/// - `crates/vike-indicators/src/indicators/volatility.rs`'s `batch_hvol` ->
///   `libm::log(v[i] / v[i - 1])`
/// - `crates/vike-indicators/src/indicators/volatility.rs`'s `batch_chop` ->
///   `libm::log10(period)` and `libm::log10(sum_tr / rng)`
/// - `crates/vike-indicators/src/indicators/overlap.rs`'s `batch_alma` ->
///   `libm::exp(-(k - m)^2 / (2 s^2))`
/// - `crates/vike-indicators/src/indicators/statistics.rs`'s `batch_linearreg_angle` ->
///   `libm::atan(b)`
const SINGLE: [(&str, &[f64]); 13] = [
    ("fisher", &[9.0]),
    ("fisher", &[5.0]),
    ("hvol", &[20.0, 365.0]),
    ("hvol", &[5.0, 252.0]),
    ("chop", &[14.0]),
    ("chop", &[7.0]),
    ("alma", &[20.0, 0.85, 6.0]),
    ("alma", &[9.0, 0.5, 3.0]),
    ("linearreg_angle", &[14.0]),
    // The `powi(3)` / `powi(4)` sites #1538 converted to `math.rs`'s `cube` / `quart` — plus one
    // row that is not one of them. This pin's own doc requires "coverage of every converted call
    // site", and the converted sites had been left off the table entirely, so the gate was
    // incomplete against its stated contract from the moment #1538 landed — while that same
    // unmoved table was being quoted as the evidence for indicators it never touched.
    //
    // ⚠ WHICH ROW COVERS WHAT, because #1538 / #1540 / ADR 0032 all got the third one wrong:
    //   `skew`     -> `crates/vike-indicators/src/indicators/statistics.rs`'s `batch_skew` (`cube`)
    //   `kurtosis` -> the same file's `batch_kurtosis` (`quart`)
    //   `mcginley` -> `crates/vike-indicators/src/indicators/overlap.rs`'s `batch_mcginley`, whose
    //                 `period as f64 * quart(ratio)` is the FIFTH converted call site and the one
    //                 the pin missed — `quart`'s other caller, unpinned until this row.
    //   `t3`       -> NOT a converted site at all. `batch_t3` contains no `powi`, no `cube` and no
    //                 `quart`; #1540 added the row believing it was the third conversion. It stays
    //                 because coverage of a real registry indicator is not harmful and churning
    //                 the table to remove it would buy nothing. See the module doc for how "the T3
    //                 ratio" came to be written down three times.
    ("skew", &[20.0]),
    ("kurtosis", &[20.0]),
    ("t3", &[20.0, 0.7]),
    ("mcginley", &[14.0]),
];

/// The pair-seam rows:
///
/// - `crates/vike-indicators/src/pairs.rs`'s `batch_spread` (log = 1) ->
///   `libm::log(ca[i]) - libm::log(cb[i])`
/// - `crates/vike-indicators/src/pairs.rs`'s `batch_correl_log` -> `libm::log(ca[i] / ca[i - 1])`
const PAIRED: [(&str, &[f64]); 2] = [("spread", &[1.0]), ("correl_log", &[20.0])];

/// Drive every row of [`SINGLE`] and [`PAIRED`] over `seeds x VOLS` series of `n` bars, and
/// return each row's collected values in a stable order.
///
/// Shared by the probe and the pin ON PURPOSE: two sweeps free to drift apart would let the gate
/// hold something the measurement no longer describes.
fn sweep_series(seeds: u64, n: usize) -> Vec<(String, Vec<f64>)> {
    let mut hs = vec![Vec::<f64>::new(); SINGLE.len()];
    let mut hp = vec![Vec::<f64>::new(); PAIRED.len()];
    for seed in 0..seeds {
        for &vol in VOLS.iter() {
            let bars = synth_bars(n, seed, vol);
            let b2: Vec<Bar> = synth_bars(n, seed + 7919, vol)
                .iter()
                .map(|b| Bar { close: b.close * 1.3 + 5.0, ..b.clone() })
                .collect();
            for (k, (name, params)) in SINGLE.iter().enumerate() {
                let ind =
                    make_with(name, params).unwrap_or_else(|| panic!("indicator {name} missing"));
                for col in ind.vectorize(&bars) {
                    hs[k].extend(col);
                }
            }
            for (k, (name, params)) in PAIRED.iter().enumerate() {
                let ind =
                    pair_make_with(name, params).unwrap_or_else(|| panic!("pair {name} missing"));
                for col in ind.vectorize_pair(&bars, &b2) {
                    hp[k].extend(col);
                }
            }
        }
    }
    SINGLE
        .iter()
        .zip(hs)
        .map(|((name, params), vals)| (format!("{name}{params:?}"), vals))
        .chain(
            PAIRED
                .iter()
                .zip(hp)
                .map(|((name, params), vals)| (format!("pair::{name}{params:?}"), vals)),
        )
        .collect()
}

/// ALMA's own PARAMETER sweep. Its Gaussian weights depend only on `(period, offset, sigma)` —
/// never on the data — so the fixed parameter sets in [`SINGLE`] exercise a handful of `exp`
/// arguments and prove nothing about the rest of the user-settable grid.
fn sweep_alma(n: usize, periods: &[usize], offsets: &[f64], sigmas: &[f64]) -> Vec<f64> {
    let bars = synth_bars(n, 11, 0.02);
    let mut acc = Vec::<f64>::new();
    for &period in periods {
        for &offset in offsets {
            for &sigma in sigmas {
                let ind = make_with("alma", &[period as f64, offset, sigma]).expect("alma missing");
                for col in ind.vectorize(&bars) {
                    acc.extend(col);
                }
            }
        }
    }
    acc
}

/// Series per regime — 60 x 3 = 180 series per indicator, each 800 bars.
const SEEDS: u64 = 60;

#[test]
#[ignore = "measurement probe — run explicitly on each platform and diff the output"]
fn libm_platform_probe() {
    println!("\n=== vike-indicators — indicators whose value passes through a transcendental ===");
    println!(
        "    ({} series each: {SEEDS} seeds x {} vol regimes x 800 bars)",
        SEEDS as usize * VOLS.len(),
        VOLS.len()
    );
    for (label, vals) in sweep_series(SEEDS, 800) {
        report(label, &vals);
    }
    // The registry's own ranges: period 2..400, offset 0..1 step .05, sigma 1..20 step .5.
    let offsets: Vec<f64> = (0..=20).map(|i| i as f64 * 0.05).collect();
    let sigmas: Vec<f64> = (0..=38).map(|i| 1.0 + i as f64 * 0.5).collect();
    let alma = sweep_alma(600, &[2, 3, 5, 8, 13, 20, 34, 55, 89, 144, 233, 400], &offsets, &sigmas);
    report("alma[PARAM GRID 12x21x39]".to_string(), &alma);
    println!();
}

// ======================= the committed cross-platform gate =======================

/// The pin's sweep is deliberately SMALLER than the probe's above, because the two tests answer
/// different questions.
///
/// The probe HUNTS a divergence that a platform libm produces only occasionally, so it needs
/// enough samples for a rare last-bit disagreement to survive being rounded away — hence 180
/// series. The pin compares against a COMMITTED constant, so a single moved value flips the hash
/// of the first row carrying it; it needs coverage of every converted call site, not statistical
/// reach. This sizing keeps it an ordinary-speed test that runs on every `just t vike-indicators`
/// rather than a slow one somebody eventually marks `#[ignore]`.
const PIN_SEEDS: u64 = 16;
/// Bars per pinned series.
const PIN_BARS: usize = 400;
/// Bars for the pinned ALMA grid. ⚠ Must exceed the largest [`PIN_ALMA_PERIODS`] entry, or that
/// parameter set emits nothing but NaN and pins nothing — `batch_alma` returns early when
/// `n < period`.
const PIN_ALMA_BARS: usize = 600;
/// Pinned ALMA periods, spanning the registry range 2..400.
const PIN_ALMA_PERIODS: [usize; 6] = [2, 3, 8, 34, 144, 400];
/// Pinned ALMA offsets, spanning the registry range 0..1.
const PIN_ALMA_OFFSETS: [f64; 5] = [0.0, 0.25, 0.5, 0.75, 1.0];
/// Pinned ALMA sigmas, spanning the registry range 1..20.
const PIN_ALMA_SIGMAS: [f64; 5] = [1.0, 4.0, 8.0, 12.0, 20.0];

/// ⚠ **ONE set of constants for EVERY platform — that is the entire claim being made here.** A
/// per-platform table (`#[cfg(windows)]` / `#[cfg(unix)]`) would be this gate conceding exactly
/// the thing it exists to deny, so if one is ever proposed the answer is that a conversion
/// regressed and the call site is what needs fixing.
///
/// Recorded by RUNNING, never by hand: a mismatch panics with a paste-ready replacement block.
///
/// ⚠ **Which rows are evidence for WHAT is not uniform, and the correction lives here as well as
/// in the module doc because this table is what gets cited.** Everything through
/// `linearreg_angle`, both `pair::*` rows and the ALMA grid predate #1538: they cover the
/// TRANSCENDENTAL conversion and say nothing whatever about `powi`. #1538 nevertheless offered
/// "this table did not move" as its evidence that `cube` / `quart` moved nothing, and that
/// inference is invalid — none of those twelve rows reaches `skew`, `kurtosis` or `mcginley`, so
/// an unmoved table only ever proved that the already-covered indicators still agreed with
/// themselves. `skew[20.0]` and `kurtosis[20.0]` are the rows carrying the real evidence:
/// RECORDED on Windows/MSVC `dev`, then passing UNCHANGED on Linux/glibc `dev`. `t3[20.0, 0.7]` is
/// coverage of a real indicator but of NO converted site (see [`SINGLE`]).
///
/// ⚠ **`mcginley[14.0]` was recorded 2026-08-25 and carries `quart`'s REAL caller.** It was
/// recorded on Windows/MSVC `dev` — the profile that lowers `llvm.powi` to the CRT's `pow()`, and
/// therefore the only configuration the defect was ever visible in — and the same literal then
/// passed unchanged on Linux/glibc `dev`. Recording it from one box alone would have buried
/// exactly the disagreement this pin exists to surface, so both runs are the evidence, not one.
///
/// The `t3` row above it is coverage of a real indicator but of NO converted site: #1540 added it
/// believing `batch_t3` held the fifth `powi`, and it holds none. It stays because extra coverage
/// costs nothing, not because it proves anything about `quart`.
///
/// # ⚠ A row may be written `"RECORD"`, and that is a PLACEHOLDER which fails by construction
///
/// Adopted 2026-08-26 from the `vike-analytics` twin
/// (`crates/vike-analytics/tests/libm_platform_probe.rs`'s `PLATFORM_INVARIANT`), which has used
/// it since its two `stats` rows were added on a box that could not run cargo. The convention
/// exists because that situation recurs — a row is often written by whoever adds the call site,
/// and a plausible-looking hex literal invented there is INDISTINGUISHABLE from a recorded one
/// while pinning nothing at all.
///
/// `"RECORD"` cannot match any output of [`Fnv::hex`], which is `format!("{:016x}", ..)` — exactly
/// sixteen characters drawn from `0-9a-f`. `"RECORD"` is six characters and four of them are
/// outside that alphabet, so it fails on length and on content independently. That is the whole
/// mechanism: the gate stays red until a human runs this file on Windows AND on Linux, confirms
/// the two runs agree bit-for-bit, and pastes the agreed hash in.
///
/// The point of spelling it as a WORD rather than a wrong number is what a reader concludes from a
/// red row. `"RECORD"` reads as an UNFINISHED RECORDING — nobody has measured this yet — while a
/// hex literal that does not match reads as a REGRESSION: an indicator's value moved and something
/// is broken. Those call for opposite responses, and a made-up literal makes the second one
/// impossible to tell from the first forever after.
///
/// ⚠ **No placeholder remains in the table below**, and one comment there had gone stale saying
/// otherwise: the `mcginley[14.0]` row carried a "NOT YET RECORDED" note directly above a
/// recorded hash, contradicting the paragraph three lines up that records both of its runs. The
/// note is now the history it actually is.
const PINNED: [(&str, &str); 16] = [
    ("fisher[9.0]", "d0cc148f0aa461bf"),
    ("fisher[5.0]", "116e342b6de05da9"),
    ("hvol[20.0, 365.0]", "3a6baf181d4ac64c"),
    ("hvol[5.0, 252.0]", "b335277d1983fd19"),
    ("chop[14.0]", "19e2b38f5f05847a"),
    ("chop[7.0]", "2e77aac0b142d95a"),
    ("alma[20.0, 0.85, 6.0]", "e230136f906dc361"),
    ("alma[9.0, 0.5, 3.0]", "9cdc175457f0ddb0"),
    ("linearreg_angle[14.0]", "0201fa1056dc882f"),
    ("skew[20.0]", "7d120fd9d6426776"),
    ("kurtosis[20.0]", "ea8a211abf37c98e"),
    ("t3[20.0, 0.7]", "43f2f6d1938299bb"),
    // `quart`'s second caller, and the site #1540 believed `t3` covered. ⚠ This comment read "NOT
    // YET RECORDED" while the literal beside it was already recorded — the row went in as a
    // `"RECORD"` placeholder, was measured on Windows/MSVC `dev` and confirmed on Linux/glibc
    // `dev`, and the note simply did not follow. Kept, corrected, because a stale "not yet"
    // beside a real hash is worse than no note: it invites the next author to overwrite a
    // measured value believing nothing was there. See this table's doc for the convention.
    ("mcginley[14.0]", "e4daa2e16096a7e2"),
    ("pair::spread[1.0]", "765f4800e025d514"),
    ("pair::correl_log[20.0]", "ab5f66ecedec5348"),
    ("alma[PIN GRID 6x5x5]", "1bc045140e2ff6bb"),
];

/// Smallest number of DISTINCT finite values a pinned row may carry before its pin is treated as
/// vacuous. See [`report`]'s `distinct` column for the failure this guards: an all-NaN or
/// one-constant row hashes identically on every platform and would read as a pass while proving
/// nothing at all. The analytics twin caught a real instance of that (`overfit::pbo_cscv`).
const MIN_DISTINCT: usize = 64;

#[test]
fn libm_cross_platform_pin() {
    let mut rows = sweep_series(PIN_SEEDS, PIN_BARS);
    rows.push((
        "alma[PIN GRID 6x5x5]".to_string(),
        sweep_alma(PIN_ALMA_BARS, &PIN_ALMA_PERIODS, &PIN_ALMA_OFFSETS, &PIN_ALMA_SIGMAS),
    ));
    assert_eq!(rows.len(), PINNED.len(), "row count changed — PINNED must be re-recorded");

    let mut actual: Vec<(String, String)> = Vec::with_capacity(rows.len());
    let mut bad: Vec<String> = Vec::new();
    for ((label, vals), (want_label, want_hash)) in rows.iter().zip(PINNED.iter()) {
        assert_eq!(label, want_label, "row order changed — PINNED must be re-recorded");

        let mut h = Fnv::new();
        for &v in vals {
            h.push(v);
        }
        let got = h.hex();
        actual.push((label.clone(), got.clone()));

        let mut seen: Vec<u64> =
            vals.iter().filter(|v| v.is_finite()).map(|v| v.to_bits()).collect();
        seen.sort_unstable();
        seen.dedup();
        if seen.len() < MIN_DISTINCT {
            bad.push(format!(
                "{label}: only {} distinct finite values (need >= {MIN_DISTINCT}) — the row is \
                 degenerate, so its pin proves nothing",
                seen.len()
            ));
        }
        if got != *want_hash {
            bad.push(format!("{label}: pinned {want_hash}, computed {got}"));
        }
    }

    if !bad.is_empty() {
        let paste: String =
            actual.iter().map(|(l, h)| format!("    (\"{l}\", \"{h}\"),\n")).collect();
        panic!(
            "\ncross-platform pin broke:\n  {}\n\n\
             There is no tolerance here to widen, and hand-editing a digit defeats the gate.\n\
             A moved hash means an indicator's VALUE moved — most likely a call site reverted to\n\
             the platform's `f64::ln`/`log10`/`exp`/`atan`, or the `libm` crate was upgraded.\n\
             Establish WHY first; only then re-record by pasting this over `PINNED`, quoting the\n\
             old and new hashes in the commit message.\n\n\
             ⚠ A row whose PINNED side reads RECORD is not a regression at all — it is the\n\
             placeholder convention (see `PINNED`'s doc): the row was written by somebody who\n\
             could not run this test, and it stays red until a human has run it on Windows AND on\n\
             Linux and confirmed the two agree. Paste the AGREED hash; never one box's.\n\n\
             const PINNED: [(&str, &str); {}] = [\n{paste}];\n",
            bad.join("\n  "),
            actual.len(),
        );
    }
}

/// The `f64` methods whose last bit is the PLATFORM's business rather than IEEE 754's, as bare
/// names. [`banned_needles`] renders each into the spellings that reach it.
///
/// ⚠ **The inverse-trig and hyperbolic family was MISSING from this list**, and the omission was
/// live rather than theoretical: `crates/vike-indicators/src/indicators/statistics.rs`'s
/// `batch_linearreg_angle` calls `libm::atan(b)` in a file that carries no `#[cfg(test)]` marker at
/// all — every line of it is production and every line of it was scanned — so reverting that one
/// call to `b.atan()` passed this gate cleanly, and the pinned `linearreg_angle` row would then
/// have had to disagree across platforms before anything went red. `tan` cannot stand in for
/// `atan`: [`banned_needles`] anchors a `(` immediately after the name and a `.` or `::`
/// immediately before it, so `.tan(` matches neither `.atan(` nor `.tanh(`, and `atan2` needs its
/// own entry rather than riding on `atan`.
///
/// ⚠ **WIDENED 2026-08-26 from 22 names to 26** — `asinh`, `acosh`, `atanh`, `sin_cos`. Not one of
/// the four was reachable from any of the original 22 by substring, which is the only way this
/// list has ever gained coverage, and the claim was checked needle by needle rather than assumed:
/// `.asin(` misses `.asinh(` because an `h` sits where the paren must be, `.sinh(` misses
/// `.asinh(` because the character before `sinh` there is an `a` rather than a dot, and
/// `.sin_cos(` escapes `.sin(` by the paren rule and `.cos(` by the dot rule at once. None of the
/// four appears anywhere under this crate's `src/`, so the widening changed no verdict — it
/// removed a gap the next edit could have walked into.
///
/// ⚠ `to_degrees` / `to_radians` are deliberately NOT here and should not be added: std lowers
/// each to a single multiply by a constant, so they are ordinary arithmetic rather than a libcall
/// and this gate has no business with them. There IS a separate reason `batch_linearreg_angle`
/// must not use one — `crates/vike-indicators/src/indicators/statistics.rs`'s module doc records
/// that Rust's precomputed constant can differ by a ULP from CPython's `math.degrees`, so the port
/// spells `atan(b) * (180.0 / PI)` by hand — but that is oracle parity, a different contract with
/// a different owner, and folding it in here would make this list mean two things at once.
const BANNED_FNS: [&str; 26] = [
    "powf", "ln", "log", "log2", "log10", "exp", "exp2", "exp_m1", "ln_1p", "sin", "cos", "tan",
    "asin", "acos", "atan", "atan2", "sinh", "cosh", "tanh", "cbrt", "hypot", "powi", "asinh",
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
///
/// ⚠ The `powi` entry is what `crates/vike-indicators/tests/parity.rs`'s `self_multiplies` leans
/// on when it declares a square-through-`powi` unable to come back silently. That claim now covers
/// both spellings, which it did not before this function existed.
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
/// available.** None of the ten files under `crates/vike-indicators/src` carrying a `#[cfg(test)]`
/// puts it anywhere but last, so a tree-shaped assertion — "some production line below a marker
/// was scanned" — would be FALSE today and could only be written by weakening it into something
/// that proves nothing. The mechanism is held here instead: production before a test module, a
/// banned call inside it, production after it, and a banned call in each production half. It also
/// covers the `;` header form, which no brace-matching walk would handle by accident.
#[test]
fn the_test_module_cut_excludes_only_the_test_module() {
    // Every fixture row is a string literal, so the needles inside them are data rather than
    // calls; this file lives under `tests/` and the gate below scans `src/` only.
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
    // The control arm, and the one that matters most in THIS crate: the module doc on fixture
    // line 1 is the house-style header `crates/vike-mm/src/xemm/pricing.rs` and three siblings
    // already open with, naming the attribute in backticks, and that convention could arrive here
    // any day. If prose could open a range, `items[0]` would start at 0 and every assertion here
    // would still pass while the gate scanned nothing at all.
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

/// Production code must reach a transcendental through the `libm` CRATE, never through `f64`'s
/// platform-libm methods — and must not reach a power through `powi` at all.
///
/// The hash gate above proves today's outputs agree; this one stops the next edit from
/// reintroducing a platform call somewhere the corpus happens not to reach. `sqrt` is deliberately
/// absent from the banned set: IEEE 754 requires it to be correctly rounded, so it is already
/// platform-invariant.
///
/// ⚠ **This crate had no source gate at all until this test.** Every `.powi(2)` routed through
/// `crates/vike-indicators/src/math.rs`'s `sq` by convention only, which is precisely the state
/// that lets the next edit undo the fix without anything going red — and the `vike-analytics` twin
/// had already demonstrated the shape.
///
/// ⚠ **It RECURSES, and that is load-bearing rather than tidiness.** The twin uses a flat
/// `read_dir`, which is right for a crate whose sources are all top-level. This crate keeps its
/// real math under `src/indicators/`, so a flat scan would walk `math.rs` and `lib.rs`, report a
/// confident zero, and never open `statistics.rs` or `overlap.rs` — the two files whose `powi(3)`
/// and `powi(4)` this gate exists to hold converted. A gate that cannot see its own subject is
/// worse than no gate, because it certifies.
///
/// The banned set is [`BANNED_FNS`] rendered through [`banned_needles`], which is where the
/// argument for every name lives — including the two SPELLINGS each name gets. Until 2026-08-26
/// only the method form was banned, so a fully-qualified `f64::exp(x)` matched nothing.
///
/// Scope: each source file MINUS its `#[cfg(test)]` item RANGES. Test code inside one builds
/// fixtures with whatever is convenient and is not shipped — measured, not assumed: every
/// `.sin(`/`.cos(` call in this crate is a fixture generator inside `pairs.rs`'s or `rolling.rs`'s
/// trailing test module, which is why the full transcendental list could be adopted here at zero
/// cost rather than carved down.
///
/// ⚠ **RANGES as of 2026-08-26, and a `break` at the first marker is not the same rule.** A
/// `break` says "the shipped half of a file ends at its first test module", which holds only while
/// the test module is LAST; for a file with one in the MIDDLE it discards every production line
/// below it and still reports a confident zero. Measured: of the ten files under
/// `crates/vike-indicators/src` carrying a marker, not one declares anything after it, so this
/// change moves NOTHING here and no verdict differs. It is a trap removed, not a finding — and the
/// trap is real one crate over: `crates/vike-mm/src/platform_probe.rs`'s `cfg_test_ranges` records
/// 1,612 production lines the same `break` was discarding there, two live `libm` sites among them,
/// and a module doc that had copied the resulting undercount into prose.
/// [`the_test_module_cut_excludes_only_the_test_module`] is the non-vacuity arm, and it is a
/// FIXTURE rather than a tree measurement precisely because this tree cannot exercise the case.
///
/// ⚠ **That cut is taken LINE BY LINE, and it used to be taken over the whole file text with one
/// `find`.** The comment skip in the loop below runs on the lines that SURVIVE the cut, so it
/// structurally could not rescue a truncation a COMMENT had caused: a single `//!` line naming
/// `#[cfg(test)]` near the top of a file silently reduced that file's scanned region to its
/// header, and nothing anywhere went red — the gate would still have reported a confident,
/// meaningless zero. Fixing it moved NOTHING on today's tree, and that is the point: it is a trap
/// removed, not a finding. No file under either gated `src/` tree names the attribute in prose
/// (`git grep -n '#\[cfg(test)\]' -- crates/vike-indicators/src crates/vike-analytics/src` is the
/// check — every hit is the attribute itself), but the tripping line already exists elsewhere in
/// the workspace: `crates/vike-mm/src/xemm/pricing.rs` and three of its siblings open with a
/// "House style: naïve f64 folds …, in-file `#[cfg(test)]`" header. Adopting that convention in a
/// gated crate would have disarmed that file's scan for free.
///
/// ⚠ A marker is recognised by `starts_with`, never by equality: `#[cfg(test)] mod tests {`
/// written on one line is a spelling this workspace already uses, and an equality test would walk
/// straight past it into the test module and start reporting fixture code as production.
#[test]
fn production_code_calls_libm_not_the_platform() {
    // The banned set, in both spellings, lives on `BANNED_FNS` / `banned_needles` — moved out of
    // this function body so the argument for each name sits with the name rather than in a comment
    // the next widening has to be threaded through.
    let banned = banned_needles();
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut found = Vec::new();
    let mut scanned = Vec::new();
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
            let lines: Vec<&str> = text.lines().collect();
            let test_items = cfg_test_ranges(&lines);
            for &(start, _, closed) in &test_items {
                if !closed {
                    unclosed.push(format!("  {name}:{}", start + 1));
                }
            }
            for (i, line) in lines.iter().enumerate() {
                if test_items.iter().any(|&(start, end, _)| i >= start && i < end) {
                    // Inside a `#[cfg(test)]` item — not shipped. ⚠ This used to `break` at the
                    // first marker, which discards production code placed BELOW a test module.
                    // No file in this crate does that today, so the change moves nothing here;
                    // `crates/vike-mm/src/platform_probe.rs`'s twin is where the same `break` was
                    // hiding two live call sites, and `cfg_test_ranges` carries that measurement.
                    continue;
                }
                let code = line.trim_start();
                if code.starts_with("//") {
                    // A doc comment naming `powi` is prose, not a call — and this skip must come
                    // BEFORE the range test is even reachable for a marker line, which is why
                    // `cfg_test_ranges` applies the same filter first: a comment mentioning
                    // `#[cfg(test)]` must not be able to open a range over a file whose test
                    // module has not started yet.
                    continue;
                }
                for pat in &banned {
                    if line.contains(pat.as_str()) {
                        found.push(format!("  {name}:{} {}", i + 1, line.trim()));
                    }
                }
            }
        }
    }
    // Non-vacuity, and specifically the RECURSION's non-vacuity: naming the two nested files the
    // gate exists for means a scan that silently stopped at the top level fails here rather than
    // passing with an empty `found`.
    for must in ["statistics.rs", "overlap.rs", "math.rs"] {
        assert!(
            scanned.iter().any(|s| s == must),
            "the scan never opened {must} — it is not walking src/indicators/, so an empty \
             result proves nothing. Scanned: {scanned:?}"
        );
    }
    assert!(
        unclosed.is_empty(),
        "a `#[cfg(test)]` item's closing line could not be located, so everything below it went \
         UNSCANNED — the exact hole the old `break` left, wearing a different cause. See \
         `cfg_test_ranges`: the end is the item's own indentation followed by a lone `}}`, or a \
         `;` for a `mod NAME;` declaration.\n{}",
        unclosed.join("\n")
    );
    assert!(
        found.is_empty(),
        "production code must call the `libm` CRATE rather than `f64`'s platform-libm methods, \
         and must spell a power as `math.rs`'s `sq` / `cube` / `quart` rather than `powi` — so a \
         value does not depend on which box, or which optimisation level, ran it. BOTH spellings \
         are banned, `x.exp()` and `f64::exp(x)`: they are the same inherent method and the same \
         libcall, and a list naming only the first is a list with a hole in it (see \
         `banned_needles`):\n{}",
        found.join("\n")
    );
}
