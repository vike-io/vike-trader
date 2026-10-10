//! MEASUREMENT probe **and** the committed cross-platform GATE for the WIRE QUANTIZERS — every
//! place in this workspace where an `f64` crosses into `rust_decimal` and comes back out as the
//! string that is SENT to a venue or SIGNED into an order hash.
//!
//! It is the non-`f64` member of the platform-probe family
//! (`crates/vike-indicators/tests/libm_platform_probe.rs`,
//! `crates/vike-analytics/tests/libm_platform_probe.rs`, `crates/vike-mm/src/platform_probe.rs`),
//! whose verdict is
//! `docs/decisions/0032-transcendentals-come-from-the-libm-crate-not-the-platform.md` and is not
//! re-derived here. This file measures the boundary where a last-bit difference stops being a last
//! bit.
//!
//! ```text
//! <cargo> test -p vike-bridge-core --test wire_quantizer_probe -- --ignored --nocapture
//! ```
//!
//! # Scope
//!
//! * **`f32` transcendentals: none in production, so no probe.** No source line carries both an
//!   `f32` and a transcendental call; the `f32` sites are egui pixel space, an exact wire decode
//!   (`crates/bridges/dukascopy/src/data.rs`'s `decode_ticks`) and LightGBM's label width (hashed
//!   and widened, never computed on). ⚠ A probe over a vacuous corpus hashes the same on every
//!   platform and reads as a pass while proving nothing, so the category is declared empty instead.
//! * **Integer-rounding cliffs elsewhere** — `crates/vike-orderflow/src/bar_agg.rs`'s
//!   `nice_orderflow_tick`, `crates/vike-mm/src/avellaneda.rs`'s `effective_blackout_ms`,
//!   `crates/vike-chart/src/scale.rs`'s `log_nice_values` — are known and not re-reported here.
//! * **The wire quantizers** are the subject: the highest-stakes instance of the cliff shape,
//!   because the integer they round to is a TICK or a LOT on a live order. [`rows`] is the roster
//!   this file drives; [`WIRE_FILES`] is the roster the source gate walks.
//!
//! | site | quantizer | one ULP upstream becomes |
//! |---|---|---|
//! | [`format_to_step`] / [`format_to_step_f`] | `(value / step).trunc()` | a whole TICK or LOT |
//! | [`format_scaled_to_step_f`] | the same `trunc`, after an exact rational rescale | a whole step |
//! | `crates/bridges/okx/src/perp.rs`'s `to_contracts` | `(raw / ct / step).trunc()` | a whole CONTRACT step |
//! | `crates/bridges/hyperliquid/src/px.rs`'s `clamp_price` | `MidpointAwayFromZero` | a whole tick, in the SIGNED action hash |
//! | `crates/bridges/hyperliquid/src/px.rs`'s `round_size` | `ToZero` | a whole lot |
//! | `crates/bridges/polymarket/src/exec_plane/order.rs`'s `to_base_units` | `(x * 1e6).round()` | one micro-unit, in the SIGNED EIP-712 order |
//!
//! A transcendental 1 ulp apart on two boxes yields two prices differing in the 16th digit —
//! invisible. Push them through a quantizer whose decision is a TRUNCATION or an exact-midpoint
//! comparison and the two boxes emit different WIRE STRINGS: a different limit price, a different
//! size, and for the two EIP-712 venues a different signature over a different order. The cliff
//! rows below measure how often that happens and how far it moves.
//!
//! # ⚠ What the PIN half of this file does and does not claim
//!
//! **None of these functions calls a platform transcendental**, and that is derived, not assumed:
//! [`the_wire_quantizers_reach_decimal_through_a_string`] scans [`WIRE_FILES`] for every
//! platform-libm spelling and for `Decimal::from_f64`. The whole path is Rust's own shortest-
//! round-trip float formatter plus `rust_decimal`'s integer arithmetic. The one libm SPELLING on a
//! quantizer's path is `crates/bridges/hyperliquid/src/instruments.rs`'s `pow10_neg` (a
//! `10f64.powi(..)` that `round_size`'s grid witness calls): exempt for the reason [`POW10_EXEMPT`]
//! records, and DERIVED rather than trusted, because [`REACHED_FILES`] puts that file under the
//! same banned-spelling scan.
//!
//! So these outputs are platform-invariant BY CONSTRUCTION, and the committed table is a
//! REGRESSION TRIPWIRE, not the closing of a live divergence (a set of agreeing numbers looks the
//! same whether it was always true or was made true). What it is FOR is the one edit that would
//! quietly break it: swapping `Decimal::from_str(&py_f64_str(x))` for `Decimal::from_f64(x)`
//! ([`BANNED_SPELLINGS`] carries why).
//!
//! # What FEEDS these quantizers
//!
//! 1. a strategy computes a price — for the maker `crates/vike-mm/src/avellaneda.rs`'s
//!    `as_reservation_price` and `as_optimal_half_spread`, whose logarithms are `libm`;
//! 2. `crates/vike-mm/src/avellaneda.rs`'s `snap_to_tick` snaps it, delegating to
//!    `crates/vike-model/src/scalar.rs`'s `round_to_step`;
//! 3. `crates/vike-exec/src/risk.rs`'s `RiskGate` re-rounds through `round_to`;
//! 4. the venue adapter formats it — `format_to_step_f` for binance/bybit/okx/aster, `clamp_price`
//!    / `round_size` for hyperliquid.
//!
//! Steps 2 and 3 are `(value / step).round_ties_even() * step`, both halves correctly rounded under
//! IEEE 754 and so platform-invariant; step 1 is not by nature, which is why it was converted. This
//! file makes a reversion cost a visible tick instead of an invisible bit.
//!
//! The chain has a second, platform-INDEPENDENT hazard, measured by the `steps lost` row:
//! `round_to_step` ends in a MULTIPLICATION, and `n * step` can land one ULP BELOW the decimal grid
//! point, where `format_to_step`'s truncation costs a full step. Without
//! `crates/vike-bridge-core/src/format.rs`'s `grid_multiple_image` that row read 150 of 4096; it
//! records `0/4096` now and is a regression detector ([`MIN_DISTINCT_RESULT`] argues its floor).
//!
//! # Declared coverage, and the gaps declared as gaps
//!
//! [`rows`] covers `vike_bridge_core::format`'s three public quantizers plus `py_f64_str` (the
//! shim every one of them goes through), and hyperliquid's three. Deribit's combo path
//! (`crates/bridges/deribit/src/combo.rs`'s `build_combo_order_params`) quantizes through
//! `format_scaled_to_step_f`, which is that row. Two sites are covered by the SOURCE GATE and not
//! by the hash pin:
//!
//! * `crates/bridges/okx/src/perp.rs`'s `to_contracts` is a METHOD on a stateful client;
//!   `crates/bridges/okx/tests/offline/r6_okx_parity.rs` compares it bit-for-bit against the frozen
//!   `fixtures/r6` export instead (stronger for the values that fixture carries, weaker for the
//!   rest).
//! * `crates/bridges/polymarket/src/exec_plane/order.rs`'s `to_base_units` is private and its
//!   public caller sits behind vike-polymarket's `polymarket` feature, which this crate does not
//!   take. Its cliff is the sharpest in the table (a `.round()` at an exact half, inside the signed
//!   order) and it deserves a probe in ITS crate.
//!
//! # ⚠ The corpus is PURE ARITHMETIC, and that is load-bearing
//!
//! Inputs come from an LCG and `+ - * /` only, never from `.sin()`/`.cos()`. Those are themselves
//! platform-dependent, so generating inputs with them would make every column diverge because of
//! the INPUTS rather than the function under test. [`corpus_value`] is the CONTROL row: a table
//! where every other row moved but that one did not is a function regression, and a table where
//! that row moved too is a broken corpus.
//!
//! # ⚠ A pinned constant reading `"RECORD"` is a PLACEHOLDER which fails by construction
//!
//! The convention is the libm probes': `"RECORD"` cannot match any output of [`Fnv::hex`]
//! (`format!("{:016x}", ..)`, sixteen characters from `0-9a-f`), so it fails on length and on
//! content independently. A red row spelled `"RECORD"` is an UNFINISHED RECORDING, not a
//! regression. Every re-record obeys one rule: a human runs it on Windows/MSVC `dev` AND on
//! Linux/glibc `dev`, confirms the two runs agree bit-for-bit, and pastes the AGREED hashes, never
//! one box's. A plausible-looking hex literal invented instead would pin nothing at all.

