//! THE study↔strategy gate: for every general window feature this crate ships, the per-bar
//! (streaming) computation equals the batch (matrix) computation for the same row, BIT FOR BIT.
//!
//! This is `tests/parity.rs`'s claim on the OTHER seam. That file proves `on_bar == vectorize` for
//! the `Indicator` roster, which streams a `vike_model::Bar`; this one proves
//! `FeatureStream::push == ColumnFeature::vectorize` for the [`vike_indicators::feature`] seam,
//! which streams a raw numeric column — a funding rate, an open-interest series, a per-cohort
//! aggregate — because that is the shape a model feature actually has.
//!
//! # ⚠ The harness is NOT in this file
//!
//! It is `vike_indicators::test_support`, behind the `test-support` feature, and this file is one
//! CALLER of it. The reason is where the definitions live: `parity.rs` can iterate `registry()`
//! because this crate owns that roster, but a study's own features belong to whoever wrote the
//! study and — per `docs/superpowers/specs/2026-08-24-research-engine-user-split-design.md`'s R1 —
//! live in `user_data/`, outside this repository. A gate that could only ever run on in-tree
//! features would gate everything except the thing the handoff actually breaks on.
//!
//! # What this file gates, and why these features
//!
//! `vike_indicators::window`'s six kernels under both of `WindowSpec`'s presets, plus
//! `vike_indicators::rolling::LaggedDiff`. Those are the GENERAL features: the `window` module's
//! own doc states the registered `zscore`/`var` indicators and the research features "are presets
//! of the same code", the `point_in_time` preset it names as the model-feature one (`lag = 1`,
//! `ddof = 1`) is the preset `rolling`'s z-score and rank blocks take, and `LaggedDiff` is what its
//! delta block takes. Gating those three types here is what puts every panel block behind this
//! seam, because each block IS one of them applied per asset and holds no arithmetic of its own
//! (`rolling.rs`'s `the_blocks_are_the_column_features_they_name_and_nothing_else`).
//! No study-owned feature definition is reached into, imported or copied here.
//!
//! The second half of the file is the part that makes the first half worth anything: PLANTED
//! defects, each a `#[should_panic]`, proving the harness rejects the failure it claims to. A gate
//! whose non-vacuity is assumed is the failure mode this repository has already shipped.

use vike_indicators::feature::{ColumnFeature, FeatureStream, WindowFeature, WindowStat};
use vike_indicators::rolling::LaggedDiff;
use vike_indicators::test_support::{
    assert_feature_parity, assert_reach_is_tight, assert_stream_matches_batch,
    assert_stream_matches_batch_per_group, bits_eq, shaped_column,
};
use vike_indicators::window::WindowSpec;

/// Every kernel, and — for the one that takes a parameter beyond the window — every disposition of
/// it.
///
/// The three `ZScore` rows are not padding. `None` is the unclipped form; `Some(6.0)` is the
/// clipped form, the magnitude `crates/vike-indicators/src/rolling.rs`'s `Z_CLIP` carries — spelled
/// as a literal rather than imported, so this table stays a statement about the KERNEL under every
/// clip a caller might pass rather than about one caller's choice of clip; and
/// `Some(0.0)` is the degenerate bound, where the clip collapses every finite z to a signed zero
/// and the `-0.0`/`+0.0` distinction is the only thing left in the bits. A streaming path that
/// reconstructed the clip rather than reusing the kernel is most likely to differ exactly there.
const STATS: &[WindowStat] = &[
    WindowStat::Mean,
    WindowStat::Var,
    WindowStat::Std,
    WindowStat::RankPct,
    WindowStat::Median,
    WindowStat::ZScore { clip: None },
    WindowStat::ZScore { clip: Some(6.0) },
    WindowStat::ZScore { clip: Some(0.0) },
];

/// The periods run, chosen to span the failure modes rather than to be round.
///
/// `2` is the shallowest window a variance is defined over at `ddof = 1` (at `period = 1` the
/// sample denominator is zero and every kernel answers NaN — covered separately below). `3` keeps a
/// window where an off-by-one in the reach is arithmetically obvious. `20` is the `stddev`
/// indicator's own default. `168` is a week of hourly bars: it is the DEEP case, and it is the one
/// that matters, because a reach bug is invisible until the series is long enough to truncate — the
/// property `parity.rs` records as "it passed by never reaching the code".
const PERIODS: &[usize] = &[2, 3, 20, 168];

