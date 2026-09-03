//! MEASUREMENT probe **and** the committed cross-platform GATE for the WIRE QUANTIZERS — every
//! place in this workspace where an `f64` crosses into `rust_decimal` and comes back out as the
//! string that is SENT to a venue or SIGNED into an order hash.
//!
//! It joins the platform-probe family. `crates/vike-indicators/tests/libm_platform_probe.rs`,
//! `crates/vike-analytics/tests/libm_platform_probe.rs` and `crates/vike-mm/src/platform_probe.rs`
//! each measured the `f64` TRANSCENDENTAL surface of one crate and then converted it; the verdict
//! they serve is
//! `docs/decisions/0032-transcendentals-come-from-the-libm-crate-not-the-platform.md` and it is
//! deliberately NOT re-derived here. This file measures the surface those three do not touch: the
//! non-`f64` boundary, where a last-bit difference stops being a last bit.
//!
//! ```text
//! <cargo> test -p vike-bridge-core --test wire_quantizer_probe -- --ignored --nocapture
//! ```
//!
//! # Why this file exists: a sweep returned CLEAN by READING
//!
//! A survey of the non-`f64` numeric surface — `f32` transcendentals, the `Decimal` boundaries,
//! integer-rounding cliffs downstream of a converted call — reported nothing to fix. It reached
//! that verdict by reading source, and reading is exactly what this whole program exists to
//! distrust: "`powi` lowers to a multiply chain, not a libm call" read as obviously true, was
//! written into two probe headers as an exclusion, and was FALSE on MSVC in a `dev` build. The
//! three categories are answered below by construction rather than by inspection.
//!
//! ## 1. `f32` transcendentals — NOTHING TO PROBE, and the derivation rather than the assertion
//!
//! There is no `f32` transcendental in production anywhere in this workspace, and no probe is
//! built for one. The derivation, so the next reader can redo it rather than trust this line:
//!
//! ```text
//! git grep -nE 'f32' -- 'crates/**/*.rs' | grep -E '\.(powf|powi|exp|ln|log|log10|sin|cos|…)\('
//! ```
//!
//! returns nothing — no source line in the tree carries both an `f32` and a transcendental call.
//! An inferred-type `f32` could still escape that, so the crates that USE `f32` were enumerated
//! and each one's transcendental sites were typed by hand. Every hit is one of four shapes, and
//! not one of them is a number anybody reads:
//!
//! * **egui PIXEL space** — `crates/vike-chart`, `crates/vike-panels`, `crates/vike-cockpit`,
//!   `crates/vike-app-core`, `crates/vike-studio`, `crates/vike-ui-theme`: rect geometry, colour
//!   channels, animation. `crates/vike-chart/src/render.rs`'s `draw_pnf` ellipse ring and
//!   `crates/vike-chart/src/interact.rs`'s `K_DRAG` zoom factor DO call transcendentals — but
//!   both on `f64`, and both through `libm` already.
//! * **A wire DECODE** — `crates/bridges/dukascopy/src/data.rs`'s `decode_ticks` reads
//!   `f32::from_be_bytes(..) as f64`. Both halves are EXACT: a big-endian bit reinterpretation and
//!   an `f32`→`f64` widening, which is lossless in IEEE 754. No transcendental, nothing to diverge.
//! * **A model LABEL width** — `crates/vike-ml/src/train.rs`'s `TrainData` carries a `y` field of
//!   `&[f32]`, because that is LightGBM's own label width. It is hashed
//!   (`crates/vike-ml/src/gbdt_learner.rs`'s `fingerprint_labels`) and widened (`f64::from`),
//!   never transformed.
//!   `crates/vike-ml/src/metrics.rs`'s `binary_logloss` takes those `f32` labels and immediately
//!   widens them; its logarithms are `libm::log` on `f64`.
//! * **One `f32` CONSTANT that decides something** — `crates/vike-ml/src/model.rs`'s
//!   `ZERO_THRESHOLD` is `1e-35f32 as f64`, reproducing LightGBM's C++ widening. It is
//!   const-evaluated and exact; it is a threshold, not a computation.
//!
//! ⚠ A probe over a vacuous corpus hashes the same on every platform and reads as a pass while
//! proving nothing — `crates/vike-analytics/tests/libm_platform_probe.rs`'s `overfit::pbo_cscv`
//! row is the worked example, kept in the tree reporting its own vacuity. Building an `f32` probe
//! here would have been exactly that, so the category is declared empty instead.
//!
//! ## 2. The `Decimal` boundaries — the subject of this file
//!
//! `CLAUDE.md` calls `format_to_step` "the ONE pinned Decimal wire site" (plus OKX's
//! `to_contracts`). ⚠ **That is stale, and the correction is the reason this probe has a roster
//! rather than a subject.** `crates/bridges/hyperliquid/src/px.rs` is a THIRD, and it says so in
//! its own module doc — "`rust_decimal` is the one exact-arithmetic site here, the twin of
//! `vike_bridge_core::format::format_to_step`". `crates/vike-bridge-core/src/format.rs`'s header
//! carried the same "only here and at OKX" claim and has been corrected in place. The roster this
//! file drives is on [`rows`]; [`WIRE_FILES`] is the roster the source gate walks.
//!
//! ## 3. Integer-rounding cliffs — and the two that must NOT be re-reported
//!
//! `crates/vike-app-core/src/orderflow.rs`'s `nice_orderflow_tick` (a `log10().floor()`, a
//! TEN-FOLD divergence) and `crates/vike-mm/src/avellaneda.rs`'s `effective_blackout_ms` (a
//! `.round() as i64`, a whole millisecond) are known. A third was already found and cured in
//! place: `crates/vike-chart/src/scale.rs`'s `log_nice_values` feeds `libm::log10` straight into
//! `.floor() as i32` and its own doc names the decade cliff.
//!
//! **The ones nobody had are the wire quantizers themselves**, and they are the highest-stakes
//! instance of the shape in the tree, because the integer they round to is a TICK or a LOT on a
//! live order:
//!
//! | site | quantizer | one ULP upstream becomes |
//! |---|---|---|
//! | [`format_to_step`] / [`format_to_step_f`] | `(value / step).trunc()` | a whole TICK or LOT |
//! | [`format_scaled_to_step_f`] | the same `trunc`, after an exact rational rescale | a whole step |
//! | `crates/bridges/okx/src/perp.rs`'s `to_contracts` | `(raw / ct / step).trunc()` | a whole CONTRACT step |
//! | `crates/bridges/hyperliquid/src/px.rs`'s `clamp_price` | `MidpointAwayFromZero` | a whole tick, in the SIGNED action hash |
//! | `crates/bridges/hyperliquid/src/px.rs`'s `round_size` | `ToZero` | a whole lot |
//! | `crates/bridges/polymarket/src/order.rs`'s `to_base_units` | `(x * 1e6).round()` | one micro-unit, in the SIGNED EIP-712 order |
//!
//! That table is what makes the "clean" reading dangerous rather than merely incomplete. A
//! transcendental that is 1 ulp apart on two boxes produces two prices that differ in the 16th
//! digit — invisible, and arguably harmless. Push those same two prices through a quantizer whose
//! decision is a TRUNCATION or an exact-midpoint comparison, and the two boxes emit different WIRE
//! STRINGS: a different limit price, a different size, and for the two EIP-712 venues a different
//! signature over a different order. The cliff rows below measure how often that happens and how
//! far it moves, over the value shape the production chain actually produces.
//!
//! # ⚠ What the PIN half of this file does and does not claim
//!
//! **None of these functions calls a platform transcendental** — that is derived, not assumed:
//! [`the_wire_quantizers_reach_decimal_through_a_string`] scans all three files for every
//! platform-libm spelling and for `Decimal::from_f64`, and the whole path is Rust's own
//! shortest-round-trip float formatter (`core`'s, not the platform's) plus `rust_decimal`'s
//! integer arithmetic.
//!
//! ⚠ **That sentence read "calls a transcendental at all" until `round_size` grew its grid
//! witness, and the stronger wording had stopped being true.** `crates/bridges/hyperliquid/src/px.rs`'s
//! `grid_multiple_image` needs the lot step SPELLED ONCE — the same f64 the grid was built from —
//! so it calls `crates/bridges/hyperliquid/src/instruments.rs`'s `pow10_neg`, which is a
//! `10f64.powi(..)`. That is a libm SPELLING (LLVM lowers `llvm.powi` to the CRT's `pow` on MSVC
//! in a `dev` build) on a SIGNED quantizer's path. It is exempt for the reason [`POW10_EXEMPT`]
//! records and no other — base ten, and an exponent the witness itself bounds at 22, where every
//! power of ten is exactly representable — and it is DERIVED rather than trusted:
//! [`REACHED_FILES`] puts that file under the same banned-spelling scan, so an edit swapping it
//! for a runtime-base `powf` is red here rather than invisible. The alternative was leaving the
//! only protection a doc comment, which is the exact failure mode this file opens by describing.
//!
//! So these outputs are platform-invariant BY CONSTRUCTION, and the committed
//! table below is a REGRESSION TRIPWIRE rather than the closing of a divergence that is moving
//! numbers today. That is the same honest framing the analytics twin
//! (`crates/vike-analytics/tests/libm_platform_probe.rs`) gives its two `stats` rows, and it is
//! worth stating plainly: a set of agreeing numbers looks identical whether it was always true or
//! was made true.
//!
//! What the tripwire is FOR is the one edit that would quietly break it — swapping
//! `Decimal::from_str(&py_f64_str(x))` for `Decimal::from_f64(x)`. The two are not the same
//! function: the first converts the number the caller MEANT (the shortest decimal that
//! round-trips), the second converts the binary value's full noise, and at an exact midpoint they
//! round opposite ways. Both `format.rs`'s `dec_from_f64` and `px.rs`'s `clamp_price` go through
//! the string deliberately, and `px.rs` says why at the site.
//!
//! # What FEEDS these quantizers
//!
//! The live chain, named by symbol so it can be re-walked:
//!
//! 1. a strategy computes a price — for the maker that is
//!    `crates/vike-mm/src/avellaneda.rs`'s `as_reservation_price`, and the half-spread beside it
//!    (`as_optimal_half_spread`), whose logarithms are `libm` since
//!    `crates/vike-mm/src/platform_probe.rs` measured them;
//! 2. `crates/vike-mm/src/avellaneda.rs`'s `snap_to_tick` snaps it, delegating to
//!    `crates/vike-model/src/scalar.rs`'s `round_to_step`;
//! 3. `crates/vike-exec/src/risk.rs`'s `RiskGate` re-rounds through the same `round_to`;
//! 4. the venue adapter formats it — `format_to_step_f` for binance/bybit/okx/aster, `clamp_price`
//!    / `round_size` for hyperliquid.
//!
//! Steps 2 and 3 are `(value / step).round_ties_even() * step`, and BOTH halves of that are
//! required by IEEE 754 to be correctly rounded, so they are already platform-invariant. Step 1 is
//! not, by nature — which is why it was converted. **So the answer to "is anything upstream a
//! platform-libm value the Decimal conversion then treats as exact" is: it was, it no longer is,
//! and this file is what makes a reversion cost a visible tick instead of an invisible bit.**
//!
//! There is a second, platform-INDEPENDENT thing the same chain can do, and the `steps lost` row
//! below is what measures it. `round_to_step` ends in a MULTIPLICATION, and `n * step` is not
//! always the f64 nearest the decimal grid point `n·step` — it can land one ULP BELOW it, where
//! `format_to_step`'s truncation then costs a full step. `format_scaled_to_step_f`'s own doc
//! records that hazard for the non-dyadic-rational case (`0.03 · ⅓` at step `0.0005` emits
//! `"0.0095"` through the f64 route and `"0.0100"` through the Decimal one). Whether the ORDINARY
//! `round_to_step` → `format_to_step_f` chain ever hit it was unmeasured when this row was written;
//! that row measured it, and the answer was **150 of 4096** — a whole step dropped on a value that
//! was already on the grid, with no perturbation applied at all. That number is what produced
//! #1562's cure, and the row now records `0/4096`. ⚠ Read its all-`+0` column as a RESULT rather
//! than as a collapsed corpus: [`MIN_DISTINCT_RESULT`] carries the argument for why that floor is
//! `1`, and the row is a regression detector now rather than a discovery.
//!
//! # Declared coverage, and the gaps declared as gaps
//!
//! [`rows`] covers `vike_bridge_core::format`'s three public quantizers plus `py_f64_str` (the
//! shim every one of them goes through), and hyperliquid's three. Deribit's combo path needs no
//! row of its own: `crates/bridges/deribit/src/combo.rs`'s `build_combo_order_params` quantizes through
//! `format_scaled_to_step_f`, which is the `format::format_scaled_to_step_f` row.
//!
//! ⚠ **TWO sites are covered by the SOURCE GATE and not by the hash pin**, each for a stated
//! reason rather than by oversight — the same shape `crates/vike-mm/src/platform_probe.rs`
//! declares for the sites its own pin cannot reach:
//!
//! * `crates/bridges/okx/src/perp.rs`'s `to_contracts` is a METHOD on `OkxPerpRest<T>`, so calling
//!   it means standing up a signer and a `T: OkxTransport` double — a stateful client, which is a
//!   different test with different failure modes. It is not unpinned, either:
//!   `crates/bridges/okx/tests/offline/r6_okx_parity.rs` compares it bit-for-bit against the
//!   frozen `fixtures/r6` export, which is a stronger statement than a hash for the values that
//!   fixture carries — and a weaker one for every value it does not.
//! * `crates/bridges/polymarket/src/order.rs`'s `to_base_units` is private, and its public caller
//!   `build_order` lives behind vike-polymarket's `polymarket` feature, which this crate does not
//!   take. Its cliff is the sharpest in the table above (a `.round()` at an exact half, inside the signed
//!   EIP-712 order) and it deserves a probe in ITS crate.
//!
//! # ⚠ The corpus is PURE ARITHMETIC, and that is load-bearing
//!
//! Inputs come from an LCG and `+ - * /` only, never from `.sin()`/`.cos()`. Those are themselves
//! platform-dependent (measured, in the indicators twin), so generating inputs with them would
//! make every column below diverge because of the INPUTS rather than the function under test — a
//! false positive that reads exactly like a real one. [`corpus_value`] is the CONTROL row: it hashes
//! the generator's own output, so a table where every other row moved but that one did not is a
//! function regression, and a table where that row moved too is a broken corpus.
//!
//! # ⚠ Every pinned constant below reads `"RECORD"`, and that is a PLACEHOLDER which fails by
//! construction
//!
//! The convention is `crates/vike-analytics/tests/libm_platform_probe.rs`'s and
//! `crates/vike-indicators/tests/libm_platform_probe.rs`'s, adopted verbatim: `"RECORD"` cannot
//! match any output of [`Fnv::hex`], which is `format!("{:016x}", ..)` — sixteen characters from
//! `0-9a-f`. `"RECORD"` is six characters and four of them are outside that alphabet, so it fails
//! on length and on content independently.
//!
//! This whole file was written on a box that could not run cargo, so **every row is unrecorded**.
//! A human must run it on Windows/MSVC `dev` AND on Linux/glibc `dev`, confirm the two runs agree
//! bit-for-bit, and paste the AGREED hashes — never one box's. A red row spelled `"RECORD"` is an
//! UNFINISHED RECORDING, not a regression; that distinction is the entire reason the placeholder is
//! a word rather than a plausible-looking hex literal, which would be indistinguishable from a
//! measured one while pinning nothing at all.