use std::path::{Path, PathBuf};

use vike_bridge_core::format::{
    format_scaled_to_step_f, format_to_step, format_to_step_f, py_f64_str,
};
use vike_hyperliquid::consts::PERP_MAX_DECIMALS;
use vike_hyperliquid::px::{clamp_price, float_to_wire, round_size};

// ================================ hashing and the corpus =========================================

/// FNV-1a, the sibling probes' hasher, over BYTES rather than `f64::to_bits` because every row
/// here produces a STRING. It needs no NaN canonicalisation but does need a SEPARATOR, so that
/// `["ab", "c"]` cannot hash equal to `["a", "bc"]` and a row's boundaries cannot move unnoticed.
struct Fnv(u64);

impl Fnv {
    fn new() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }
    fn byte(&mut self, b: u8) {
        self.0 ^= u64::from(b);
        self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
    }
    fn push(&mut self, s: &str) {
        for &b in s.as_bytes() {
            self.byte(b);
        }
        self.byte(0); // the separator — see the type's doc
    }
    fn hex(&self) -> String {
        format!("{:016x}", self.0)
    }
}

/// Deterministic pseudo-random `f64` in `(0, 1)` from a pure-integer LCG.
///
/// ⚠ No transcendental in here (the module doc says why): the `as f64` and the single division are
/// exact or correctly rounded, so the corpus is bit-identical on every platform by construction.
fn lcg(seed: &mut u64) -> f64 {
    *seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
    // Top 53 bits over 2^53: an exact ratio of integers, then one correctly-rounded divide.
    ((*seed >> 11) as f64) / ((1u64 << 53) as f64)
}

/// The magnitudes a real price or size occupies, from a 1e-6 prediction-market probability to a
/// 1e4 BTC price. Decimal LITERALS: the compiler's own conversion fixes each one identically on
/// every platform.
const DECADES: [f64; 11] = [1e-6, 1e-5, 1e-4, 1e-3, 1e-2, 1e-1, 1.0, 1e1, 1e2, 1e3, 1e4];

/// A corpus value: a mantissa in `[1, 10)` times one of [`DECADES`]. Deliberately NOT uniform over
/// one range: every quantizer here is scale-sensitive, so a one-decade corpus would exercise one
/// branch of each and report a confident hash over it.
fn corpus_value(seed: &mut u64) -> f64 {
    let mantissa = 1.0 + lcg(seed) * 9.0;
    let idx = ((lcg(seed) * DECADES.len() as f64) as usize).min(DECADES.len() - 1);
    mantissa * DECADES[idx]
}

