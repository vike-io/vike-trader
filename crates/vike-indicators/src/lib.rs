//! vike-indicators — the Qt-free indicator framework. One [`Indicator`] trait with
//! a streaming `on_bar` (the live/event-engine path — one bar at a time, no
//! lookahead) and a batch `vectorize` (the vector-engine/chart path) that are ONE
//! source of truth: for every indicator, `on_bar` folded over a series equals
//! `vectorize` on that series, bit-for-bit (gated in `tests/parity.rs`). Faithful
//! f64 port of the whole vike-trader-app `core/indicators/*.py` catalog — the
//! **171** single-series indicators across 8 categories (overlap/momentum/
//! volatility/volume/statistics/structure/price/pattern; see [`registry`]) plus
//! the **6** 2-series (benchmark) indicators behind the separate [`pairs`] seam
//! (177 total). Depends on vike-model, plus `libm` for the transcendentals —
//! see the crate manifest for why a PLATFORM libm must not be used here.
//!
//! CONTRACT:
//! - `vectorize` is the reference kernel (a faithful line-for-line port of the
//!   Python function). Warm-up positions emit `f64::NAN`; fold order (naive left
//!   folds) is load-bearing. ⚠ A bounded-window kernel folds its window and
//!   carries NOTHING across bars — see [`WindowReach`], which is the property
//!   `hist_indicator!`'s history drain depends on and which the sliding
//!   accumulators this crate used to run (`sum += c[i] - c[i-n]`, `run_sum`/
//!   `run_sum2` pairs) silently violated.
//! - `on_bar` MUST match `vectorize` bit-for-bit. Simple recurrences use bounded
//!   O(1)/O(window) state; the rest stream by history-recompute (`stream_tail`),
//!   which is correct-by-construction for causal indicators (vike-indicators is
//!   NOT the vike-core hot path).
//! - Three indicators read future bars (ichimoku's chikou, zigzag,
//!   williams_fractal) and are flagged [`registry::IndicatorMeta::batch_only`]:
//!   `vectorize` is authoritative, `on_bar` is a causal best-effort, and the
//!   `on_bar == vectorize` gate skips them.
//! - Never widen a float tolerance to make a test pass — compare via `f64::to_bits`
//!   and treat both-NaN as equal.
//! - [`Indicator::lookback`] reports warm-up depth (the first non-NaN `vectorize`
//!   index) so a caller can size its seed history by name+params — via
//!   [`registry::lookback`] / [`IndicatorMeta::lookback`], and
//!   [`pairs::pair_lookback`] for the pair seam. It is a CONSERVATIVE lower bound:
//!   105 of the 171 single-series indicators (and 4 of the 6 pairs) carry a real
//!   param-derived override flagged [`Indicator::lookback_exact`] and gated `==`;
//!   the rest (the 63 candlestick patterns, whose signal series is data-dependent
//!   and never NaN, plus the batch-only/structure series) keep the `0` default and
//!   are gated `>=`. `tests/lookback.rs` is that gate and prints measured values so
//!   coverage can be ratcheted.
//! - A SECOND seam sits beside the indicator one: [`feature::ColumnFeature`], the same
//!   streaming-equals-batch contract over a raw numeric COLUMN rather than over a `Bar`. It exists
//!   for the study→strategy handoff — a model feature computed one row at a time by a live
//!   strategy must equal the matrix column its model trained on — and it buys the property by
//!   CONSTRUCTION rather than by convention: an implementor writes `vectorize` only, and
//!   [`feature::FeatureStream`] IS that kernel over a bounded tail, so there is no second
//!   arithmetic to drift. The one declaration left to get wrong is
//!   [`feature::ColumnFeature::reach`], and `test_support` (the `test-support` feature) is the
//!   harness that gates it — for this crate's own features in `tests/feature_parity.rs`, and for a
//! - A THIRD seam sits above both: [`frame::Frame`], the validated `(asset, timestamp)` PANEL, and
//!   [`rolling`], the blocks that apply a [`feature::ColumnFeature`] to every column of one. The
//!   panel is what [`window::per_group`] needs and could not previously be handed: an index sorted
//!   by `(asset, ts)` and strictly increasing inside an asset, from which the per-asset row ranges
//!   are DERIVED — so a window reaching across an asset boundary is unrepresentable rather than
//!   discouraged. Its timestamps are opaque integers and its grid ([`frame::Frame::on_grid`]) is a
//!   caller-named cadence in the caller's own unit, so this crate names no time base of its own.
//! - [`Indicator::lookback_full`] is the ALL-LINES twin
//!   (the number to size seed history with on multi-line indicators), and
//!   [`Indicator::warmup_path_dependent`] flags the cumulative/path-dependent series
//!   whose truthful `lookback() == 0` does NOT mean "warm".