use std::path::{Path, PathBuf};

use vike_bridge_core::format::{
    format_scaled_to_step_f, format_to_step, format_to_step_f, py_f64_str,
};
use vike_hyperliquid::consts::PERP_MAX_DECIMALS;
use vike_hyperliquid::px::{clamp_price, float_to_wire, round_size};

// ================================ hashing and the corpus =========================================

/// FNV-1a, the same hasher the three sibling probes use — but over BYTES rather than over
/// `f64::to_bits`, because every row here produces a STRING.
///
/// The NaN canonicalisation the siblings need has no analogue: a quantizer's output is a decimal
/// string, and `format_to_step`'s own zero handling already collapses the two signed zeros to one
/// spelling per its Python parity rule. What this hasher needs instead is a SEPARATOR, so that the
/// two-row sequence `["ab", "c"]` cannot hash equal to `["a", "bc"]` — a collision that would let a
/// row's boundaries move without the hash noticing.
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
/// ⚠ No transcendental anywhere in here, for the reason the module doc gives. The `as f64` and the
/// single division are exact-or-correctly-rounded operations, so the corpus is bit-identical on
/// every platform by construction — which is what makes a divergence in the OUTPUT attributable to
/// the function under test rather than to its inputs.
fn lcg(seed: &mut u64) -> f64 {
    *seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
    // Top 53 bits over 2^53: an exact ratio of integers, then one correctly-rounded divide.
    ((*seed >> 11) as f64) / ((1u64 << 53) as f64)
}