/// The value's bit pattern as hex — how the CONTROL row renders, so its pin is bit-level rather
/// than digit-level.
fn bits(v: f64) -> String {
    format!("{:016x}", v.to_bits())
}

/// The real venue tick/lot grids, transcribed from
/// `crates/vike-bridge-core/tests/format_props.rs`'s `step_strategy`: every one is a TERMINATING
/// decimal, so a moved output is a genuine wire change rather than a rounding artifact. Both
/// spellings, because [`format_to_step`] takes the STRING and [`format_to_step_f`] routes the `f64`
/// through [`py_f64_str`]; pinning only one would leave the other's `str(float)` shim uncovered.
/// ⚠ It stops at 1e-8 while live grids go to 1e-12: a declared residual, see [`PINNED`].
const STEPS: [(&str, f64); 18] = [
    ("1", 1.0),
    ("10", 10.0),
    ("100", 100.0),
    ("0.1", 0.1),
    ("0.01", 0.01),
    ("0.001", 0.001),
    ("0.0001", 0.0001),
    ("0.00001", 0.00001),
    ("0.000001", 0.000001),
    ("0.0000001", 0.0000001),
    ("0.00000001", 0.00000001),
    ("0.5", 0.5),
    ("0.25", 0.25),
    ("0.05", 0.05),
    ("2.5", 2.5),
    ("5", 5.0),
    ("0.0005", 0.0005),
    ("0.025", 0.025),
];

/// The venue rescale ratios [`format_scaled_to_step_f`] exists for — Deribit's reduced combo
/// ratios. `(1, 1)` is included because it takes the SHORT-CIRCUIT arm (straight to
/// [`format_to_step_f`]) and a pin that never drove that arm would not notice it changing;
/// `(-1, 3)` carries the credit-combo sign; `(5, 7)` is a non-terminating expansion, so the
/// Decimal division truncates at `rust_decimal`'s own precision rather than landing exact.
const RATIOS: [(i64, i64); 6] = [(1, 1), (1, 3), (3, 1), (2, 3), (-1, 3), (5, 7)];

/// Samples per row: large enough that a rare disagreement survives into the hash.
const SAMPLES: usize = 4096;

/// The largest whole-step multiple a grid point is built from, so `n` stays an exact integer count
/// and the `steps lost` arithmetic is a small-integer subtraction.
const GRID_MAX_STEPS: f64 = 1_000_000.0;

// ================================ the ULP and cliff machinery ====================================

/// The next representable `f64` above `x`, for a POSITIVE finite `x`, spelled as the bit increment
/// that DEFINES one ULP. Non-positive or non-finite input passes through unchanged (the corpus is
/// strictly positive, and an identity beats a panic inside a measurement).
fn ulp_up(x: f64) -> f64 {
    if !x.is_finite() || x <= 0.0 {
        return x;
    }
    f64::from_bits(x.to_bits() + 1)
}

/// The next representable `f64` BELOW `x`, for a positive finite `x` above the smallest normal.
/// See [`ulp_up`] for why this is spelled as a bit decrement.
fn ulp_down(x: f64) -> f64 {
    if !x.is_finite() || x <= f64::MIN_POSITIVE {
        return x;
    }
    f64::from_bits(x.to_bits() - 1)
}

/// `10^-n` — the width of the `n`-th decimal place, i.e. the tick a decimal-place quantizer works
/// in. ⚠ A `powi` in a platform probe, deliberately: bit-identical across platforms for every `n`
/// this file uses, for the reason [`POW10_EXEMPT`] records.
fn ten_pow_neg(n: u32) -> f64 {
    10f64.powi(-(n as i32))
}

/// A production-shaped grid point: `(n, round_to_step(n · step, step))`.
///
/// The value shape the live path hands a quantizer: `round_to_step` is the snap both `snap_to_tick`
/// and the `RiskGate`'s `round_to` reach, and its multiplication is where the error enters.
fn grid_point(seed: &mut u64, step: f64) -> (f64, f64) {
    let n = (lcg(seed) * GRID_MAX_STEPS).floor() + 1.0;
    (n, vike_model::round_to_step(n * step, step))
}

/// How far a quantizer's output moved, in whole `unit`s, rendered as a signed integer.
///
/// `unit` is the tick the quantizer works in — the step for [`format_to_step_f`], `10^-decimals`
/// for the hyperliquid pair. `f64` arithmetic is adequate BECAUSE the answer is a small integer and
/// the `.round()` collapses any residue. `"?"` (un-parseable output) is a distinct rendered value,
/// so a row that started producing it moves its hash rather than being absorbed into `+0`.
fn displacement(before: &str, after: &str, unit: f64) -> String {
    let (Ok(a), Ok(b)) = (before.parse::<f64>(), after.parse::<f64>()) else {
        return "?".to_string();
    };
    if !unit.is_finite() || unit <= 0.0 {
        return "?".to_string();
    }
    let moved = ((b - a) / unit).round();
    if !moved.is_finite() {
        return "?".to_string();
    }
    let n = moved as i64;
    format!("{n:+}")
}

// ================================ the rows ======================================================

/// One measured row: its rendered values, and the smallest number of DISTINCT ones its verdict is
/// worth anything with.
struct Row {
    label: &'static str,
    values: Vec<String>,
    /// ⚠ NON-DEGENERACY GUARD, PER ROW: a quantizer row emits thousands of distinct strings, a
    /// cliff row a handful of small integers, and one threshold low enough for the second would
    /// stop guarding the first.
    min_distinct: usize,
}

/// The floor for a QUANTIZER row (4096 corpus values through a formatter): anything near 1 means
/// the corpus collapsed and the row's cross-platform agreement would be vacuous.
const MIN_DISTINCT_QUANTIZER: usize = 256;

