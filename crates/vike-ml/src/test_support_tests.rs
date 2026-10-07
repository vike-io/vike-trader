use super::*;
use crate::search::DEFAULT_POINT;

fn data() -> (Vec<f64>, Vec<f32>) {
    // 6 rows x 2 cols, row-major, column 0 the row ordinal.
    let x = (0..6).flat_map(|r| [r as f64, 100.0 + r as f64]).collect();
    (x, vec![0.0, 1.0, 0.0, 1.0, 1.0, 0.0])
}

#[test]
fn a_constant_learner_answers_its_constant_for_every_row_and_counts_the_fit() {
    let (x, y) = data();
    let d = TrainData::new(&x, &y, 2).unwrap();
    let l = ScriptedLearner::constant(0.7);
    let m = l.fit(&d, &DEFAULT_POINT, 0).unwrap();
    assert_eq!(m.predict_proba(d.row(0)), 0.7);
    assert_eq!(m.predict_proba(d.row(5)), 0.7);
    assert_eq!(l.fits(), 1);
}

/// The double is only worth having if a test can tell it apart from one that quietly answers a
/// constant — every other assertion here would pass against that impostor.
#[test]
fn per_row_answers_by_the_row_and_is_not_a_constant() {
    let (x, y) = data();
    let d = TrainData::new(&x, &y, 2).unwrap();
    let script = vec![0.10, 0.25, 0.40, 0.55, 0.70, 0.85];
    let m = ScriptedLearner::per_row(script.clone()).fit(&d, &DEFAULT_POINT, 0).unwrap();
    for (i, want) in script.iter().enumerate() {
        assert_eq!(m.predict_proba(d.row(i)), *want, "row {i}");
    }
    // Stated as an inequality too, so a failure names the fault rather than a number: six
    // equal answers is what a silently-constant double looks like.
    assert_ne!(
        m.predict_proba(d.row(0)),
        m.predict_proba(d.row(5)),
        "the double answered one probability for two different rows"
    );
}

#[test]
fn per_row_answers_nan_for_an_ordinal_that_is_not_a_usable_index() {
    let (x, y) = data();
    let d = TrainData::new(&x, &y, 2).unwrap();
    let m = ScriptedLearner::per_row(vec![0.10, 0.55]).fit(&d, &DEFAULT_POINT, 0).unwrap();
    assert_eq!(m.predict_proba(&[0.0, 0.0]), 0.10, "the control case");
    // Each of these is `0usize` after a saturating `as` cast, so each would silently replay
    // `probs[0]` — the exact answer the control case above proves is a plausible one.
    assert!(m.predict_proba(&[f64::NAN, 0.0]).is_nan(), "a NaN ordinal read as probs[0]");
    assert!(m.predict_proba(&[-1.0, 0.0]).is_nan(), "a negative ordinal read as probs[0]");
    assert!(m.predict_proba(&[f64::NEG_INFINITY, 0.0]).is_nan());
    assert!(m.predict_proba(&[f64::INFINITY, 0.0]).is_nan());
    assert!(m.predict_proba(&[2.0, 0.0]).is_nan(), "an ordinal past the script's end");
    // ...and the one that would otherwise be a panic rather than an answer.
    assert!(m.predict_proba(&[]).is_nan(), "an empty row must not abort the caller's run");
}

/// Same reason as `per_row_answers_by_the_row_and_is_not_a_constant`: a `by_point` double that
/// ignored the point would make a search's determinism test pass vacuously.
#[test]
fn by_point_reads_the_grid_point_it_was_fitted_at() {
    let (x, y) = data();
    let d = TrainData::new(&x, &y, 2).unwrap();
    let l = ScriptedLearner::by_point(|p| p.learning_rate + p.feature_fraction);
    let lo = l.fit(&d, &DEFAULT_POINT, 0).unwrap();
    let hi = l
        .fit(&d, &GridPoint { learning_rate: 0.1, feature_fraction: 0.9, ..DEFAULT_POINT }, 0)
        .unwrap();
    assert_eq!(
        lo.predict_proba(d.row(0)),
        DEFAULT_POINT.learning_rate + DEFAULT_POINT.feature_fraction
    );
    assert_eq!(hi.predict_proba(d.row(0)), 1.0);
    assert_ne!(
        lo.predict_proba(d.row(0)),
        hi.predict_proba(d.row(0)),
        "the double ignored the grid point it was fitted with"
    );
    // ...and the model is a CONSTANT once fitted: the point is read at fit time, not at
    // predict time, which is the property a caller leans on when it asserts what was fitted.
    assert_eq!(hi.predict_proba(d.row(0)), hi.predict_proba(d.row(4)));
}