/// The magnitudes a real price or size occupies, from the 1e-6 end of a prediction-market
/// probability to the 1e4 end of a BTC price. Decimal LITERALS, so the compiler's own
/// decimal→binary conversion (exact, and part of the language rather than of a libm) fixes each
/// one identically on every platform.
const DECADES: [f64; 11] = [1e-6, 1e-5, 1e-4, 1e-3, 1e-2, 1e-1, 1.0, 1e1, 1e2, 1e3, 1e4];

/// A corpus value: a mantissa in `[1, 10)` times one of [`DECADES`].
///
/// Deliberately NOT uniform over a single range. Every quantizer here is scale-sensitive —
/// `clamp_price`'s significant-figure stage keys off the decade, `format_to_step`'s truncation
/// keys off `value / step` — so a corpus confined to one decade would exercise one branch of each
/// and report a confident hash over it.
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

/// The real venue tick/lot grids, in both spellings the two entry points take. Transcribed from
/// `crates/vike-bridge-core/tests/format_props.rs`'s `step_strategy`, which chose them for the same
/// reason: every one is a TERMINATING decimal, so the internal Decimal division is exact and a
/// moved output is a genuine wire change rather than a rounding artifact.
///
/// The pairing is not redundant. [`format_to_step`] takes the STRING and
/// [`format_to_step_f`] takes the `f64` and routes it through [`py_f64_str`] — so the two entry
/// points can disagree, and pinning only one would leave the other's `str(float)` shim uncovered.
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