/// The floor for a CLIFF row that perturbs by one ULP: some samples moved and some did not.
///
/// ⚠ A red here is a CORPUS finding, not a regression: the perturbation never (or always) crossed
/// a quantization boundary. Widen or re-aim the corpus; never lower this number, the only thing
/// standing between these rows and a hash that agrees across platforms because it is constant.
const MIN_DISTINCT_CLIFF: usize = 2;

/// The floor for a RESULT row, which asks whether the ordinary chain drops a whole step on a value
/// already on the grid. An all-`+0` column is a RESULT (the chain is clean on this corpus), and
/// demanding two outcomes would turn good news red.
const MIN_DISTINCT_RESULT: usize = 1;

/// Every covered quantizer and every cliff, in a FIXED order: [`PINNED`] is compared positionally,
/// so a reordering is a re-record. Shared by the printing probe and the gate ON PURPOSE, so the
/// gate can never hold something the measurement no longer describes.
fn rows() -> Vec<Row> {
    let mut out: Vec<Row> = Vec::new();

    // --- the CONTROL: the corpus generator's own output ------------------------------------------
    {
        let mut s = 0x1234_5678_9abc_def0u64;
        let values: Vec<String> = (0..SAMPLES).map(|_| bits(corpus_value(&mut s))).collect();
        out.push(Row {
            label: "corpus (LCG bits — CONTROL)",
            values,
            min_distinct: MIN_DISTINCT_QUANTIZER,
        });
    }

    // --- format::py_f64_str — the `str(float)` shim every other bridge-core row goes through -----
    {
        let mut s = 0x0fed_cba9_8765_4321u64;
        let values: Vec<String> = (0..SAMPLES).map(|_| py_f64_str(corpus_value(&mut s))).collect();
        out.push(Row { label: "format::py_f64_str", values, min_distinct: MIN_DISTINCT_QUANTIZER });
    }

    // --- format::format_to_step — the STRING-step entry point ------------------------------------
    // A third of the corpus is negated: a positive-only corpus never reaches `quantize_to_step`'s
    // NEGATIVE-ZERO arm.
    {
        let mut s = 0xa5a5_5a5a_c3c3_3c3cu64;
        let values: Vec<String> = (0..SAMPLES)
            .map(|i| {
                let v = corpus_value(&mut s);
                let v = if i % 3 == 0 { -v } else { v };
                format_to_step(v, STEPS[i % STEPS.len()].0)
            })
            .collect();
        out.push(Row {
            label: "format::format_to_step (string steps)",
            values,
            min_distinct: MIN_DISTINCT_QUANTIZER,
        });
    }

    // --- format::format_to_step_f — the f64-step entry point -------------------------------------
    {
        let mut s = 0x3c3c_c3c3_5a5a_a5a5u64;
        let values: Vec<String> = (0..SAMPLES)
            .map(|i| {
                let v = corpus_value(&mut s);
                let v = if i % 3 == 0 { -v } else { v };
                format_to_step_f(v, STEPS[i % STEPS.len()].1)
            })
            .collect();
        out.push(Row {
            label: "format::format_to_step_f (f64 steps)",
            values,
            min_distinct: MIN_DISTINCT_QUANTIZER,
        });
    }

    // --- format::format_scaled_to_step_f — the exact-rational sibling ----------------------------
    // The only quantizer here with NO frozen-fixture pin of its own (`fixtures/r6/format.json`
    // predates it), and the one Deribit's combo path quantizes amount AND price through.
    {
        let mut s = 0x9e37_79b9_7f4a_7c15u64;
        let values: Vec<String> = (0..SAMPLES)
            .map(|i| {
                let v = corpus_value(&mut s);
                let (num, den) = RATIOS[i % RATIOS.len()];
                format_scaled_to_step_f(v, num, den, STEPS[i % STEPS.len()].1)
            })
            .collect();
        out.push(Row {
            label: "format::format_scaled_to_step_f (rationals)",
            values,
            min_distinct: MIN_DISTINCT_QUANTIZER,
        });
    }

    // --- px::float_to_wire — the string that is MSGPACKED INTO THE SIGNED ACTION HASH ------------
    // Driven over grid points: `float_to_wire` REJECTS a value its 8-decimal form shifts by >= 1e-12,
    // so arbitrary mantissas would pin the reject path alone. The error is rendered, so a row that
    // started rejecting what it used to accept moves its hash.
    {
        let mut s = 0x2545_f491_4f6c_dd1du64;
        let values: Vec<String> = (0..SAMPLES)
            .map(|i| {
                let (_, v) = grid_point(&mut s, STEPS[i % STEPS.len()].1);
                match float_to_wire(v) {
                    Ok(w) => w,
                    Err(e) => format!("ERR:{e:?}"),
                }
            })
            .collect();
        out.push(Row {
            label: "px::float_to_wire (grid points)",
            values,
            min_distinct: MIN_DISTINCT_QUANTIZER,
        });
    }

    // --- px::clamp_price — the two-stage sig-fig + decimal-place clamp, perp and spot -------------
    // Both flavours: `MAX_DECIMALS` differs (6 perp / 8 spot), so the second stage binds at
    // different szDecimals on each.
    {
        let mut s = 0xdead_beef_cafe_babeu64;
        let values: Vec<String> = (0..SAMPLES)
            .map(|i| clamp_price(corpus_value(&mut s), (i % 7) as u32, false))
            .collect();
        out.push(Row {
            label: "px::clamp_price (perp)",
            values,
            min_distinct: MIN_DISTINCT_QUANTIZER,
        });
    }
    {
        let mut s = 0xbabe_cafe_beef_deadu64;
        let values: Vec<String> =
            (0..SAMPLES).map(|i| clamp_price(corpus_value(&mut s), (i % 9) as u32, true)).collect();
        out.push(Row {
            label: "px::clamp_price (spot)",
            values,
            min_distinct: MIN_DISTINCT_QUANTIZER,
        });
    }

    // --- px::round_size — the size floor -----------------------------------------------------------
    {
        let mut s = 0x0123_4567_89ab_cdefu64;
        let values: Vec<String> =
            (0..SAMPLES).map(|i| round_size(corpus_value(&mut s), (i % 7) as u32)).collect();
        out.push(Row { label: "px::round_size", values, min_distinct: MIN_DISTINCT_QUANTIZER });
    }

    // ============================== THE CLIFF ROWS ==============================================
    //
    // Each perturbs a production-shaped grid point by exactly one ULP and reports how many whole
    // TICKS the wire string moved: these functions cannot INTRODUCE a divergence, but they can
    // AMPLIFY one.

    {
        let mut s = 0xfeed_face_dead_c0deu64;
        let values: Vec<String> = (0..SAMPLES)
            .map(|i| {
                let step = STEPS[i % STEPS.len()].1;
                let (_, v) = grid_point(&mut s, step);
                let before = format_to_step_f(v, step);
                let after = format_to_step_f(ulp_up(v), step);
                displacement(&before, &after, step)
            })
            .collect();
        out.push(Row {
            label: "CLIFF format_to_step_f (+1 ULP)",
            values,
            // ⚠ Floor 1, as a RESULT row: with `grid_multiple_image` a grid point and its +1-ULP
            // neighbour both render the grid point (0/4096 moved, one distinct value, on both
            // platforms), so a two-outcome demand would turn the cure red. It is a REGRESSION
            // detector now: revert the witness and it returns to 139 moved, which the pin catches.
            min_distinct: MIN_DISTINCT_RESULT,
        });
    }
    {
        let mut s = 0xc0de_dead_face_feedu64;
        let values: Vec<String> = (0..SAMPLES)
            .map(|i| {
                let step = STEPS[i % STEPS.len()].1;
                let (_, v) = grid_point(&mut s, step);
                let before = format_to_step_f(v, step);
                let after = format_to_step_f(ulp_down(v), step);
                displacement(&before, &after, step)
            })
            .collect();
        out.push(Row {
            label: "CLIFF format_to_step_f (-1 ULP)",
            values,
            min_distinct: MIN_DISTINCT_CLIFF,
        });
    }

    // The platform-INDEPENDENT half: does the ordinary chain lose a step all by itself? `n` is the
    // exact number of steps the value was built from, so `emitted / step - n` is the whole-step
    // shortfall. `+0` is clean; `-1` is a full tick or lot silently dropped off a live order.
    {
        let mut s = 0x5eed_1234_abcd_9876u64;
        let values: Vec<String> = (0..SAMPLES)
            .map(|i| {
                let step = STEPS[i % STEPS.len()].1;
                let (n, v) = grid_point(&mut s, step);
                let wire = format_to_step_f(v, step);
                match wire.parse::<f64>() {
                    Ok(x) => {
                        let k = (x / step).round() - n;
                        let k = k as i64;
                        format!("{k:+}")
                    }
                    Err(_) => "?".to_string(),
                }
            })
            .collect();
        out.push(Row {
            label: "RESULT format_to_step_f (steps lost on a grid point)",
            values,
            min_distinct: MIN_DISTINCT_RESULT,
        });
    }

    // `clamp_price` rounds HALF AWAY FROM ZERO, so its cliff lives at the exact decimal MIDPOINT of
    // the last kept digit: the corpus is built there — `(k + 0.5) · 10^-allowed` — and perturbed in
    // both directions. ⚠ Displacement is in units of the DECIMAL-PLACE stage's tick; where the
    // SIGNIFICANT-FIGURE stage binds, the same move reads as a multiple of it. That is the cliff
    // being BIGGER there, not noise.
    {
        let mut s = 0x7777_8888_9999_aaaau64;
        let values: Vec<String> = (0..SAMPLES)
            .map(|i| {
                let sz = (i % 7) as u32;
                let allowed = PERP_MAX_DECIMALS.saturating_sub(sz);
                let unit = ten_pow_neg(allowed);
                let k = (lcg(&mut s) * GRID_MAX_STEPS).floor() + 1.0;
                let px = (k + 0.5) * unit;
                displacement(
                    &clamp_price(px, sz, false),
                    &clamp_price(ulp_down(px), sz, false),
                    unit,
                )
            })
            .collect();
        out.push(Row {
            label: "CLIFF px::clamp_price perp (-1 ULP at a decimal midpoint)",
            values,
            min_distinct: MIN_DISTINCT_CLIFF,
        });
    }
    {
        let mut s = 0xaaaa_9999_8888_7777u64;
        let values: Vec<String> = (0..SAMPLES)
            .map(|i| {
                let sz = (i % 7) as u32;
                let allowed = PERP_MAX_DECIMALS.saturating_sub(sz);
                let unit = ten_pow_neg(allowed);
                let k = (lcg(&mut s) * GRID_MAX_STEPS).floor() + 1.0;
                let px = (k + 0.5) * unit;
                displacement(&clamp_price(px, sz, false), &clamp_price(ulp_up(px), sz, false), unit)
            })
            .collect();
        out.push(Row {
            label: "CLIFF px::clamp_price perp (+1 ULP at a decimal midpoint)",
            values,
            min_distinct: MIN_DISTINCT_CLIFF,
        });
    }

    // `round_size` TRUNCATES, so its cliff is at the grid point itself rather than at a midpoint —
    // the same shape as `format_to_step_f`'s, one crate over and inside the signed action hash.
    {
        let mut s = 0xbbbb_cccc_dddd_eeeeu64;
        let values: Vec<String> = (0..SAMPLES)
            .map(|i| {
                let sz = (i % 7) as u32;
                let unit = ten_pow_neg(sz);
                let (_, v) = grid_point(&mut s, unit);
                displacement(&round_size(v, sz), &round_size(ulp_down(v), sz), unit)
            })
            .collect();
        out.push(Row {
            label: "CLIFF px::round_size (-1 ULP at a grid point)",
            values,
            // ⚠ Floor TWO, unlike the `CLIFF format_to_step_f (+1 ULP)` row, on purpose: the
            // direction of the perturbation is the whole of it. With the witness, `round_size(v)`
            // is the honest multiple `k`; its ULP-DOWN neighbour is no multiple's image, the
            // witness declines, and it truncates to `k-1` (a `-1`). The `+0`s come from samples
            // whose grid float sits one ULP ABOVE the decimal (`fl(0.1)` > `0.1`, so `sz=1`), where
            // the neighbour below IS the grid point. Perturbing UP has no such second population.
            // If that reasoning breaks, a collapse to one value fails loudly on the floor.
            min_distinct: MIN_DISTINCT_CLIFF,
        });
    }

    out
}