use vike_model::Bar;

pub mod feature;
pub mod frame;
mod indicators;
mod math;
pub mod pairs;
pub mod registry;
pub mod rolling;
pub mod window;

// The STUDY↔STRATEGY parity harness. Behind `test-support` so a shipped build compiles NONE of it —
// the `vike-ml`/`vike-data` model. It is a LIBRARY module rather than a `tests/` file because the
// feature definitions it exists to check do not live in this repository: per
// `docs/superpowers/specs/2026-08-24-research-engine-user-split-design.md` (R1) a study's own
// source is the author's and sits in `user_data/`, so a test binary here could never run on it.
// `crates/vike-indicators/tests/feature_parity.rs` is one caller; the study is another.
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;

// ⚠ No `pub use indicators::{…}` here, and the absence is deliberate. `mod indicators` is PRIVATE
// and its concrete kernel types are reached through [`registry`] — `make`/`make_with` hand back a
// `Box<dyn Indicator>` — never by name. A crate-root re-export of `indicators/mod.rs`'s
// `pub use base::{…}` line, copied identifier-for-identifier, stood here with ZERO users at either
// spelling, in or out of this crate; every kernel family added after `base.rs` (momentum, overlap,
// patterns, statistics, structure, volatility, volume) was never added to it, so it named the
// original set and nothing since. In-crate construction goes through `registry.rs`'s
// `use crate::indicators::*;`, which resolves against the private module and never saw this line.
// Re-export a kernel type here only when a caller needs to NAME it — and say which caller.
// ⚠ No crate-root re-export of [`feature`]'s types, deliberately, and the precedent is
// [`window::WindowSpec`] rather than [`Indicator`]: a module that carries its own vocabulary is
// named through its module (`vike_indicators::feature::ColumnFeature`), which is how the one
// existing out-of-crate consumer of `window` spells it. Add one here only when a caller needs the
// shorter path — and say which caller.
pub use pairs::{pair_lookback, pair_make, pair_make_with, pair_registry, PairIndicator, PairMeta};
pub use registry::{
    coerce, get, lookback, lookback_full, make, make_with, registry, Category, IndicatorMeta,
    OutSpec, OutputStyle, ParamSpec, RenderKind, UserFactory,
};

/// How far back a batch kernel's value at bar `i` can actually reach — the ONE fact
/// `hist_indicator!`'s history drain sizes retained history from.
///
/// The distinction is not "does this indicator have a period"; it is **what arithmetic produces
/// the value**. A kernel that folds a bounded window and carries nothing across bars is a pure
/// function of that window, so a retained tail containing the window reproduces its bits exactly.
/// A kernel with an exponential/Wilder recurrence keeps a decaying contribution from every bar
/// since the mount, so it needs a retention sized against that decay instead.
///
/// ⚠ **The classification is STRUCTURAL, and it must be.**
/// `crates/vike-indicators/src/indicators/mod.rs`'s `window_reach` is the authority and carries
/// the per-name evidence.
/// `parity.rs`'s `every_trimmed_indicator_is_truncation_invariant` is a BACKSTOP, not the
/// authority: it was MEASURED that `stochrsi` looks invariant from 30 retained bars on that
/// gate's own 4,000-bar series while its Wilder `rsi_vals` sub-kernel needs ~493 bars on every
/// data shape tried — so a wrong `Finite` declaration for it would be greenlit. That is the
/// `declaration-pinning-tests-dont-gate` failure mode this repo has already hit; the cure is that
/// the declaration cites the kernel, not the test.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowReach {
    /// The value at bar `i` is a fold over a FIXED COUNT of consecutive bars ending at `i`, and
    /// nothing is carried between bars. Retention needs the window plus slack, nothing more.
    Finite,
    /// An infinite impulse response whose SMOOTHING PERIOD is known — retention is bounded by the
    /// decay of that period rather than by a blanket multiple of `lookback_full`.
    ///
    /// ⚠ The distinction is worth a variant because `lookback_full` is the WRONG quantity to scale
    /// an IIR bound by, and it is wrong in the expensive direction. `relative_volatility` declares
    /// `lookback_full = 2p - 2`, so at its default `p = 14` the blanket rule retains
    /// `64 * 26 + 1 = 1665` bars — while the decay it actually needs is over `p`, measured stable
    /// from **305** bars (analytic EMA bound `18.4 * 14 = 258`). `stochrsi` is the same shape: 1857
    /// retained against a Wilder `rsi_vals` sub-kernel measured to need ~493.
    ///
    /// The factor is [`SMOOTHED_FACTOR`], sized for WILDER (`alpha = 1/n`, the slower decay) so one
    /// number is safe for both families — see [`keep_for`].
    ///
    /// ⚠ NOT for a kernel whose reach is unbounded for a NON-decay reason. `hvol` and `kst` fold
    /// over `smooth_defined`'s DEFINED values, so a single non-positive close creates an interior
    /// gap and the window spans arbitrarily many bars. No decay bound helps that, and they keep
    /// [`WindowReach::Smoothed`].
    SmoothedOver(usize),
    /// The value carries an infinite impulse response whose period this crate cannot name, or
    /// reaches a variable distance back (a window over NaN-COMPACTED values can span arbitrarily
    /// many bars when the source has interior gaps). Retention is the conservative
    /// [`KEEP_FACTOR`] multiple.
    ///
    /// This is the DEFAULT, and deliberately: over-retaining costs memory and streaming time,
    /// under-retaining silently returns a different number.
    Smoothed,
}