/// Samples per row. Large enough that a rare disagreement survives into the hash rather than being
/// rounded away — the trap the indicators twin fell into twice with a single series, where two
/// genuinely divergent indicators both reported "identical".
const SAMPLES: usize = 4096;

/// The largest whole-step multiple a grid point is built from. Bounded so that `n as f64 * step`
/// stays exactly representable as an integer count and the `steps lost` arithmetic below is a
/// small-integer subtraction rather than a large-magnitude one.
const GRID_MAX_STEPS: f64 = 1_000_000.0;

// ================================ the ULP and cliff machinery ====================================

/// The next representable `f64` above `x`, for a POSITIVE finite `x`.
///
/// Spelled as a bit increment rather than as `f64::next_up` deliberately: this file's whole subject
/// is what one ULP costs downstream, and the bit increment is the definition rather than a call
/// that has to be looked up. Non-positive or non-finite input passes through unchanged — every
/// price and size in the corpus is strictly positive, and a silent identity there is preferable to
/// a panic inside a measurement.
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
/// in.
///
/// ⚠ **A `powi` in a platform probe, deliberately, and it must stay one.** MEASURED 2026-08-26
/// across the Windows dev box (MSVC) and the CI Linux runners (glibc): `10f64.powi(n)` is
/// BIT-IDENTICAL on both for every `n` in `-22..=22`, because powers of ten in that range are
/// exactly representable in `f64` and there is no rounding for two libms to disagree about.
/// `crates/vike-chart/src/scale.rs`'s `log_nice_values` carries the recorded sweep and the same
/// exemption at the same spelling. `n` here is at most `SPOT_MAX_DECIMALS`, far inside the range.
fn ten_pow_neg(n: u32) -> f64 {
    10f64.powi(-(n as i32))
}