/// Distinct rendered values in a row.
fn distinct(values: &[String]) -> usize {
    let mut seen: Vec<&str> = values.iter().map(String::as_str).collect();
    seen.sort_unstable();
    seen.dedup();
    seen.len()
}

/// The human-readable half of a cliff row: how many samples moved, and how far the worst one went.
/// The hash pins the whole sequence; this is what a person reads off the terminal.
fn cliff_summary(values: &[String]) -> String {
    let (mut moved, mut unparsed, mut max_abs) = (0usize, 0usize, 0i64);
    for v in values {
        match v.parse::<i64>() {
            Ok(0) => {}
            Ok(n) => {
                moved += 1;
                max_abs = max_abs.max(n.abs());
            }
            Err(_) => unparsed += 1,
        }
    }
    format!("moved {moved}/{} max|d|={max_abs} unparsed={unparsed}", values.len())
}

// ================================ the MEASUREMENT ===============================================

#[test]
#[ignore = "measurement probe — run explicitly on each platform and diff the output"]
fn wire_quantizer_probe() {
    println!(
        "\n=== the wire quantizers — f64 -> rust_decimal -> the string that is sent/signed ==="
    );
    println!("    ({SAMPLES} samples per row, corpus = LCG x {} decades)", DECADES.len());
    for row in rows() {
        let mut h = Fnv::new();
        for v in &row.values {
            h.push(v);
        }
        let extra = if row.label.starts_with("CLIFF") || row.label.starts_with("RESULT") {
            format!("  {}", cliff_summary(&row.values))
        } else {
            String::new()
        };
        println!(
            "  {:<56} {}  n={} distinct={}{extra}",
            row.label,
            h.hex(),
            row.values.len(),
            distinct(&row.values)
        );
    }
    println!();
}