/// ⚠ **THE GATE.** Every general window feature, both presets, every shape, streaming == batch.
#[test]
fn every_window_feature_streams_exactly_as_it_batches() {
    let mut checked = 0usize;
    for &stat in STATS {
        for &period in PERIODS {
            for spec in [WindowSpec::indicator(period), WindowSpec::point_in_time(period)] {
                let f = WindowFeature::new(stat, spec);
                let label = format!("{stat:?}/p{period}/lag{}", spec.lag);
                assert_feature_parity(&label, &f);
                checked += 1;
            }
        }
    }
    assert_eq!(
        checked,
        STATS.len() * PERIODS.len() * 2,
        "the matrix collapsed — a gate that iterates nothing passes forever"
    );
}

/// The lags [`vike_indicators::rolling::delta_block`] is asked for by its in-tree caller, plus the
/// degenerate `0`.
///
/// `0` is here because it is the one lag at which the feature reads a single row and
/// `assert_reach_is_tight` therefore has nothing to say — it returns without asserting — so a
/// mistake there would be invisible to the tightness half and is caught only by parity.
const LAGS: &[usize] = &[0, 1, 4, 12, 24, 72];

/// ⚠ **The second general column feature, and the one that is NOT a window preset.**
///
/// [`vike_indicators::rolling::LaggedDiff`] reads exactly two rows and answers a number when the
/// rows BETWEEN them are missing, which is precisely what a window preset cannot do — `min_periods`
/// blanks the whole window on one interior NaN. That difference is why it has its own impl rather
/// than being bent into a [`WindowFeature`], and it is why running the harness over it is not
/// redundant with the matrix above: the `with-gaps` shape drives a branch no `WindowStat` reaches
/// the same way.
#[test]
fn the_lagged_difference_streams_exactly_as_it_batches() {
    let mut checked = 0usize;
    for &lag in LAGS {
        assert_feature_parity(&format!("lagged_diff/lag{lag}"), &LaggedDiff::new(lag));
        checked += 1;
    }
    assert_eq!(checked, LAGS.len(), "the loop collapsed — a gate that iterates nothing passes");
}

/// `lag + 1`, asserted directly — the same shape as the window reach check below, and cheap.
#[test]
fn the_lagged_differences_declared_reach_is_lag_plus_one() {
    for &lag in LAGS {
        assert_eq!(LaggedDiff::new(lag).reach(), lag + 1);
    }
}

/// The panel twin for the lagged difference, over group lengths that are not multiples of any lag.
///
/// A lagged difference is the CHEAPEST thing to get wrong across an instrument boundary — it is a
/// one-row reach at `lag = 1`, and it would be wrong exactly once per boundary, which is the kind
/// of error that survives every eyeball.
#[test]
fn a_stacked_panel_of_lagged_differences_agrees_with_a_stream_reset_at_each_instrument() {
    let x = shaped_column("high-vol", 2_003);
    let groups = [0..701, 701..1_303, 1_303..x.len()];
    for &lag in LAGS {
        assert_stream_matches_batch_per_group(
            &format!("lagged_diff/lag{lag}"),
            &LaggedDiff::new(lag),
            &x,
            &groups,
        );
    }
}

/// The reach declaration, asserted directly rather than only through its consequences.
///
/// `period + lag` is the claim `WindowFeature::reach`'s doc derives from `window_at`; this is that
/// derivation checked against the code instead of against the comment. It is cheap and it fails
/// with a readable message, where the parity gate above would fail with a row index.
#[test]
fn the_declared_reach_is_period_plus_lag() {
    for &period in PERIODS {
        assert_eq!(WindowFeature::indicator(WindowStat::Mean, period).reach(), period);
        assert_eq!(WindowFeature::point_in_time(WindowStat::Mean, period).reach(), period + 1);
    }
}

/// The degenerate windows — a zero period, a zero sample denominator, a lag past the end of the
/// series. They are excluded from [`PERIODS`] because `assert_reach_is_tight` has nothing to say
/// about a feature that answers the same thing everywhere, and because two of them are shorter than
/// any series can truncate. They are gated here instead, with the stream folded by hand.
///
/// ⚠ The answers are NOT uniformly NaN, which is the reason to check rather than assume: at
/// `WindowSpec::point_in_time(1)` the sample denominator is zero so `var`/`std`/`zscore` are NaN
/// everywhere, while `mean` is a plain one-row lag, `rank_pct` is a constant `1.0` and `median` is
/// the lagged value itself. Whatever each one is, a live strategy must receive the number the
/// matrix holds.
#[test]
fn the_degenerate_windows_stream_exactly_what_the_matrix_holds() {
    let x = shaped_column("random-walk", 200);
    for spec in [
        WindowSpec::indicator(0),     // period 0 — no window at all
        WindowSpec::point_in_time(1), // period 1, ddof 1 — a zero sample denominator
        WindowSpec { period: 3, lag: 250, min_periods: 3, ddof: 0 }, // a lag past the series
    ] {
        for &stat in STATS {
            let f = WindowFeature::new(stat, spec);
            let batch = f.vectorize(&x);
            let mut s = FeatureStream::new(&f);
            for (i, &v) in x.iter().enumerate() {
                let got = s.push(v);
                assert!(
                    bits_eq(got, batch[i]),
                    "{}/period{}/lag{} row {i}: streamed {got:?}, matrix holds {:?}",
                    stat.name(),
                    spec.period,
                    spec.lag,
                    batch[i]
                );
            }
        }
    }
}