/// A production-shaped grid point: `(n, round_to_step(n · step, step))`.
///
/// This is the value shape the live path actually hands a quantizer, not an arbitrary float. The
/// multiplication is where the interesting error enters — `n as f64 * step` is not always the `f64`
/// nearest the decimal `n·step` — and `crates/vike-model/src/scalar.rs`'s `round_to_step` is the
/// EXACT snap `crates/vike-mm/src/avellaneda.rs`'s `snap_to_tick` delegates to, and the one
/// `crates/vike-exec/src/risk.rs`'s `RiskGate` reaches through its own `round_to` — so driving it
/// here drives what both of them produce.
fn grid_point(seed: &mut u64, step: f64) -> (f64, f64) {
    let n = (lcg(seed) * GRID_MAX_STEPS).floor() + 1.0;
    (n, vike_model::round_to_step(n * step, step))
}

/// How far a quantizer's output moved, in whole `unit`s, rendered as a signed integer.
///
/// `unit` is the tick the quantizer works in — the step for [`format_to_step_f`], `10^-decimals`
/// for the hyperliquid pair. The subtraction and division are `f64`, which is adequate BECAUSE the
/// answer is a small integer: both operands parse through Rust's own correctly-rounded decimal
/// parser, their difference is a low multiple of `unit`, and the `.round()` that follows collapses
/// any residue. Rendering it as a STRING keeps every row of this file on one hashing and one
/// non-vacuity mechanism.
///
/// `"?"` is the un-parseable answer — reserved for a quantizer that emitted something outside plain
/// decimal notation. It is a distinct rendered value, so a row that started producing them would
/// move its hash rather than being silently absorbed into `+0`.
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
    /// ⚠ NON-DEGENERACY GUARD, and it is PER ROW here rather than one global constant, because
    /// this file's rows genuinely have different value-set sizes. A quantizer row emits thousands
    /// of distinct strings; a cliff row emits a handful of small integers. One threshold would
    /// have to be set low enough for the second, which would stop guarding the first at all.
    min_distinct: usize,
}

/// The floor for a QUANTIZER row — a row that maps 4096 corpus values through a formatter and
/// should produce something close to 4096 distinct strings. Anything near 1 means the corpus
/// collapsed (all-zero, all-error) and the row's cross-platform agreement would be vacuous.
const MIN_DISTINCT_QUANTIZER: usize = 256;

/// The floor for a CLIFF row that perturbs by one ULP: at least two outcomes must occur — some
/// samples moved and some did not.
///
/// ⚠ A red here is a CORPUS finding, not a regression. `distinct == 1` means either the
/// perturbation never crossed a quantization boundary in 4096 tries (the corpus is aimed wrong) or
/// it always did (likewise). Both call for a wider or better-aimed corpus, never for lowering this
/// number — the threshold is the only thing standing between these rows and a hash that agrees
/// across platforms because it is constant.
const MIN_DISTINCT_CLIFF: usize = 2;