// ================================ the GATE ======================================================

/// The hash every row must produce, on EVERY platform.
///
/// ⚠ **ONE set of constants for EVERY platform — that is the entire claim.** A per-platform table
/// (`#[cfg(windows)]` / `#[cfg(unix)]`) would be this gate conceding exactly the thing it exists to
/// deny, so if one is ever proposed the answer is that a wire string moved and the call site is
/// what needs fixing.
///
/// ⚠ **A row reading `"RECORD"` is the placeholder convention** (module doc). Record by RUNNING on
/// both boxes; a mismatch panics with a paste-ready replacement block.
///
/// ⚠ **RESIDUAL — [`STEPS`] STOPS AT 1e-8 AND SHOULD NOT.** okx serves a 1e-12 tick on live
/// order-placing markets, so four rungs below 1e-8 are quantized on real orders and carry no
/// PLATFORM pin here (`crates/vike-bridge-core/tests/grid_multiple_survives_the_wire.rs`'s `LADDER`
/// watches the DEFECT on them every run). Extending [`STEPS`] is not a prose edit: every row
/// indexes it with `i % STEPS.len()`, so new rungs move every hash — case (a) of the panic message,
/// a full re-record on BOTH boxes. The Linux half is recorded in
/// `crates/vike-bridge-core/tests/format_props.proptest-regressions`; ⚠ never promote one box's
/// figures into this array on their own.
const PINNED: [(&str, &str); 15] = [
    ("corpus (LCG bits — CONTROL)", "84ca134593c61bac"),
    ("format::py_f64_str", "ad340d952f9891f6"),
    ("format::format_to_step (string steps)", "3730e326ab370b73"),
    ("format::format_to_step_f (f64 steps)", "7b5525192c9fb323"),
    ("format::format_scaled_to_step_f (rationals)", "a01125a291579d20"),
    ("px::float_to_wire (grid points)", "6cc7bf439f3dec0a"),
    ("px::clamp_price (perp)", "be8c65a7ff90abd1"),
    ("px::clamp_price (spot)", "acc36b3563606a1c"),
    ("px::round_size", "fcec55bb6e15167f"),
    ("CLIFF format_to_step_f (+1 ULP)", "37be73b273603325"),
    ("CLIFF format_to_step_f (-1 ULP)", "57ead550ec310aaf"),
    ("RESULT format_to_step_f (steps lost on a grid point)", "37be73b273603325"),
    ("CLIFF px::clamp_price perp (-1 ULP at a decimal midpoint)", "5f65507fc0b48710"),
    ("CLIFF px::clamp_price perp (+1 ULP at a decimal midpoint)", "c5415333a3c52eca"),
    ("CLIFF px::round_size (-1 ULP at a grid point)", "4c02f85b76d4961b"),
];