/// `hist_indicator!`'s retained-history multiple for the [`WindowReach::Smoothed`] family, and its
/// floor — promoted out of the macro so a TEST can compute the same `keep` the drain does.
///
/// A gate that hard-coded these would drift the moment the macro changed; a gate that reads them
/// asks the real question. `crates/vike-indicators/src/indicators/macros.rs` is where they are
/// USED, and its comment carries the derivation (`F*n*alpha > 53*ln2` — Wilder sets the bound).
pub const KEEP_FACTOR: usize = 64;
/// See [`KEEP_FACTOR`].
pub const KEEP_FLOOR: usize = 256;

/// Extra bars retained beyond a [`WindowReach::Finite`] kernel's own warm-up depth.
///
/// A finite kernel needs `lookback_full + 1` bars and not one more, so this is pure margin against
/// a `lookback_full` that under-reports by a bar or two (`tr[i]` reading `c[i-1]`, `dpo`'s
/// displacement, `eom`'s NaN at index 0). It is NOT margin against an unbounded reach — a kernel
/// that can reach an unbounded distance is [`WindowReach::Smoothed`] by definition, and no slack
/// is the right amount of slack for "unbounded".
/// The multiple of a SMOOTHING period [`WindowReach::SmoothedOver`] retains.
///
/// `37`, from the Wilder bound `F * n * alpha > 53*ln2` with `alpha = 1/n` (`F > 36.7`). EMA
/// (`alpha = 2/(n+1)`) needs only `> 18.4`, so one number covers both families. Measured margin at
/// the two rows that use it: `37 * 14 = 518` retained against `relative_volatility`'s 305 and
/// `stochrsi`'s ~493.
pub const SMOOTHED_FACTOR: usize = 37;

pub const FINITE_SLACK: usize = 32;
/// See [`FINITE_SLACK`]. The floor matters for the paramless/shallow kernels (`ac`'s
/// `lookback_full` is a constant 37, `trima` at `period = 1` is 0) where the slack alone would be
/// a very small buffer to amortise the drain over.
pub const FINITE_FLOOR: usize = 64;