/// The floor for the `steps lost` row, and the one place this file accepts `1`.
///
/// ⚠ This row is NOT a perturbation experiment; it asks whether the ordinary
/// `round_to_step` → `format_to_step_f` chain ever drops a whole step on a value that was already
/// on the grid. An all-`+0` column there is a RESULT — the chain is clean on this corpus — and the
/// probe prints the count so the result stays legible. Demanding two outcomes would turn good news
/// red.
const MIN_DISTINCT_RESULT: usize = 1;

/// Every covered quantizer and every cliff, in a FIXED order. [`PINNED`] is compared positionally,
/// so a reordering is a re-record rather than a silent re-pairing.
///
/// Shared by the printing probe and by the gate ON PURPOSE: two sweeps free to drift apart would
/// let the gate hold something the measurement no longer describes.
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
    // A third of the corpus is negated: `quantize_to_step` carries a bespoke NEGATIVE-ZERO arm
    // (Python Decimal keeps `-0.00000000` through trunc/mult where rust_decimal normalises it
    // away), and a positive-only corpus never reaches it.
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
    // The only quantizer in this family with NO frozen-fixture pin of its own: `format.rs`'s
    // in-file tests cover four hand-written cases and `fixtures/r6/format.json` predates the
    // function. It is also the one Deribit's combo path (`crates/bridges/deribit/src/combo.rs`'s
    // `build_combo_order_params`) quantizes both amount AND price through.
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
    // Driven over grid points rather than raw corpus values: `float_to_wire` REJECTS anything whose
    // 8-decimal form shifts the value by >= 1e-12, so a corpus of arbitrary mantissas would be
    // almost all `Err` and would pin the reject path alone. The error is rendered rather than
    // dropped — a row that started rejecting what it used to accept must move its hash.
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
    // Both flavours, because `MAX_DECIMALS` differs (6 perp / 8 spot) and the second stage is the
    // binding one at different szDecimals on each.
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
    // TICKS the emitted wire string moved. This is the measurement the module doc's table promises,
    // and it is the reason "the Decimal path carries no libm call" is not the end of the story: it
    // says these functions cannot INTRODUCE a divergence, not that they cannot AMPLIFY one.

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
            // ⚠ **The floor drops to 1 in the same commit that fixes the defect, and the drop IS
            // the evidence.** Before `grid_multiple_image`, `format_to_step_f(grid)` was itself a
            // whole step low on 150 of these 4096 grid points, so nudging the input UP disagreed
            // with it 139 times and this row genuinely had two outcomes. With the wire honest, a
            // grid point and its +1-ULP neighbour both render the grid point: measured 0/4096
            // moved, one distinct value, on Windows/MSVC and Linux/glibc alike. The phenomenon
            // this row was built to catch is GONE, so [`MIN_DISTINCT_CLIFF`]'s two-outcome demand
            // would turn the cure red — which is the argument [`MIN_DISTINCT_RESULT`] already
            // carries for its own row, arriving here for the same reason.
            //
            // The row is kept, and is now a REGRESSION detector rather than a measurement: revert
            // the witness and it returns to 139 moved and two outcomes. The pinned hash catches
            // that move in either direction, so nothing is lost by the softer floor — what would
            // be lost is the ability to tell a cured phenomenon from a collapsed corpus, and the
            // paragraph above is what preserves it.
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

    // `clamp_price` rounds HALF AWAY FROM ZERO, so a uniform corpus almost never sits near its
    // decision. The cliff lives at the exact decimal MIDPOINT of the last kept digit, so the corpus
    // is built there — `(k + 0.5) · 10^-allowed` — and perturbed in both directions.
    //
    // ⚠ The displacement is reported in units of `10^-allowed`, the DECIMAL-PLACE stage's tick.
    // When the SIGNIFICANT-FIGURE stage is the binding one (a large price at a small szDecimals)
    // the same move reads as a multiple of that unit rather than as `±1`. That is not noise: it is
    // the cliff being BIGGER there, and it is why the number is measured rather than predicted.
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
            // ⚠ **This row MOVES when `round_size` gains its grid witness, and its floor STAYS at
            // two outcomes — the opposite of what happened to the `CLIFF format_to_step_f (+1
            // ULP)` row above, which collapsed to ONE the moment its quantizer stopped losing a
            // step.** Two sibling rows curing the same defect and landing on different floors is
            // exactly the kind of thing that reads as an oversight, so the difference is argued
            // here rather than left to be "tidied".
            //
            // The direction of the perturbation is the whole of it. With the witness in,
            // `round_size(v)` is the honest multiple `k`; its ULP-DOWN neighbour is no longer any
            // multiple's f64 image, the witness declines for it, and it truncates to `k-1` — a
            // `-1`. The `+0`s that keep the row non-degenerate come from the samples where the
            // grid point's own float sits one ULP ABOVE the decimal (`fl(0.1)` is above `0.1`, so
            // `sz=1` produces them), because there the neighbour below IS the decimal grid point
            // and renders as `k`. The `+1 ULP` row has no such second population: perturbing UP
            // from an honest grid point lands inside the same step every time.
            //
            // What the fix actually moves here is the `sz=6` defect samples, which read `+0`
            // before for the WRONG reason — the two sides agreed only because both were a lot
            // short — and read `-1` after.
            //
            // If that reasoning is wrong the gate says so out loud rather than silently: a
            // collapse to one value fails on [`MIN_DISTINCT_CLIFF`] with the corpus named, which
            // is the outcome this floor exists to produce.
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
/// ⚠ **EVERY row is `"RECORD"`** — see the module doc for the convention and for why a
/// plausible-looking hex literal invented here would be the worst possible thing to write. Record
/// by RUNNING on both boxes; a mismatch panics with a paste-ready replacement block.
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
    // ⚠ RE-RECORD. `crates/bridges/hyperliquid/src/px.rs`'s `round_size` gained the grid witness
    // `crates/bridges/hyperliquid/src/px.rs`'s `grid_multiple_image` — the same cure
    // `crates/vike-bridge-core/src/format.rs`'s `grid_multiple_image` already carries — so this
    // row's INTENDED move is case (a) of the panic message below. The recorded value was
    // `f8f81429088d332f` at `moved 3032/4096`; the defect samples that used to read `+0` because
    // both sides were a lot short now read `-1`, so the count rises and the hash changes. The
    // floor stays at two outcomes, argued at the row.
    ("CLIFF px::round_size (-1 ULP at a grid point)", "4c02f85b76d4961b"),
];