/// An ORDINARY test, so every `just t vike-bridge-core` and every CI roster run executes it.
///
/// It checks two different things: that every row's hash matches [`PINNED`], and that every row
/// carries enough DISTINCT values for that match to mean anything.
#[test]
fn wire_quantizer_platform_pin() {
    let got = rows();
    assert_eq!(got.len(), PINNED.len(), "row count changed — PINNED must be re-recorded");

    let mut actual: Vec<(&str, String)> = Vec::with_capacity(got.len());
    let mut bad: Vec<String> = Vec::new();
    for (row, (want_label, want_hash)) in got.iter().zip(PINNED.iter()) {
        assert_eq!(&row.label, want_label, "row order changed — PINNED must be re-recorded");

        let mut h = Fnv::new();
        for v in &row.values {
            h.push(v);
        }
        let hash = h.hex();
        actual.push((row.label, hash.clone()));

        let d = distinct(&row.values);
        if d < row.min_distinct {
            bad.push(format!(
                "{}: only {d} distinct rendered values (need >= {}) over {} samples — the row is \
                 degenerate, so its pin proves nothing. WIDEN THE CORPUS; never lower the floor.",
                row.label,
                row.min_distinct,
                row.values.len()
            ));
        }
        if hash != *want_hash {
            bad.push(format!("{}: pinned {want_hash}, computed {hash}", row.label));
        }
    }

    if !bad.is_empty() {
        let paste: String =
            actual.iter().map(|(l, h)| format!("    (\"{l}\", \"{h}\"),\n")).collect();
        panic!(
            "\nwire-quantizer pin broke:\n  {}\n\n\
             There is no tolerance here to widen, and hand-editing a digit defeats the gate.\n\
             A moved hash means a WIRE STRING moved — the price, size or contract count an order\n\
             carries, and for hyperliquid the bytes that get signed. Establish WHY first:\n\
               (a) an intended change — re-record by RUNNING this test on BOTH Windows and Linux\n\
                   and pinning the hash they AGREE on; or\n\
               (b) a quantizer changed how it reaches Decimal (a `Decimal::from_f64` where a\n\
                   `Decimal::from_str(&py_f64_str(..))` used to be), in which case the fix is in\n\
                   the source, not here.\n\n\
             ⚠ A row whose PINNED side reads RECORD is NOT a regression — it is the placeholder\n\
             convention (see PINNED's doc). Every row of this file landed that way, written on a\n\
             box that could not run cargo. It stays red until a human has run it on Windows AND on\n\
             Linux and confirmed the two agree. Paste the AGREED hash; never one box's.\n\n\
             const PINNED: [(&str, &str); {}] = [\n{paste}];\n",
            bad.join("\n  "),
            actual.len(),
        );
    }
}

// ================================ the SOURCE gate ================================================

/// The files the wire-quantizer family lives in, repo-relative: `format.rs`, the named authority,
/// and its declared twins. Reaching into other crates' sources is deliberate, the same shape as
/// `crates/vike-bridge-core/tests/bridge_conformance.rs`: a contract that spans the bridges is
/// hosted in the crate that owns it.
const WIRE_FILES: [&str; 3] = [
    "crates/vike-bridge-core/src/format.rs",
    "crates/bridges/hyperliquid/src/px.rs",
    "crates/bridges/okx/src/perp.rs",
];

/// Files a wire quantizer REACHES INTO, held to the platform-libm half of the ban and to nothing
/// else.
///
/// `crates/bridges/hyperliquid/src/px.rs`'s `round_size` calls
/// `crates/bridges/hyperliquid/src/instruments.rs`'s `pow10_neg` for its lot step (its grid witness
/// must ask against the SAME f64 the grid was built from), which puts a `10f64.powi(..)` on a
/// SIGNED quantizer's path in a file [`WIRE_FILES`] does not scan. The call is exempt
/// ([`POW10_EXEMPT`]; the witness bounds the exponent at 22 via `MAX_GRID_WITNESS_DECIMALS`), and
/// this roster is what makes a swap to a runtime-base spelling red rather than invisible.
///
/// ⚠ These files are deliberately NOT held to [`REQUIRED_SPELLING`]: they touch no `Decimal`, so
/// that check would fail on a correct file. Their non-vacuity is the read itself, which panics on
/// a path that does not resolve.
const REACHED_FILES: [&str; 1] = ["crates/bridges/hyperliquid/src/instruments.rs"];