/// The reach is TIGHT, not merely sufficient — asserted for every row of the matrix.
///
/// `assert_feature_parity` already calls this, but only after the parity assertions; running it
/// alone makes an over-declared reach fail with its own message instead of hiding behind a green
/// parity run that streamed the whole series.
#[test]
fn no_window_feature_over_declares_its_reach() {
    for &stat in STATS {
        for &period in PERIODS {
            for spec in [WindowSpec::indicator(period), WindowSpec::point_in_time(period)] {
                assert_reach_is_tight(
                    &format!("{stat:?}/p{period}/lag{}", spec.lag),
                    &WindowFeature::new(stat, spec),
                );
            }
        }
    }
}

/// A real column, stacked three instruments deep, with a boundary landing mid-window.
///
/// This is the panel discipline: `window::per_group` on the batch side, `FeatureStream::reset` on
/// the live side. `assert_feature_parity` runs a generated version; this one uses group lengths
/// that are deliberately NOT multiples of any period in [`PERIODS`], so no boundary can coincide
/// with a window edge and hide a leak.
#[test]
fn a_stacked_panel_agrees_with_a_stream_reset_at_each_instrument() {
    let x = shaped_column("high-vol", 2_003);
    let groups = [0..701, 701..1_303, 1_303..x.len()];
    for &stat in STATS {
        for period in [3usize, 20, 168] {
            let f = WindowFeature::point_in_time(stat, period);
            assert_stream_matches_batch_per_group(&format!("{stat:?}/p{period}"), &f, &x, &groups);
        }
    }
}

// =============================================================================================
// The planted defects: proof the harness rejects what it claims to.
//
// ⚠ Every assertion above is worth exactly as much as this section. `declaration-pinning tests
// don't gate` is a failure this repository has already shipped, and the cure is to mutate the gate
// and watch it go red. These mutants make that permanent instead of a thing somebody did once.
// =============================================================================================

/// A feature that is correct in every way EXCEPT that it under-declares its reach by one.
///
/// This is THE defect the harness exists for, in its smallest form: the kernel is untouched, the
/// batch column is exactly right, and the streamed column is computed from one value too little
/// history. Nothing errors — the numbers are simply different, which is what "the edge degrades
/// silently" means in practice.
struct OneShort(WindowFeature);
impl ColumnFeature for OneShort {
    fn vectorize(&self, x: &[f64]) -> Vec<f64> {
        self.0.vectorize(x)
    }
    fn reach(&self) -> usize {
        self.0.reach() - 1
    }
}

#[test]
#[should_panic(expected = "study→strategy divergence")]
fn the_harness_rejects_a_reach_that_is_one_too_short() {
    let f = OneShort(WindowFeature::point_in_time(WindowStat::ZScore { clip: Some(6.0) }, 24));
    assert_stream_matches_batch("planted/one-short", &f, &shaped_column("random-walk", 600));
}

/// The other archetype: a kernel whose value at row `i` depends on ALL history since row 0, wearing
/// a finite reach.
///
/// An expanding mean is the simplest honest example, and it is not a strawman — a running
/// accumulator that reads further back than its author believes is the exact class
/// `parity.rs`'s `NOT_TRUNCATION_INVARIANT` was written for, and the class that put six macro-
/// generated indicators into `main` computing cumulative sums from a truncated start. A study whose
/// feature normalises by an expanding mean, streamed with a rolling buffer, would drift apart from
/// its training matrix a little more with every bar.
struct ExpandingMean;
impl ColumnFeature for ExpandingMean {
    fn vectorize(&self, x: &[f64]) -> Vec<f64> {
        let mut sum = 0.0f64;
        x.iter()
            .enumerate()
            .map(|(i, v)| {
                sum += *v;
                sum / (i + 1) as f64
            })
            .collect()
    }
    fn reach(&self) -> usize {
        32
    }
}