/// The retained-history length `hist_indicator!` drains to, for an indicator whose last output line
/// warms at `lookback_full` and whose kernel reaches back as far as `reach` says. The drain fires
/// once the buffer reaches `2 * keep`, so a retained tail is always in `keep ..= 2*keep - 1`.
///
/// ⚠ **Callers pass the indicator's OWN `reach`, never a reconstructed policy.** `parity.rs` and
/// the drain must compute the same number, so both read [`Indicator::window_reach`]; a gate that
/// re-derived which family an indicator belongs to would be testing its own copy of the answer.
pub fn keep_for(lookback_full: usize, reach: WindowReach) -> usize {
    match reach {
        // The decay bound, over the SMOOTHING period rather than the warm-up depth. Sized for
        // Wilder (`alpha = 1/n`, so `F * n * alpha > 53*ln2` gives `F > 36.7`) because that is the
        // slower of the two families and one safe number beats two that must be kept straight.
        WindowReach::SmoothedOver(period) => period
            .saturating_mul(SMOOTHED_FACTOR)
            .saturating_add(1)
            .max(KEEP_FLOOR)
            // ⚠ Never MORE than the blanket rule would have retained: this variant exists to
            // shrink retention, and a deep `lookback_full` with a shallow smoothing period must not
            // make it grow.
            .min(lookback_full.saturating_mul(KEEP_FACTOR).saturating_add(1).max(KEEP_FLOOR)),
        WindowReach::Smoothed => {
            lookback_full.saturating_mul(KEEP_FACTOR).saturating_add(1).max(KEEP_FLOOR)
        }
        WindowReach::Finite => {
            lookback_full.saturating_add(FINITE_SLACK).saturating_add(1).max(FINITE_FLOOR)
        }
    }
}

/// Object-safe state clone for boxed indicators. Lets a caller evaluate a
/// speculative bar — e.g. the live *forming* bar, which mutates in place and
/// which a streaming fold cannot un-see — on a throwaway copy, without
/// advancing the persistent instance. Blanket-implemented for every `Clone`
/// indicator; `Indicator: BoxedClone` makes cloneability part of the contract
/// (indicator state is plain numeric data, so `#[derive(Clone)]` suffices).
pub trait BoxedClone {
    /// An independent copy of the full streaming state as a fresh boxed trait
    /// object. Feeding the copy never affects the original (and vice versa).
    fn clone_box(&self) -> Box<dyn Indicator>;
}

impl<T: Indicator + Clone + 'static> BoxedClone for T {
    fn clone_box(&self) -> Box<dyn Indicator> {
        Box::new(self.clone())
    }
}

/// A technical indicator with a unified streaming + batch interface. The two
/// paths are one source of truth (see the crate-level contract).
pub trait Indicator: BoxedClone + Send + Sync {
    /// Streaming: feed the next bar, advance internal state, and return this bar's
    /// output(s) — one value per output line (e.g. MACD → 3). Warm-up bars return
    /// `f64::NAN`. No lookahead: this is what the event engine and live path use.
    fn on_bar(&mut self, bar: &Bar) -> Vec<f64>;

    /// Batch: compute the whole series at once — one `Vec<f64>` per output line.
    /// This is the vector engine / chart path and the parity reference.
    fn vectorize(&self, bars: &[Bar]) -> Vec<Vec<f64>>;

    /// The most recent `on_bar` output without advancing (same shape as `on_bar`).
    fn value(&self) -> Vec<f64>;

    /// Clear all streaming state back to construction.
    fn reset(&mut self);

    /// The registry key (e.g. "macd").
    fn name(&self) -> &str;

    /// Warm-up depth: the index of the FIRST bar at which `vectorize` produces a
    /// non-NaN value on **any** output line — i.e. the first bar at which this
    /// indicator says ANYTHING. `lookback() == 0` means "some line is defined from
    /// the very first bar".
    ///
    /// **This is NOT a seed size for a multi-line indicator.** On indicators whose
    /// lines warm at different indices (`adx`'s namesake line trails its `+DI`/`-DI`
    /// by `period - 1`; `macd`'s signal/hist trail the macd line; `stochastic`'s %D
    /// trails %K) reading `cols[i][lookback()]` yields NaN on the later lines. A
    /// caller sizing seed history so that EVERY line is valid must use
    /// [`Indicator::lookback_full`], which is `>= lookback()` and gated separately.
    ///
    /// CONTRACT (gated exhaustively in `tests/lookback.rs`):
    /// - This is a **conservative lower bound**. A real override returns the exact
    ///   index; the default returns `0`, which is trivially a lower bound for every
    ///   indicator. Callers may therefore always seed AT LEAST `lookback()` bars and
    ///   never over-seed, but must not assume a non-NaN value at exactly that index
    ///   unless the indicator is in the exact-override set
    ///   ([`registry::IndicatorMeta::lookback_exact`]).
    /// - Data-dependent indicators (candlestick patterns, pivot/structure series)
    ///   have no derivable warm-up and keep the `0` default.
    ///
    /// Additive: the default impl means no existing indicator is broken by this
    /// method's introduction.
    fn lookback(&self) -> usize {
        0
    }

