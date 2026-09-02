//! The STUDY↔STRATEGY PARITY HARNESS: given a feature definition, prove that the per-bar
//! (streaming) computation equals the batch (matrix) computation for the same row, bit for bit.
//!
//! # Why this is a library module and not a `tests/` file
//!
//! `crates/vike-indicators/tests/parity.rs` already solves the equivalent problem for the
//! [`crate::Indicator`] roster, and this module deliberately follows it rather than inventing a
//! second idiom — same bitwise comparison with both-NaN equal, same "several data shapes, because
//! one smooth oscillation is a weak verifier", same insistence that a gate assert its own
//! non-vacuity. What it cannot follow is that file's LOCATION. `parity.rs` iterates
//! `crate::registry()`, a roster this crate owns; a model feature's definition belongs to whoever
//! wrote the study, and per
//! `docs/superpowers/specs/2026-08-24-research-engine-user-split-design.md` (R1) that source lives
//! in the author's own tree, not in this repository. A test binary here could never run on it.
//!
//! So the harness is public API behind the `test-support` feature — the `vike-ml`/`vike-data`
//! model, which a shipped build never compiles — and `crates/vike-indicators/tests/feature_parity.rs`
//! is one CALLER of it rather than the harness itself. The study is another, from wherever it
//! lives.
//!
//! # What is actually being proven, and what the tolerance is
//!
//! **The tolerance is ZERO** — `f64::to_bits` equality, with both-NaN counting as equal. That is
//! not an aspiration and it needs no justification about float error budgets, because
//! [`crate::feature::FeatureStream`] re-runs the feature's OWN `vectorize` over a retained tail:
//! there is no second arithmetic that could round differently. The crate's standing rule —
//! "never widen a float tolerance to make a test pass" — therefore costs nothing here.
//!
//! The one substantive claim is [`crate::feature::ColumnFeature::reach`]: that a bounded tail
//! reproduces the number a full-history batch pass computes. Under-declare it and the streamed
//! feature is computed from truncated history, silently, past a threshold no short series reaches
//! — the exact shape of the defect `parity.rs`'s
//! `every_trimmed_indicator_is_truncation_invariant` was written for, where six indicators were
//! green because the gate ran 400 bars and the trim first fired at 512. Everything below is built
//! so that the analogous mistake here fails loudly:
//!
//! * [`assert_stream_matches_batch`] refuses a series that is not longer than the reach, because
//!   over such a series the buffer never drops a value and the assertion compares a batch pass
//!   with itself.
//! * [`assert_reach_is_tight`] asserts the opposite direction — that retaining one value FEWER
//!   changes the answer — so a reach padded to the point of uselessness (`usize::MAX / 2` passes
//!   every parity assertion and streams nothing) is reported rather than rewarded.
//! * [`assert_feature_parity`] runs both over [`SHAPES`], a series long enough to truncate deeply.

use crate::feature::{ColumnFeature, FeatureStream};

/// The largest reach the generated-series harnesses will build a fixture for.
///
/// ⚠ This is a REFUSAL, not a clamp, and it exists because the failure it guards is the one
/// [`assert_reach_is_tight`] is about, taken to its limit. The generated series are sized from the
/// reach, so a feature declaring `usize::MAX / 2` would ask for a fixture larger than memory — and
/// the resulting OOM or overflow panic is an unreadable way to be told "your reach declaration is
/// nonsense". A million rows is far past any window this workspace's callers take (the deepest in
/// the tree is a week of hourly bars) while staying a fixture a test can actually build.
pub const MAX_TESTABLE_REACH: usize = 1 << 20;

/// Bitwise float equality, with both-NaN treated as equal.
///
/// The same predicate `crates/vike-indicators/tests/parity.rs` uses, for the same reason: NaN is a
/// legitimate feature value (a warm-up row, an undefined z-score) and `NaN != NaN` would make every
/// warm-up row a failure, while a tolerance would let a real divergence through.
pub fn bits_eq(a: f64, b: f64) -> bool {
    if a.is_nan() && b.is_nan() {
        true
    } else {
        a.to_bits() == b.to_bits()
    }
}