#[test]
#[should_panic(expected = "study→strategy divergence")]
fn the_harness_rejects_a_kernel_that_reads_further_back_than_it_declares() {
    assert_stream_matches_batch(
        "planted/expanding",
        &ExpandingMean,
        &shaped_column("random-walk", 400),
    );
}

/// A reach so large the stream retains the entire series — the failure `assert_stream_matches_batch`
/// structurally cannot see, because retaining everything makes the streamed column IDENTICAL by
/// definition while the "stream" allocates without bound and reads history no kernel touches.
struct Padded(WindowFeature);
impl ColumnFeature for Padded {
    fn vectorize(&self, x: &[f64]) -> Vec<f64> {
        self.0.vectorize(x)
    }
    fn reach(&self) -> usize {
        self.0.reach() + 64
    }
}

#[test]
fn a_padded_reach_passes_parity_and_is_caught_only_by_the_tightness_assertion() {
    let f = Padded(WindowFeature::point_in_time(WindowStat::Std, 12));
    // Parity is GREEN — this is the half that proves the tightness assertion is not redundant.
    assert_stream_matches_batch("planted/padded", &f, &shaped_column("random-walk", 900));
    // ...and tightness is RED.
    let caught = std::panic::catch_unwind(|| assert_reach_is_tight("planted/padded", &f));
    assert!(
        caught.is_err(),
        "a reach padded by 64 rows must fail the tightness assertion — if it does not, an \
         over-declared reach is invisible to this crate's gates and a `stream` that retains its \
         whole input would ship as a stream"
    );
}

/// The harness must refuse a series that cannot truncate, rather than passing on it.
///
/// Over a series no longer than the reach the buffer never evicts a value, so the comparison is a
/// batch pass against itself and would stay green with the reach declared arbitrarily wrong. This
/// is the vacuity `parity.rs` records as the reason six indicators shipped broken — "it passed by
/// never reaching the code" — refused at the door instead.
#[test]
#[should_panic(expected = "batch pass against itself")]
fn the_harness_refuses_a_series_too_short_to_truncate() {
    let f = WindowFeature::point_in_time(WindowStat::Mean, 50);
    assert_stream_matches_batch("planted/too-short", &f, &shaped_column("random-walk", 50));
}

/// A feature declaring a reach of `0`. `FeatureStream` raises it to `1` so it can run at all, and
/// the harness must NOT let that survival measure pass for a declaration.
#[test]
#[should_panic(expected = "reach() is 0")]
fn the_harness_refuses_a_zero_reach() {
    struct Zero;
    impl ColumnFeature for Zero {
        fn vectorize(&self, x: &[f64]) -> Vec<f64> {
            vec![0.0; x.len()]
        }
        fn reach(&self) -> usize {
            0
        }
    }
    assert_stream_matches_batch("planted/zero", &Zero, &shaped_column("random-walk", 100));
}

/// A stream that is NOT reset at an instrument boundary carries the previous instrument's values
/// into the next one's window — the leak `per_group` removes on the batch side.
///
/// Proven directly rather than trusted: the same feature, folded straight through a stacked column
/// with no reset, must DISAGREE with the grouped batch column. If it agreed, the per-group half of
/// the harness would be asserting nothing.
#[test]
fn a_stream_that_forgets_to_reset_disagrees_with_the_grouped_matrix() {
    let mut x = shaped_column("random-walk", 400);
    // A second instrument at a completely different price level, so a window straddling the
    // boundary is unmistakable rather than merely slightly off.
    for v in x.iter_mut().skip(200) {
        *v = *v * 100.0 + 5_000.0;
    }
    let groups = [0..200, 200..400];
    let f = WindowFeature::point_in_time(WindowStat::Mean, 10);

    // The correct discipline agrees...
    assert_stream_matches_batch_per_group("boundary/reset", &f, &x, &groups);

    // ...and folding straight through does not.
    let batch = vike_indicators::window::per_group(&x, &groups, "x", |s| f.vectorize(s)).unwrap();
    let mut s = FeatureStream::new(&f);
    let leaked: Vec<f64> = x.iter().map(|&v| s.push(v)).collect();
    let disagreements = (0..x.len()).filter(|&i| !bits_eq(leaked[i], batch[i])).count();
    assert!(
        disagreements > 0,
        "a stream folded across an instrument boundary with no reset produced the SAME column as \
         the grouped batch pass — the per-group assertion is then vacuous and the boundary leak it \
         exists for is invisible to it"
    );
}