/// An ORDINARY test, so every `just t vike-bridge-core` and every CI roster run executes it.
///
/// It checks two things at once, and they are not the same thing: that every row's hash matches
/// [`PINNED`], and that every row carries enough DISTINCT values for that match to mean anything.
/// A row that collapsed to one value would agree across platforms for a reason that has nothing to
/// do with the function under test.
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

/// The three files the wire-quantizer family lives in, repo-relative.
///
/// ⚠ This gate reaches into two OTHER crates' sources, which the three sibling probes do not do.
/// That is deliberate and follows the precedent already in this directory: the venue-adapter
/// contract spans every bridge and no bridge contains it, so
/// `crates/vike-bridge-core/tests/bridge_conformance.rs` and its market-data twin are hosted here,
/// "in the crate whose venue-adapter contract it machine-checks". The Decimal wire contract is the
/// same shape — `vike-bridge-core` owns `format.rs`, the named authority, and the other two are
/// its declared twins.
const WIRE_FILES: [&str; 3] = [
    "crates/vike-bridge-core/src/format.rs",
    "crates/bridges/hyperliquid/src/px.rs",
    "crates/bridges/okx/src/perp.rs",
];

/// Files a wire quantizer REACHES INTO, held to the platform-libm half of the ban and to nothing
/// else.
///
/// ⚠ **This roster exists because a fix opened a hole in the gate's own derivation, and the honest
/// close was to widen the scan rather than to soften the claim.**
/// `crates/bridges/hyperliquid/src/px.rs`'s `round_size` calls
/// `crates/bridges/hyperliquid/src/instruments.rs`'s `pow10_neg` for its lot step — deliberately,
/// because its grid witness can only ask its question against the SAME f64 the grid was built
/// from, and a second spelling disagreeing by one bit would make it decline exactly where it is
/// needed. The cost of that reuse is a `10f64.powi(..)` on the path of a quantizer whose output is
/// SIGNED, sitting in a file [`WIRE_FILES`] does not scan.
///
/// The call is exempt (see [`POW10_EXEMPT`]: base ten, exponent bounded at 22 by the witness's own
/// `MAX_GRID_WITNESS_DECIMALS`, every such power exactly representable) — but `pow10_neg`'s
/// portability is recorded in ADR 0032 and in that function's doc comment and in NO RUNNING TEST,
/// so before this roster the only thing standing between a signed order size and a runtime-base
/// `powf` was prose. Now an edit to that spelling is red here.
///
/// ⚠ These files are deliberately NOT held to [`REQUIRED_SPELLING`]. They are not quantizers and
/// touch no `Decimal` at all, so demanding `Decimal::from_str` of them would be a non-vacuity
/// check over the wrong property — and one that would fail on a correct file, which is the worst
/// shape a gate can have. Their non-vacuity is that the file was READ, which the read enforces by
/// panicking on a path that does not resolve.
const REACHED_FILES: [&str; 1] = ["crates/bridges/hyperliquid/src/instruments.rs"];