    /// Full warm-up depth: the index of the first bar at which **every** output
    /// line of `vectorize` is non-NaN. This is the number a caller should use to
    /// size seed history — it is the one that makes `cols[line][lookback_full()]`
    /// valid for all lines. Always `>= lookback()`; equal for single-line
    /// indicators and for multi-line ones whose lines warm together.
    ///
    /// Exactness follows [`Indicator::lookback_exact`]: an exact indicator's
    /// `lookback_full` is gated `==` against the measured all-lines-defined index in
    /// `tests/lookback.rs`; the default (`0`) is gated `>=` like `lookback`.
    ///
    /// The default impl forwards to [`Indicator::lookback`] — correct for every
    /// single-line indicator, and the gate FAILS any staggered multi-line indicator
    /// that forgets to override it (so the default can never silently under-report).
    fn lookback_full(&self) -> usize {
        self.lookback()
    }

    /// `true` when the indicator's value depends on ALL history since it was
    /// mounted rather than on a bounded window — path-dependent recurrences with no
    /// derivable warm-up. ⚠ The membership is
    /// `crates/vike-indicators/src/indicators/mod.rs`'s `is_path_dependent` and NOT this sentence,
    /// which named eight of them while the set held eleven — `net_volume`, `asi` and `adosc` were
    /// missing, and `asi`/`adosc` joined in #1183 precisely because a name list was wrong.
    /// Their [`Indicator::lookback`] is a
    /// truthful `0` (they emit a number from bar one) but that number is NOT the
    /// same number a chart holding full history shows: `Psar` fabricates its initial
    /// trend, `Vwap` accumulates only from the current UTC day *as seen*, and the
    /// cumulative sums start from whatever bar the mount began at.
    ///
    /// A caller that seeds `lookback()` bars for one of these is NOT warm — seed as
    /// much history as is available (for `vwap`, at least back to the UTC-day
    /// boundary). Gated by name in `tests/lookback.rs`.
    /// True when this indicator streams by HISTORY-RECOMPUTE over a truncated tail — i.e. it was
    /// generated by `hist_indicator!`, which drains `hist` to [`keep_for`] and re-runs the batch
    /// kernel over what remains.
    ///
    /// A hand-written incremental impl (`Sma`, `Obv`, `Psar`, …) folds from bar 0 and never
    /// truncates, so the truncation-invariance question does not arise for it. This flag is what
    /// lets `parity.rs`'s `every_trimmed_indicator_is_truncation_invariant` ask ONLY the
    /// indicators the drain can actually reach, rather than a hand-kept name list — the shape of
    /// list that was wrong in #1183 and is the reason that gate exists.
    fn trims_history(&self) -> bool {
        false
    }

    /// How far back this indicator's batch kernel can reach — see [`WindowReach`]. Read by
    /// [`keep_for`], and by nothing else: it exists so the drain and its gate share one answer.
    ///
    /// The default is the conservative [`WindowReach::Smoothed`], so an indicator that never
    /// declares anything retains the large multiple. A hand-written incremental impl never trims
    /// at all, so the value is unused for it.
    fn window_reach(&self) -> WindowReach {
        WindowReach::Smoothed
    }

    fn warmup_path_dependent(&self) -> bool {
        false
    }

    /// `true` when [`Indicator::lookback`] is a REAL, param-derived override whose
    /// value is the exact first-non-NaN index (gated `==` in `tests/lookback.rs`);
    /// `false` when it is the conservative `0` default (gated `>=`). Lets a caller
    /// distinguish "no warm-up" from "warm-up unknown" — and lets the gate ratchet
    /// coverage: flipping an indicator to exact makes the gate demand equality.
    fn lookback_exact(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod send_bound {
    use super::*;
    #[test]
    fn boxed_indicator_is_send() {
        fn assert_send<T: Send>() {}
        fn assert_sync<T: Sync>() {}
        assert_send::<Box<dyn Indicator>>(); // registry make_with returns this
        assert_sync::<Box<dyn Indicator>>(); // both halves of the trait's `Send + Sync` bound
        let ind = make_with("sma", &[3.0]).unwrap();
        std::thread::spawn(move || {
            let _ = ind;
        })
        .join()
        .unwrap(); // moves across threads
    }
}