#[test]
fn a_failing_learner_returns_its_own_message_rather_than_panicking() {
    let (x, y) = data();
    let d = TrainData::new(&x, &y, 2).unwrap();
    let l = ScriptedLearner::always_err("the trainer refused this point");
    assert_eq!(
        l.fit(&d, &DEFAULT_POINT, 0).map(|_| ()).unwrap_err(),
        "the trainer refused this point",
        "a refusal's SENTENCE is the whole value of it"
    );
}

#[test]
fn the_fit_counter_counts_every_call_including_the_ones_that_fail() {
    let (x, y) = data();
    let d = TrainData::new(&x, &y, 2).unwrap();
    let l = ScriptedLearner::always_err("scripted failure");
    assert_eq!(l.fits(), 0);
    for _ in 0..3 {
        assert!(l.fit(&d, &DEFAULT_POINT, 0).is_err());
    }
    assert_eq!(l.fits(), 3, "a failed fit still reached the learner");
}

/// A caller scoring a grid fits from several threads at once (the `Sync` bound on [`Learner`]
/// exists for exactly that), so the counter is read across threads. A load-then-store would
/// lose increments here; `fetch_add` does not.
#[test]
fn the_fit_counter_is_shared_across_threads() {
    let (x, y) = data();
    let d = TrainData::new(&x, &y, 2).unwrap();
    let l = ScriptedLearner::constant(0.5);
    std::thread::scope(|s| {
        for _ in 0..8 {
            s.spawn(|| {
                for _ in 0..250 {
                    l.fit(&d, &DEFAULT_POINT, 0).unwrap();
                }
            });
        }
    });
    assert_eq!(l.fits(), 2_000);
}

/// The two capture requests these tests make, named so a reader sees WHICH artifact each
/// assertion is about rather than decoding a pair of bare bools at the call site.
const IMPORTANCE_ONLY: Capture = Capture { importance: true, text: false };
const BOTH: Capture = Capture { importance: true, text: true };

/// The trait default is `fit` wearing a `None`, and a caller is meant to read that `None` as
/// "cannot report" rather than "reported nothing" — so the double must be able to be BOTH.
#[test]
fn importance_is_none_until_it_is_scripted_and_then_it_is_reported_verbatim() {
    let (x, y) = data();
    let d = TrainData::new(&x, &y, 2).unwrap();
    let p = DEFAULT_POINT;

    let (m, imp, _) =
        ScriptedLearner::constant(0.7).fit_captured(&d, &p, 0, IMPORTANCE_ONLY).unwrap();
    assert_eq!(imp, None, "an unscripted double reports no importance");
    assert_eq!(m.predict_proba(d.row(0)), 0.7, "the model is the ordinary fit's");

    // A table whose `n_features` DISAGREES with the matrix, handed back exactly as scripted —
    // the case a caller's own length check has no other way to be tested against.
    let table = FitImportance { n_features: 9, gain: vec![1.0], splits: vec![3] };
    let (_, imp, _) = ScriptedLearner::constant(0.7)
        .with_importance(table.clone())
        .fit_captured(&d, &p, 0, IMPORTANCE_ONLY)
        .unwrap();
    assert_eq!(imp, Some(table));

    // ...and an erring learner errs THROUGH the importance path rather than reporting a table
    // for a fit that never happened.
    let l = ScriptedLearner::always_err("scripted failure").with_importance(FitImportance {
        n_features: 2,
        gain: vec![1.0, 2.0],
        splits: vec![1, 1],
    });
    assert_eq!(
        l.fit_captured(&d, &p, 0, IMPORTANCE_ONLY).map(|_| ()).unwrap_err(),
        "scripted failure"
    );
    assert_eq!(l.fits(), 1, "the failed fit still reached the learner");
}

