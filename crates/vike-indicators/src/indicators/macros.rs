//! `hist_indicator!` — generates a full [`crate::Indicator`] impl that streams by
//! history-recompute (see [`super::stream_tail`]). Correct-by-construction for any
//! **causal** indicator, so `on_bar`-fold == `vectorize` bit-for-bit (the parity
//! gate). Removes the per-indicator boilerplate for the ~90 window/recurrence
//! ports whose recurrence is not cleanly incremental.
//!
//! All params are stored as `f64` and read positionally from the coerced slice
//! (`with_params`); the batch body converts to `usize` where needed
//! (`p.round() as usize`). The `batch` body is an expression over `bars: &[Bar]`
//! plus each param name bound as an `f64` local, returning `Vec<Vec<f64>>` (one
//! line per output, each the input length). `arity` is the output-line count.

/// See the module docs. Usage:
/// ```ignore
/// hist_indicator! {
///     /// doc
///     Mom, "mom", 1, [period = 10.0],
///     |bars| batch_mom(bars, period.round() as usize)
/// }
/// ```
macro_rules! hist_indicator {
    // Arm 1: no warm-up clause -> the trait's conservative `lookback() == 0` default.
    (
        $(#[$m:meta])*
        $Name:ident, $key:literal, $arity:expr, [ $( $pf:ident = $pdef:expr ),* $(,)? ],
        |$bars:ident| $body:expr
    ) => {
        hist_indicator! {
            @impl
            $(#[$m])*
            $Name, $key, $arity, [ $( $pf = $pdef ),* ],
            |$bars| $body,
            lookback = 0, full = 0, exact = false
        }
    };
    // Arm 2: an EXACT warm-up expression over the same param locals (each bound as
    // an `f64`), PLUS an explicit `full =` for multi-line indicators whose lines
    // warm up at different indices (`lookback` = first line, `full` = ALL lines).
    (
        $(#[$m:meta])*
        $Name:ident, $key:literal, $arity:expr, [ $( $pf:ident = $pdef:expr ),* $(,)? ],
        |$bars:ident| $body:expr,
        lookback = $lb:expr, full = $fl:expr
    ) => {
        hist_indicator! {
            @impl
            $(#[$m])*
            $Name, $key, $arity, [ $( $pf = $pdef ),* ],
            |$bars| $body,
            lookback = $lb, full = $fl, exact = true
        }
    };
    // Arm 3: an EXACT warm-up expression; every output line warms at the same index
    // (`full` == `lookback`).
    (
        $(#[$m:meta])*
        $Name:ident, $key:literal, $arity:expr, [ $( $pf:ident = $pdef:expr ),* $(,)? ],
        |$bars:ident| $body:expr,
        lookback = $lb:expr
    ) => {
        hist_indicator! {
            @impl
            $(#[$m])*
            $Name, $key, $arity, [ $( $pf = $pdef ),* ],
            |$bars| $body,
            lookback = $lb, full = $lb, exact = true
        }
    };
    // Internal expansion arm (all public arms funnel here).
    (
        @impl
        $(#[$m:meta])*
        $Name:ident, $key:literal, $arity:expr, [ $( $pf:ident = $pdef:expr ),* $(,)? ],
        |$bars:ident| $body:expr,
        lookback = $lb:expr, full = $fl:expr, exact = $exact:expr
    ) => {
        $(#[$m])*
        #[derive(Clone)]
        pub struct $Name {
            $( $pf: f64, )*
            hist: Vec<vike_model::Bar>,
            last: Vec<f64>,
        }
        impl $Name {
            pub fn new() -> Self {
                Self { $( $pf: $pdef, )* hist: Vec::new(), last: vec![f64::NAN; $arity] }
            }
            #[allow(unused_variables, unused_mut, unused_assignments)]
            pub fn with_params(p: &[f64]) -> Self {
                let mut s = Self::new();
                let mut i = 0usize;
                $( s.$pf = p.get(i).copied().unwrap_or($pdef); i += 1; )*
                s
            }
        }
        impl Default for $Name {
            fn default() -> Self {
                Self::new()
            }
        }
        impl $crate::Indicator for $Name {
            fn on_bar(&mut self, bar: &vike_model::Bar) -> Vec<f64> {
                self.hist.push(bar.clone());
                // Drop history the kernel provably cannot read. Without this the buffer grows
                // forever and every bar re-runs the batch over ALL of it, so streaming n bars costs
                // O(n^2): MEASURED before this line existed, `adx` went 36ms -> 144ms -> 594ms as
                // the series doubled 2k -> 4k -> 8k (4.1x per doubling), and `connors_rsi` cost
                // 2,388ms for one 4,000-bar pass. A 300k-bar chart could not build one at all.
                //
                // `lookback_full()` is the warm-up index: the bar at which the LAST output line
                // first lands, expressed in this indicator's own params. A kernel whose value at
                // bar i is a function of a bounded window cannot read further back than that, so
                // trimming to it is value-preserving — and BIT-preserving, which is the only kind
                // that counts here: the retained tail is a contiguous slice of the same bars in the
                // same order, so the kernel performs the identical float operations in the
                // identical sequence. `tests/parity.rs` re-proves that per indicator by folding
                // `on_bar` and comparing against `vectorize` bit-for-bit.
                //
                // ⚠ This is sound ONLY for kernels with a bounded window. A path-dependent
                // recurrence — the set `crate::indicators::is_path_dependent` names — reads ALL
                // history since the mount, so truncating one silently changes its value.
                //
                // ⚠⚠ This comment used to assert: "every such indicator is a hand-written `impl
                // Indicator`; none is generated by this macro." **That was FALSE for six of the
                // nine, and the drain below was corrupting all six.** `psar`/`vwap`/`obv` really
                // are hand-written, but `mcginley` (`crates/vike-indicators/src/indicators/
                // overlap.rs`'s `Mcginley`) and the cumulative volume series
                // `ad`/`nvi`/`pvi`/`pvt`/`net_volume` (`crates/vike-indicators/src/indicators/
                // volume.rs`'s `Ad`, `Nvi`, `Pvi`, `Pvt`, `NetVolume`) are ALL generated right here.
                // Their `lookback_full` is small or zero, so `keep` fell to `KEEP_FLOOR` and past
                // 512 bars each began recomputing a cumulative sum from a truncated start — a
                // silently wrong number on every chart longer than that, which is every real one.
                //
                // The parity gate could not see it: `tests/parity.rs` runs at most 400 bars, and the
                // drain first fires at `keep * 2` >= 512, so the trim never executed under test. A
                // gate that is green because it never reaches the code is the failure mode this
                // repo has hit before, so `path_dependent_indicators_survive_a_series_past_the_trim`
                // now runs past the threshold deliberately.
                //
                // So the drain now ASKS rather than a comment asserting. `warmup_path_dependent` is
                // the predicate this macro already generates below, forwarding to
                // `is_path_dependent` — one authority, consulted instead of restated. The exempt six
                // keep unbounded history and with it their O(n^2) streaming cost; that is the honest
                // trade, because they need full history BY DEFINITION. Making them cheap means
                // rewriting each as a bounded-state impl like `Obv` — a per-indicator change, not a
                // macro one.
                //
                // The `+ 1` is the current bar. The factor covers kernels that reach beyond their
                // declared warm-up for a difference or a previous close (`tr[i]` reads `c[i-1]`).
                //
                // ⚠ **`KEEP_FACTOR` is 64 because a SMOOTHED kernel never fully forgets, and the
                // old 16 was below the threshold where that stops mattering.** An EMA- or
                // Wilder-smoothed kernel has an INFINITE impulse response: every bar since the
                // mount still contributes, decaying by `(1 - alpha)` per bar. Truncation is
                // bit-exact only once the dropped tail's weight falls under one ulp:
                //
                //     (1 - alpha)^(F*n) < 2^-53      =>      F * n * alpha > 53*ln2 = 36.7
                //
                //   EMA     alpha = 2/(n+1)  ->  2F > 36.7  =>  F > 18.4   (`apo`, `ppo`, `trix`)
                //   Wilder  alpha = 1/n      ->   F > 36.7  =>  F > 36.7   (`smma`, `natr`, `asi`)
                //
                // Wilder decays HALF as fast, so it sets the bound — which is why 32 was not
                // enough either. `apo` diverged at bar 801 at F=16, and four Wilder-family
                // indicators (`smma`, `natr`, `supertrend`, `chande_kroll_stop`) still diverged at
                // F=32; all five are bit-exact at F=64, verified at 5000 bars where their drains
                // genuinely fire. Because the bound is a fixed multiple of the PERIOD rather than
                // of the series, it does not decay again at 100k bars.
                //
                // ⚠⚠ **This comment used to claim F=64 also fixed `eom` and `stochf`. It did NOT —
                // it HID them.** Neither contains an exponential term at all: `volume.rs`'s
                // `batch_eom` ends in `smooth_defined(&raw, sma, period)` and `momentum.rs`'s
                // `batch_stochf` computes %D as `smooth_defined(&k_line, sma, d)`, and
                // `math.rs`'s `sma` WAS a SLIDING-WINDOW ACCUMULATOR. Its error was a random walk
                // rather than a decaying tail, so `(1-alpha)^(F*n) < 2^-53` had nothing to act on
                // and NO factor fixed it. Raising F merely moved their drain from bar 898/962 to
                // 1794/1922 — just past the 1200-bar gate that was measuring them.
                //
                // ⚠ That is also why the "F=32 left 8, F=64 leaves 2" measurement was confounded:
                // at 1200 bars the drain fires only for `lookback_full <= 9`, so raising the factor
                // REMOVED 28 indicators from the tested population rather than fixing them.
                // `parity.rs`'s `every_trimmed_indicator_is_truncation_invariant` now asks each
                // kernel the drain's actual precondition instead of hoping a length reaches it.
                //
                // ⚠⚠⚠ **The accumulators are GONE, and that is what makes the SECOND retention
                // family below sound.** `crates/vike-indicators/src/math.rs`'s `sma` and the
                // `run_*` pairs in `crates/vike-indicators/src/indicators/statistics.rs`'s
                // `batch_var`, `crates/vike-indicators/src/indicators/volatility.rs`'s
                // `stddev_series`, `crates/vike-indicators/src/indicators/overlap.rs`'s
                // `batch_vwma` and `crates/vike-indicators/src/indicators/volume.rs`'s `batch_cmf`
                // now RECOMPUTE their window, so those kernels are pure functions of
                // a bounded window and `NOT_TRUNCATION_INVARIANT` is empty. De-accumulating alone
                // would have been a PESSIMISATION, though: at `keep = 64 * lookback_full` a window
                // fold costs O(keep * period) per bar, MEASURED at 1.1x-36x main's `on_bar` (vwma
                // at `period = 400`: 56us -> 2.03ms per bar, i.e. 3.4s -> 121s to stream 60k
                // bars). The 64x factor exists for the IIR family and for it alone; a genuinely
                // finite-window kernel needs `lookback_full + 1` bars, which is what
                // `$crate::WindowReach::Finite` grants.
                //
                // ⚠ **Where BOTH halves apply the result is faster; where only the first does, it is
                // MUCH slower — and this crate deliberately contains both cases.** For the `Finite`
                // grants: stddev p=20 10.4us -> 1.11us/bar, trima p=20 8.8us -> 0.78us. But four
                // rows (`hvol`, `kst`, `relative_volatility`, `stochrsi`) take the de-accumulation
                // and are DENIED the retention shrink, because a window over NaN-compacted values
                // can reach unboundedly far back when the source has interior gaps. They therefore
                // do strictly more work over an identical retained history: `stddev_series` at
                // `relative_volatility`'s retention is 3.3x at its DEFAULT period 14 and 37-53x at
                // period 200 (121us -> 6.4ms per streamed bar). That is the measured price of
                // withholding the shrink, and it is the right price against a silently wrong
                // number — but it is a REGRESSION, not a speed-up, and the chart pays it per FRAME.
                //
                // Nor is the win uniform among the grants: `vwma` at its registry maximum period 400
                // measured 0.75x and 1.87x on two consecutive interleaved-median runs, because
                // main's arm is memory-bandwidth-bound while this one is L1-resident. Treat p=400 as
                // break-even, not as a gain.
                //
                // That retention is ALSO the largest lever on the chart, which does not call
                // `vectorize` at all: `crates/vike-chart/src/indicators.rs`'s `Active` previews the
                // forming bar on a `clone_box()` EVERY FRAME (in its `update`), so `keep` is a
                // per-frame deep copy of `hist: Vec<Bar>` — 4.6 MB and 1006 us/frame at vwma
                // `period = 400`, against 7.8 us at the finite retention.
                //
                // `$crate::keep_for` — promoted out of this macro so `parity.rs`'s
                // truncation-invariance gate computes the SAME retained length the drain does,
                // instead of hard-coding a copy that would drift the moment this changed. It takes
                // the indicator's OWN `window_reach` for the same reason: a gate that re-derived
                // which family this is would be testing its own copy of the answer.
                let keep = $crate::keep_for(
                    $crate::Indicator::lookback_full(self),
                    $crate::Indicator::window_reach(self),
                );
                // Amortised: only compact once the buffer reaches twice the bound, so the O(len)
                // `drain` runs once per `keep` bars rather than on every bar.
                if !$crate::Indicator::warmup_path_dependent(self)
                    && self.hist.len() >= keep.saturating_mul(2)
                {
                    let cut = self.hist.len() - keep;
                    self.hist.drain(..cut);
                }
                $( let $pf = self.$pf; )*
                let out = $crate::indicators::stream_tail(
                    &self.hist,
                    |$bars: &[vike_model::Bar]| $body,
                    $arity,
                );
                self.last = out.clone();
                out
            }
            fn vectorize(&self, bars: &[vike_model::Bar]) -> Vec<Vec<f64>> {
                $( let $pf = self.$pf; )*
                let $bars = bars;
                $body
            }
            fn value(&self) -> Vec<f64> {
                self.last.clone()
            }
            fn reset(&mut self) {
                self.hist.clear();
                self.last = vec![f64::NAN; $arity];
            }
            fn name(&self) -> &str {
                $key
            }
            #[allow(unused_variables)]
            fn lookback(&self) -> usize {
                $( let $pf = self.$pf; )*
                $lb
            }
            #[allow(unused_variables)]
            fn lookback_full(&self) -> usize {
                $( let $pf = self.$pf; )*
                $fl
            }
            fn lookback_exact(&self) -> bool {
                $exact
            }
            fn warmup_path_dependent(&self) -> bool {
                crate::indicators::is_path_dependent($key)
            }
            fn window_reach(&self) -> $crate::WindowReach {
                // ⚠ The name-keyed table answers first, and for one family it CANNOT answer alone:
                // `WindowReach::SmoothedOver(period)` carries a runtime PARAMETER, and
                // `crate::indicators::window_reach` only sees a `&str`. So a row that wants the
                // decay bound returns a placeholder period of 0 there, and the real period is
                // filled in HERE, where this indicator's own params are in scope — the same shape
                // `lookback`/`lookback_full` already use.
                //
                // A `SmoothedOver(0)` that reached `keep_for` would floor to `KEEP_FLOOR` and
                // silently under-retain, so `smoothing_period` is REQUIRED to answer for every key
                // the table marks that way; `every_smoothed_over_row_names_its_period` gates it.
                match crate::indicators::window_reach($key) {
                    $crate::WindowReach::SmoothedOver(_) => {
                        $( let $pf = self.$pf; )*
                        $crate::WindowReach::SmoothedOver(
                            crate::indicators::smoothing_period($key, &[$( $pf ),*]),
                        )
                    }
                    other => other,
                }
            }
            fn trims_history(&self) -> bool {
                true
            }
        }
    };
}

pub(crate) use hist_indicator;