/// Spellings that must not appear in a wire quantizer, in the ONE list the gate renders.
///
/// Two families, and they are banned for different reasons:
///
/// * `Decimal::from_f64` / `Decimal::from_f32` — the wrong DOOR into Decimal. These convert the
///   binary value's full noise; the string route (`Decimal::from_str(&py_f64_str(x))`, or
///   `Decimal::from_str(&format!("{px}"))` in `px.rs`) converts the number the caller MEANT. At an
///   exact decimal midpoint the two round opposite ways, so swapping one for the other silently
///   moves wire strings — the exact failure the hash pin above would catch, caught one layer
///   earlier and at every call site rather than only the ones the corpus reaches.
///   `Decimal::from_f64_retain` needs no entry of its own: `Decimal::from_f64` is a prefix of it.
/// * The platform-libm method spellings — a wire quantizer's job is exact arithmetic, and a
///   transcendental appearing in one would put a platform-dependent value INSIDE the amplifier
///   rather than upstream of it. The verdict is ADR 0032
///   (`docs/decisions/0032-transcendentals-come-from-the-libm-crate-not-the-platform.md`); `sqrt`
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
/// MEASURED 2026-08-26 on the Windows dev box (MSVC) and the CI Linux runners (glibc):
/// `10f64.powi(n)` is BIT-IDENTICAL on both for every `n` in `-22..=22`, because powers of ten in
/// that range are exactly representable and there is nothing for two libms to disagree about.
/// `crates/vike-chart/src/scale.rs`'s `log_nice_values` carries the recorded sweep.
///
/// Every occurrence across the scanned set is this exact form — no count is written here, because
/// a count of needles is the shape of claim that rots first. They are of two kinds and the
/// difference matters: in `crates/bridges/hyperliquid/src/px.rs` they build decade sweeps inside
/// the `#[cfg(test)]` module, so no shipped code reaches them; in
/// `crates/bridges/hyperliquid/src/instruments.rs` ([`REACHED_FILES`]) the exempted line IS the
/// production body of `pow10_neg`, which `round_size`'s grid witness calls. The exemption is
/// therefore load-bearing rather than cosmetic, and its whole justification is the BASE — the
/// rule ADR 0032 ended up with: a literal `10` or `2` needs nothing, an arbitrary runtime `f64`
/// base needs `libm::pow`. A `powf` there, or a `powi` on a computed base, is not covered by this
/// exemption and the gate will say so.
const POW10_EXEMPT: &str = "10f64.powi(";

/// The spelling every wire quantizer MUST contain — the string-mediated door into Decimal.
///
/// This is the NON-VACUITY arm, and it is the reason the gate cannot certify a file it never read.
/// A scan that opened the wrong path, or read an empty file, would report a confident zero banned
/// hits; requiring a POSITIVE hit makes that failure loud.
///
/// ⚠ It is required of [`WIRE_FILES`] ONLY, never of [`REACHED_FILES`] — the reason is on that
/// constant.
const REQUIRED_SPELLING: &str = "Decimal::from_str";

fn repo_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is `<root>/crates/vike-bridge-core`.
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

/// The wire quantizers must reach `Decimal` through a STRING, and must contain no platform-libm
/// call.
///
/// ⚠ **This gate deliberately bans its spellings across the WHOLE file — comments and
/// `#[cfg(test)]` modules included — where the three sibling gates exclude each `#[cfg(test)]`
/// item's line range.** The difference is a judgement about cost, and it is worth stating rather
/// than leaving as an inconsistency for someone to "fix":
///
/// * The sibling gates scan a whole `src/` tree, where test modules are the majority of the
///   fixture-building code and a total ban would be red on day one. This one scans a handful of
///   NAMED FILES ([`WIRE_FILES`] plus [`REACHED_FILES`]), where a total ban is green today
///   (verified needle by needle, 2026-08-29) and stronger: a test in one of these files that built
///   its expectation with `Decimal::from_f64` would be asserting the wrong construction, so
///   banning it there is a feature.
/// * The range walk is a ~90-line machine whose own correctness needs a synthetic-fixture test to
///   prove its cut (`the_test_module_cut_excludes_only_the_test_module`, in each of the three
///   siblings). A fourth copy of it, to cover this few files, would be more machinery than subject.
///
/// The cost of the choice is real and small: prose in a scanned file may not NAME a banned
/// spelling. This probe is not scanned, so prose about them belongs here — which is where
/// [`BANNED_SPELLINGS`] states each family's argument.
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