/// A scripted table is reported only when it is ASKED for — the double honours `want` the way
/// a real learner does, so a consumer that means to test its text-only path does not get an
/// importance it never requested and cannot have its own "was it asked for" logic pass
/// vacuously.
#[test]
fn a_scripted_importance_is_withheld_from_a_request_that_did_not_ask_for_it() {
    let (x, y) = data();
    let d = TrainData::new(&x, &y, 2).unwrap();
    let table = FitImportance { n_features: 2, gain: vec![1.0, 0.0], splits: vec![4, 0] };
    let l = ScriptedLearner::constant(0.7).with_importance(table.clone());
    let text_only = Capture { importance: false, text: true };
    assert_eq!(l.fit_captured(&d, &DEFAULT_POINT, 0, text_only).unwrap().1, None);
    assert_eq!(l.fit_captured(&d, &DEFAULT_POINT, 0, Capture::default()).unwrap().1, None);
    assert_eq!(l.fit_captured(&d, &DEFAULT_POINT, 0, IMPORTANCE_ONLY).unwrap().1, Some(table));
}

/// ⚠ The decision this module's `fit_captured` doc argues, asserted where it can be seen: NO
/// constructor turns a model text on, because a double has no serialised model and a
/// fabricated string would let a consumer's "the export is the trainer's own bytes" claim
/// pass vacuously. What the absence buys is the other branch: a caller can prove that a
/// learner which cannot serialise costs the EXPORT and not the run.
#[test]
fn the_double_answers_no_model_text_however_it_is_built_or_asked() {
    let (x, y) = data();
    let d = TrainData::new(&x, &y, 2).unwrap();
    let table = FitImportance { n_features: 2, gain: vec![1.0, 0.0], splits: vec![4, 0] };
    for l in [
        ScriptedLearner::constant(0.5),
        ScriptedLearner::per_row(vec![0.1, 0.2]),
        ScriptedLearner::by_point(|p| p.learning_rate),
        ScriptedLearner::constant(0.5).with_importance(table),
    ] {
        for want in [BOTH, Capture { importance: false, text: true }, Capture::default()] {
            let (_, _, text) = l.fit_captured(&d, &DEFAULT_POINT, 0, want).unwrap();
            assert_eq!(text, None, "{want:?}");
        }
    }
}

/// The decision this module's doc argues, asserted where it can be seen: no constructor, and
/// no scripted importance, turns the identity on.
#[test]
fn the_double_answers_no_fit_identity() {
    let (x, y) = data();
    let d = TrainData::new(&x, &y, 2).unwrap();
    let table = FitImportance { n_features: 2, gain: vec![1.0, 0.0], splits: vec![4, 0] };
    for l in [
        ScriptedLearner::constant(0.5),
        ScriptedLearner::per_row(vec![0.1, 0.2]),
        ScriptedLearner::by_point(|p| p.learning_rate),
        ScriptedLearner::always_err("scripted failure"),
        ScriptedLearner::constant(0.5).with_importance(table),
    ] {
        assert_eq!(l.fit_identity(&d, &DEFAULT_POINT, 0), None);
    }
}

/// Compile-time, and that is the point: a caller that scores a grid in parallel needs these
/// bounds, and it lives in another crate that would fail with a `rayon` type error instead of
/// naming the cause.
#[test]
fn the_double_carries_the_bounds_a_parallel_search_needs() {
    fn sync<T: Sync>() {}
    fn send_sync<T: Send + Sync>() {}
    sync::<ScriptedLearner>();
    send_sync::<ScriptedModel>();
}