/// A tiny deterministic LCG — the Knuth/MMIX constants, top 53 bits as a uniform in `[0, 1)`.
///
/// This crate has no `rand` dependency and does not want one for fixture generation. It lives here
/// rather than in a test binary because both `crates/vike-indicators/tests/parity.rs` (bars) and
/// [`shaped_column`] (raw columns) need the same stream, and a `tests/` binary cannot import
/// another `tests/` binary.
///
/// ⚠ The constants and the shift are load-bearing, not stylistic. `parity.rs`'s
/// `finite_window_indicators_are_truncation_invariant_across_shapes_and_params` records MEASURED
/// mismatch counts over series this generator produces; changing the sequence invalidates them.
#[derive(Clone, Debug)]
pub struct Lcg(pub u64);

impl Lcg {
    /// The next uniform in `[0, 1)`.
    pub fn unit(&mut self) -> f64 {
        self.0 =
            self.0.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// The data shapes [`assert_feature_parity`] runs, each present because it drives a branch the
/// others do not.
///
/// ⚠ This is the lesson `parity.rs` states as "one shape is a weak verifier here", taken over to
/// raw columns — where it is SHARPER than it is for bars, because a `vike_model::Bar` cannot hold a
/// NaN and a feature column routinely does.
///
/// * `random-walk` — the ordinary case, and the one [`assert_reach_is_tight`] uses because every
///   window over it is defined.
/// * `trend` — a monotone series, where a rank-percentile pins at its bound and consecutive
///   windows share no order information.
/// * `mostly-flat` — long runs of BIT-IDENTICAL values. This is the shape `crate::window`'s module
///   doc is largely about: a constant window has variance exactly zero by DECISION, two kernels
///   branch on that (`zscore`'s `sd == 0.0`), and the branch decides NaN-versus-number rather than
///   a rounding difference. A smooth oscillation never produces it.
/// * `high-vol` — multiplicative moves, so the magnitudes a variance folds span orders.
/// * `with-gaps` — interior and leading NaNs. `WindowSpec::min_periods` blanks a window containing
///   one, and `window::zscore` carries an explicit guard for a NaN CURRENT value sitting on a
///   COMPLETE trailing window, whose absence once handed a model `-6.0` (the most extreme bearish
///   reading the clip allows) on roughly 70% of the rows of an entire feature family. At
///   `lag = 1` that is exactly the row this shape produces.
/// * `near-bound` — values pinned within ~5e-4 of a bound for long stretches WITHOUT being
///   bit-identical. `crates/vike-indicators/src/rolling.rs`'s `Z_CLIP` exists for measured
///   instances of it, and that constant's own doc carries the measurement: the standard deviation
///   collapses without reaching zero, so the constant-window decision does NOT fire and the ratio
///   explodes instead. It is the case `mostly-flat` looks like and is not.
pub const SHAPES: &[&str] =
    &["random-walk", "trend", "mostly-flat", "high-vol", "with-gaps", "near-bound"];

/// A deterministic column of `n` values in a named [`SHAPES`] shape.
///
/// Unknown shape names fall through to `high-vol` rather than panicking, so a caller extending
/// [`SHAPES`] gets a series rather than a crash while wiring it up.
pub fn shaped_column(shape: &str, n: usize) -> Vec<f64> {
    let mut rng = Lcg(0x5eed_1234_9876_abcd);
    let mut out = Vec::with_capacity(n);
    let mut prev = 100.0f64;
    for i in 0..n {
        let u = rng.unit() - 0.5;
        let v = match shape {
            "random-walk" => (prev + u * 2.0).max(1.0),
            "trend" => 100.0 + i as f64 * 0.05 + u * 0.4,
            // Long runs of BIT-IDENTICAL values — `prev` is copied, not recomputed.
            "mostly-flat" => {
                if i % 23 < 18 {
                    prev
                } else {
                    (prev + u * 3.0).max(1.0)
                }
            }
            // A leading gap (the shape of a differenced or lagged column) plus sparse interior
            // ones, confined to the first half so that complete windows exist afterwards even at
            // the deepest periods this crate's callers use.
            "with-gaps" => {
                if i < 3 || (i % 401 == 0 && i * 2 < n) {
                    f64::NAN
                } else {
                    (prev + u * 2.0).max(1.0)
                }
            }
            // Pinned just off -1.0, moving by ~1e-4 — a collapsing denominator that never reaches
            // the constant-window decision.
            "near-bound" => -1.0 + 0.017 + u * 0.0005,
            _ => (prev * (1.0 + u * 0.08)).max(1.0),
        };
        if !v.is_nan() {
            prev = v;
        }
        out.push(v);
    }
    out
}

/// ⚠ **THE HEADLINE ASSERTION: per-bar equals per-matrix, for the same row, bit for bit.**
///
/// Folds [`FeatureStream::push`] over `x` and compares every row against `feature.vectorize(&x)`.
///
/// NON-VACUOUS by refusal rather than by hope: `x` must be longer than the reach. Over a shorter
/// series the buffer never evicts anything, so the stream hands `vectorize` the entire prefix at
/// every row and the assertion degenerates into comparing a batch pass with itself — which is
/// precisely how `parity.rs`'s predecessor gate passed while six indicators were wrong.
///
/// Call this with your OWN data when you have it: a study's real column, replayed, is a stronger
/// verifier than any generated shape, because it contains the gaps, the pins and the outliers that
/// a generator has to be told about.
pub fn assert_stream_matches_batch<F>(label: &str, feature: &F, x: &[f64])
where
    F: ColumnFeature + ?Sized,
{
    let reach = feature.reach();
    assert!(
        reach > 0,
        "{label}: ColumnFeature::reach() is 0. A feature reads at least the row it answers for, \
         and a stream cannot retain nothing — `FeatureStream` raises it to 1 so it does not panic, \
         which is a survival measure and not a declaration. Declare the real bound."
    );
    assert!(
        x.len() > reach,
        "{label}: the series is {} rows and the reach is {reach}. The streaming buffer would never \
         evict a value, so this comparison is a batch pass against itself and would stay green \
         with the reach declared arbitrarily wrong. Use a longer series.",
        x.len()
    );

    let batch = feature.vectorize(x);
    assert_eq!(
        batch.len(),
        x.len(),
        "{label}: vectorize returned {} values for {} rows — the contract is one value per row",
        batch.len(),
        x.len()
    );

    let mut stream = FeatureStream::new(feature);
    for (i, &v) in x.iter().enumerate() {
        let got = stream.push(v);
        assert!(
            bits_eq(got, batch[i]),
            "{label} row {i}: streamed {got:?} ({:#018x}) but the batch matrix holds {:?} \
             ({:#018x}).\n\nThis is the study→strategy divergence: a live strategy would feed the \
             model a number it was never trained on, with no error raised anywhere. The usual \
             cause is an UNDER-DECLARED `ColumnFeature::reach` — the stream retained {reach} \
             values and the kernel read further back than that.",
            got.to_bits(),
            batch[i],
            batch[i].to_bits(),
        );
    }
}

/// The panel twin: the same assertion where the batch side is grouped by instrument and the
/// streaming side resets at each boundary.
///
/// A study's matrix is normally STACKED — several instruments in one column, one after another —
/// and `crate::window::per_group` exists because a rolling window run over the whole stack walks
/// straight across the boundary and averages the previous instrument's tail into the next one's
/// warm-up. The live side has the mirror-image hazard, in the form
/// `vike_analytics::signal::hysteresis` states for its own carried state: "a caller with several
/// assets must call this once per asset, or one asset's position leaks into the next."
///
/// This gates that the two disciplines meet: `per_group` on one side, [`FeatureStream::reset`] on
/// the other. Rows no group covers are not compared — `per_group` leaves them NaN by design, and a
/// live strategy has no such rows.
///
/// NON-VACUOUS: at least one compared row must be a NUMBER. A group shorter than the reach is
/// entirely warm-up, so both sides answer NaN at every row and the comparison proves nothing — the
/// same "it passed by never reaching the code" shape [`assert_stream_matches_batch`] refuses by
/// length. It is checked as an OUTCOME here rather than as a length precondition because a feature
/// may legitimately be NaN-heavy for reasons that have nothing to do with the group sizes.
pub fn assert_stream_matches_batch_per_group<F>(
    label: &str,
    feature: &F,
    x: &[f64],
    groups: &[std::ops::Range<usize>],
) where
    F: ColumnFeature + ?Sized,
{
    let batch = crate::window::per_group(x, groups, label, |s| feature.vectorize(s))
        .unwrap_or_else(|e| panic!("{label}: per_group refused the batch side: {e}"));

    let mut stream = FeatureStream::new(feature);
    let mut compared_a_number = false;
    for g in groups {
        stream.reset();
        for i in g.clone() {
            let got = stream.push(x[i]);
            assert!(
                bits_eq(got, batch[i]),
                "{label} row {i} (group {}..{}): streamed {got:?} but the grouped batch column \
                 holds {:?}. A stream that is not reset at an instrument boundary carries the \
                 previous instrument's values into this one's window — the leak `per_group` \
                 removes on the batch side and `reset` removes here.",
                g.start,
                g.end,
                batch[i],
            );
            compared_a_number |= !got.is_nan();
        }
    }
    assert!(
        compared_a_number,
        "{label}: every compared row was NaN on both sides, so this proves nothing about the \
         boundary discipline. Usually a group is shorter than the feature's reach ({}) and is \
         therefore entirely warm-up.",
        feature.reach()
    );
}

/// ⚠ **The other direction: a reach that is SUFFICIENT but not TIGHT.**
///
/// [`assert_stream_matches_batch`] can only catch an under-declared reach. An over-declared one
/// passes it perfectly — and `reach() = usize::MAX / 2` passes it while retaining the entire
/// series, i.e. while not streaming at all. That is a live failure mode of this mechanism, not a
/// tidiness complaint: a live strategy would allocate without bound and its "streamed" feature
/// would be a batch pass wearing a different name.
///
/// So this asserts the complement: retaining `reach - 1` values must CHANGE the answer at some row.
///
/// Call it when your reach is exact. Skip it — deliberately, and say why — when you have padded it
/// (`crate::FINITE_SLACK` is this crate's own precedent for padding a retention against a bound
/// that under-reports by a bar or two).
///
/// The series is generated as `random-walk` so that every window is defined: over a shape with
/// NaN gaps a truncated tail can answer NaN where the full pass also answers NaN, and "the same"
/// would then mean "both undefined" rather than "the reach is loose".
pub fn assert_reach_is_tight<F>(label: &str, feature: &F)
where
    F: ColumnFeature + ?Sized,
{
    let reach = feature.reach();
    if reach <= 1 {
        // A feature reading only the current row has no shorter tail to compare against; there is
        // nothing this assertion could say. Reported as a no-op rather than as a pass.
        return;
    }
    assert!(
        reach <= MAX_TESTABLE_REACH,
        "{label}: a declared reach of {reach} is past MAX_TESTABLE_REACH ({MAX_TESTABLE_REACH}). \
         That is not a fixture-size problem to work around — a reach that large means the stream \
         retains effectively everything it is fed, which is a batch pass wearing a streaming \
         interface."
    );
    let n = (reach * 6).max(64);
    let x = shaped_column("random-walk", n);
    let full = feature.vectorize(&x);

    let mut differs_somewhere = false;
    // Walk back from the end. Only rows with a complete window can distinguish anything, and the
    // last rows are the ones a live strategy actually asks about.
    for i in (reach - 1..n).rev() {
        if full[i].is_nan() {
            continue;
        }
        let short = &x[i + 2 - reach..=i]; // `reach - 1` values ending at `i`
        let cut = feature.vectorize(short);
        if !bits_eq(cut[cut.len() - 1], full[i]) {
            differs_somewhere = true;
            break;
        }
    }

    assert!(
        differs_somewhere,
        "{label}: retaining {} values instead of the declared {reach} gives the SAME answer at \
         every row, so the declared reach is larger than the kernel actually needs. An \
         over-declared reach passes every parity assertion while making the stream retain more \
         history than it reads — at the limit, all of it, which is a batch pass wearing a \
         streaming interface. Tighten the declaration, or skip this assertion deliberately and \
         record why the padding is wanted.",
        reach - 1
    );
}

/// The one call a feature author makes: [`assert_stream_matches_batch`] over every [`SHAPES`]
/// entry, plus [`assert_stream_matches_batch_per_group`] over a stacked panel, plus
/// [`assert_reach_is_tight`].
///
/// The series length is DERIVED as six times the reach (floored at 400 rows) rather than a fixed
/// number: a constant long enough for a 3-row window is nowhere near enough to truncate a 168-row
/// one, and a constant long enough for the deep case makes the shallow cases needlessly slow. This
/// is the same reasoning `parity.rs` records for deriving its own lengths from each indicator's own
/// `keep`, and for the same failure — "it passed by never reaching the code".
///
/// The panel is three groups of unequal length carved out of the same series, which is what makes
/// a boundary land mid-window rather than on a convenient multiple of the period.
pub fn assert_feature_parity<F>(label: &str, feature: &F)
where
    F: ColumnFeature + ?Sized,
{
    let reach = feature.reach();
    assert!(reach > 0, "{label}: ColumnFeature::reach() is 0 — declare the real bound");
    assert!(
        reach <= MAX_TESTABLE_REACH,
        "{label}: a declared reach of {reach} is past MAX_TESTABLE_REACH ({MAX_TESTABLE_REACH}) — \
         see that constant for why this is refused rather than clamped"
    );
    let n = (reach * 6).max(400);

    for shape in SHAPES {
        let x = shaped_column(shape, n);
        assert_stream_matches_batch(&format!("{label} [{shape}]"), feature, &x);
    }

    // Three unequal groups, each still longer than the reach so every group truncates.
    let x = shaped_column("random-walk", n * 2);
    let a = n;
    let b = a + (n * 3) / 5;
    let groups = [0..a, a..b, b..x.len()];
    assert_stream_matches_batch_per_group(&format!("{label} [panel]"), feature, &x, &groups);

    assert_reach_is_tight(label, feature);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `with-gaps` must actually contain NaNs, and `mostly-flat` must actually contain runs of
    /// BIT-IDENTICAL values — otherwise the two shapes that exist to drive the NaN and the
    /// constant-window branches drive neither, and every assertion over them is green for a reason
    /// nobody chose. This is `parity.rs`'s `the_constant_window_fixture_actually_bites` applied to
    /// the generator instead of to a single constant.
    #[test]
    fn the_shapes_contain_what_they_are_named_for() {
        let gaps = shaped_column("with-gaps", 1_000);
        assert!(gaps.iter().filter(|v| v.is_nan()).count() >= 4, "with-gaps holds no gaps");
        assert!(
            gaps[900..].iter().all(|v| !v.is_nan()),
            "the tail must be clean or every deep window is NaN"
        );

        let flat = shaped_column("mostly-flat", 1_000);
        let longest = flat
            .windows(2)
            .fold((0usize, 0usize), |(best, run), w| {
                let run = if w[0].to_bits() == w[1].to_bits() { run + 1 } else { 0 };
                (best.max(run), run)
            })
            .0;
        assert!(longest >= 10, "mostly-flat's longest bit-identical run is {longest}");

        let bound = shaped_column("near-bound", 500);
        assert!(bound.iter().all(|v| (*v + 0.983).abs() < 0.01), "near-bound must pin near -1");
        let distinct = {
            let mut b: Vec<u64> = bound.iter().map(|v| v.to_bits()).collect();
            b.sort_unstable();
            b.dedup();
            b.len()
        };
        assert!(
            distinct > 400,
            "near-bound must NOT be bit-identical — it is the case `mostly-flat` looks like and is \
             not, and a constant one would take the zero-variance shortcut instead of collapsing \
             the denominator"
        );
    }

    /// Every shape must be generated by `shaped_column` rather than falling through to the
    /// catch-all, or a typo in `SHAPES` silently runs `high-vol` six times.
    #[test]
    fn every_named_shape_is_distinct() {
        let mut seen: Vec<Vec<u64>> = Vec::new();
        for s in SHAPES {
            let col: Vec<u64> = shaped_column(s, 300).iter().map(|v| v.to_bits()).collect();
            assert!(!seen.contains(&col), "shape `{s}` duplicates another — is it spelled right?");
            seen.push(col);
        }
    }
}