/// Spellings that must not appear in a wire quantizer — THE source-gate rule, stated once. Two
/// families, banned for different reasons:
///
/// * `Decimal::from_f64` / `Decimal::from_f32` — the wrong DOOR into Decimal. These convert the
///   binary value's full noise; the string route (`Decimal::from_str(&py_f64_str(x))`, or
///   `Decimal::from_str(&format!("{px}"))` in `px.rs`) converts the number the caller MEANT. At an
///   exact decimal midpoint the two round opposite ways, and these quantizers truncate or compare
///   at exactly such points, so one ULP of difference becomes a whole tick or lot on the wire.
///   Banning the spelling catches the swap at every call site, not only where the corpus reaches.
///   `Decimal::from_f64_retain` needs no entry of its own: `Decimal::from_f64` is a prefix of it.
/// * The platform-libm method spellings — a transcendental inside a wire quantizer would put a
///   platform-dependent value INSIDE the amplifier rather than upstream of it (ADR 0032,
///   `docs/decisions/0032-transcendentals-come-from-the-libm-crate-not-the-platform.md`). `sqrt`
///   is absent because IEEE 754 requires it correctly rounded.
const BANNED_SPELLINGS: [&str; 26] = [
    "Decimal::from_f64",
    "Decimal::from_f32",
    ".powf(",
    ".powi(",
    ".exp(",
    ".ln(",
    ".log(",
    ".log2(",
    ".log10(",
    ".exp2(",
    ".sin(",
    ".cos(",
    ".tan(",
    ".atan(",
    ".asin(",
    ".acos(",
    ".cbrt(",
    ".hypot(",
    "f64::powf(",
    "f64::powi(",
    "f64::exp(",
    "f64::ln(",
    "f64::log(",
    "f64::log10(",
    "f64::sin(",
    "f64::cos(",
];

/// The one exempt spelling, skipped line-wise.
///
/// MEASURED on Windows/MSVC and Linux/glibc: `10f64.powi(n)` is BIT-IDENTICAL on both for every
/// `n` in `-22..=22`, because powers of ten in that range are exactly representable
/// (`crates/vike-chart/src/scale.rs`'s `log_nice_values` carries the recorded sweep).
///
/// In `crates/bridges/hyperliquid/src/px.rs` the occurrences build decade sweeps inside its test
/// module; in `crates/bridges/hyperliquid/src/instruments.rs` ([`REACHED_FILES`]) the exempted line
/// IS the production body of `pow10_neg`, so the exemption is load-bearing. Its whole justification
/// is the BASE (ADR 0032's rule): a literal `10` or `2` needs nothing, an arbitrary runtime `f64`
/// base needs `libm::pow`. A `powf`, or a `powi` on a computed base, is not covered and goes red.
const POW10_EXEMPT: &str = "10f64.powi(";

/// The spelling every wire quantizer MUST contain — the string-mediated door into Decimal.
///
/// The NON-VACUITY arm: a scan of the wrong path or an empty file would report a confident zero
/// banned hits, and requiring a POSITIVE hit makes that loud. Required of [`WIRE_FILES`] ONLY,
/// never of [`REACHED_FILES`] (the reason is on that constant).
const REQUIRED_SPELLING: &str = "Decimal::from_str";

// Same spelling as `crates/vike-ops/tests/common/repo.rs`'s `workspace_root` (keeps the `..`); the
// `parent()` twins, e.g. `crates/vike-catalog/tests/baseline_artifact.rs`'s `repo_root`, do not.
fn repo_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is `<root>/crates/vike-bridge-core`.
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

/// The wire quantizers must reach `Decimal` through a STRING, and must contain no platform-libm
/// call.
///
/// ⚠ **This gate bans its spellings across the WHOLE file — comments and `#[cfg(test)]` modules
/// included — where the sibling libm gates exclude test modules, on purpose.** They scan whole
/// `src/` trees, where a total ban would be red on day one; this one scans a handful of NAMED
/// FILES, where a total ban is green and stronger (a test there building its expectation with
/// `Decimal::from_f64` would assert the wrong construction), and a fourth copy of their
/// test-module range walk would be more machinery than subject. ⚠ The cost: prose in a scanned
/// file may not NAME a banned spelling. This probe is not scanned, so that prose lives here.
#[test]
fn the_wire_quantizers_reach_decimal_through_a_string() {
    let root = repo_root();
    let mut found: Vec<String> = Vec::new();
    let mut missing: Vec<String> = Vec::new();

    for rel in WIRE_FILES.iter().chain(REACHED_FILES.iter()).copied() {
        let path = root.join(rel);
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("wire-quantizer source {rel} is unreadable: {e}"));

        // The Decimal-door arm applies to the QUANTIZERS only; a REACHED_FILES entry reaches no
        // Decimal and would fail this while being entirely correct.
        if WIRE_FILES.contains(&rel) && !text.contains(REQUIRED_SPELLING) {
            missing.push(rel.to_string());
        }
        for (i, line) in text.lines().enumerate() {
            if line.contains(POW10_EXEMPT) {
                continue;
            }
            for pat in BANNED_SPELLINGS {
                if line.contains(pat) {
                    found.push(format!("  {rel}:{} {}", i + 1, line.trim()));
                }
            }
        }
    }

    assert!(
        missing.is_empty(),
        "a wire quantizer no longer contains `{REQUIRED_SPELLING}` — either the file moved (fix \
         WIRE_FILES) or it stopped reaching Decimal through a string, which is the change this \
         gate exists to catch. Without a positive hit, an empty `found` below proves nothing:\n  \
         {}",
        missing.join("\n  ")
    );
    assert!(
        found.is_empty(),
        "a wire quantizer must reach `Decimal` through a STRING and must call no platform \
         transcendental. `Decimal::from_f64` converts the binary value's noise where \
         `Decimal::from_str(&py_f64_str(x))` converts the number the caller meant — at an exact \
         decimal midpoint the two round opposite ways, and these functions decide the price, size \
         and contract count on live orders (and, at hyperliquid, the bytes that get signed). The \
         one exempt spelling is `{POW10_EXEMPT}` (measured bit-identical on both platforms; see \
         POW10_EXEMPT):\n{}",
        found.join("\n")
    );
}
