//! The shared LEARNER DOUBLE: a deterministic [`Learner`] that spawns nothing.
//!
//! Behind the `test-support` feature, so a shipped build compiles none of it — the
//! `vike-data`/`vike-model` model. A downstream crate turns it on through a DEV-dependency
//! (`vike-ml = { path = "…", features = ["test-support"] }`), which is the only edge that can
//! reach it.
//!
//! # Why this exists
//!
//! [`crate::learner`]'s two traits make a learner SUBSTITUTABLE, and that is worth nothing until
//! something substitutable exists. Fitting a real model here means driving the upstream LightGBM
//! CLI binary as a child process, and that binary is present on exactly one box in this
//! workspace's world: not the Windows dev box, and not any CI runner. So without a double, every
//! line of every consumer that fits a model — a study's fold driver, a Studio panel, a service
//! that refits on a schedule — is code no test can execute anywhere the merge gate runs.
//!
//! [`ScriptedLearner`] answers a probability its constructor scripted, counts the fits it was
//! asked for, and can be told to fail. That is the whole surface, and it is deliberately the whole
//! surface: a double grows into a second implementation the moment it starts having opinions.
//!
//! # ⚠ It answers NO fit identity, and that is a decision rather than an omission
//!
//! [`Learner::fit_identity`] promises a COLLISION-RESISTANT digest of everything a fit consumes —
//! it is the key half of a cross-run fit cache, and a wrong key does not fail loudly: it serves
//! one fit's score for another fit's inputs and lets a study pick the wrong model believing it
//! measured one. This double leaves the trait's `None` default in place, so a cache over it stores
//! nothing and serves nothing, and every fit is a real (cheap) refit. Three reasons, in the order
//! that decided it:
//!
//! 1. ⚠ **EXPIRED, and kept because it was the reason first given.** It read: the identity belongs
//!    to whoever owns the digest, and this crate does not own one — `vike-ml`'s dependencies are
//!    `vike-model` and nothing else, while the workspace's one SHA-256 framing lived with the
//!    cache that read it, one crate up. That framing has since been hoisted down here, exactly as
//!    that paragraph said it might be: [`crate::fit_cache`]'s `Transcript` is in this crate now,
//!    and so is the cache. **A double here COULD key on a digest today** — so reasons 2 and 3 are
//!    the whole of the argument, and they were always the load-bearing half.
//! 2. **A double that answers `Some` is a double that can be wrong.** The only way this one could
//!    key on its own behaviour is a caller-supplied `salt` it must remember to vary — a
//!    `constant(0.5)` and a `constant(0.9)` at one salt share every key while scoring differently,
//!    which is exactly the wrong hit. That footgun is survivable in a single file whose author
//!    owns every call site (`user_data/research/studies/rust/cohort/seam.rs`'s `with_identity`,
//!    which carries the warning); promoting it into a library used by consumers who have never
//!    read this paragraph is a different proposition. `None` makes the hazard unreachable.
//! 3. **The contract names this case itself.** [`Learner::fit_identity`]'s doc: `None` means "this
//!    learner cannot prove its inputs' identity", a cache must then never serve or store for it,
//!    "that is the safe direction (a refit, never a wrong answer), so a test double or a future
//!    learner that skips this method costs time and nothing else".
//!
//! **What a consumer that must test its cache's HIT path does instead:** wrap this double in its
//! own newtype and implement [`Learner::fit_identity`] there, with its own digest — the fifteen
//! lines land in the crate that can actually make the collision-resistance claim, next to the
//! cache that depends on it. `crates/vike-ml/tests/learner_double.rs` pins the consequence in both
//! directions: a cache over this double never hits, and the same cache does hit for a learner that
//! answers an identity.

use std::sync::atomic::{AtomicUsize, Ordering};

use crate::importance::FitImportance;
use crate::learner::{Capture, CapturedFit, Learner, ProbaModel};
use crate::search::GridPoint;
use crate::train::TrainData;

/// A deterministic [`Learner`] that spawns no process and reads no file.
pub struct ScriptedLearner {
    kind: Kind,
    fits: AtomicUsize,
    /// `Some` makes [`Learner::fit_captured`] report this table instead of the trait default's
    /// `None` — see [`ScriptedLearner::with_importance`].
    importance: Option<FitImportance>,
}

enum Kind {
    Constant(f64),
    PerRow(Vec<f64>),
    ByPoint(fn(&GridPoint) -> f64),
    Err(String),
}

/// What [`ScriptedLearner::fit`] hands back: a [`ProbaModel`] with the script baked in.
pub struct ScriptedModel {
    kind: ModelKind,
}

enum ModelKind {
    Constant(f64),
    /// Answers by the row's FIRST feature value used as an index, so a fixture that puts a row
    /// ordinal in column 0 replays a scripted probability series exactly.
    PerRow(Vec<f64>),
}

impl ScriptedLearner {
    /// Every row gets `p`.
    pub fn constant(p: f64) -> Self {
        Self::of(Kind::Constant(p))
    }

    /// Probability by row ordinal, read from the row's FIRST feature — so a caller's fixture
    /// carries `[ordinal, ..features]` and gets back the script it wrote.
    pub fn per_row(probs: Vec<f64>) -> Self {
        Self::of(Kind::PerRow(probs))
    }

    /// Every row gets `f(point)`: the fitted model's prediction is a pure function of the
    /// hyperparameter point it was fitted at, which is what makes a search's winner checkable
    /// without a trainer.
    ///
    /// ⚠ `f` is applied at FIT time, not at predict time — the returned model is a constant. That
    /// is the real shape (a fitted model does not re-read its hyperparameters), and it is what
    /// lets a caller assert that a search actually varied the point it fitted at.
    ///
    /// ⚠ Nothing here constrains `f`'s range, deliberately: a caller that wants a "probability"
    /// outside `[0, 1]` — to prove its own scorer clamps it, or refuses it — must be able to
    /// script one.
    pub fn by_point(f: fn(&GridPoint) -> f64) -> Self {
        Self::of(Kind::ByPoint(f))
    }

    /// A learner that refuses every fit with `msg`.
    ///
    /// The message survives verbatim, because the whole value of a rejection is its SENTENCE
    /// ([`crate::learner`]'s module doc says why `fit` returns `Result<_, String>`) and a consumer
    /// that folds a refusal into a report needs to assert the sentence reached it.
    pub fn always_err(msg: impl Into<String>) -> Self {
        Self::of(Kind::Err(msg.into()))
    }

    fn of(kind: Kind) -> Self {
        Self { kind, fits: AtomicUsize::new(0), importance: None }
    }

    /// Script the table [`Learner::fit_captured`] reports when a caller asks for importance.
    ///
    /// Without it the double takes the trait's default, which is `fit` wearing a `None` — the
    /// "this learner cannot report an importance" answer a caller is supposed to REFUSE on rather
    /// than write an empty table from. Both paths are worth testing, so both are reachable.
    ///
    /// ⚠ It is NOT checked against the matrix: a scripted table whose `n_features` disagrees with
    /// the data's width is handed back exactly as given. That is the point — a caller's own length
    /// check ([`FitImportance`]'s doc: a misaligned index→name mapping silently attributes
    /// importance to the wrong feature) has no other way to be tested.
    pub fn with_importance(mut self, importance: FitImportance) -> Self {
        self.importance = Some(importance);
        self
    }

    /// How many times [`Learner::fit`] was CALLED — including the calls that returned `Err`, so
    /// this answers "was the learner reached at all", which is what a "too small a fold never
    /// fits" assertion is really asking.
    pub fn fits(&self) -> usize {
        self.fits.load(Ordering::Relaxed)
    }
}

impl ProbaModel for ScriptedModel {
    /// ⚠ A per-row row whose first feature is not a usable index — absent, out of range, negative,
    /// or NaN — answers `NaN`, NOT `probs[0]`.
    ///
    /// That distinction is why this is written out rather than `row[0] as usize`: Rust's
    /// float-to-integer cast SATURATES, so `NaN as usize` and `-1.0 as usize` are both `0`. A
    /// fixture whose first column is a warm-up NaN instead of the row ordinal it was meant to be
    /// would then get `probs[0]` for every such row — a plausible number, silently replayed off
    /// the wrong script, in a double whose only job is to be checkable. `NaN` is the same answer
    /// an out-of-range ordinal already gets, so this is that rule reaching the cases a cast was
    /// hiding, not a second policy. An EMPTY row is the same fault wearing a panic, so it answers
    /// the same way: a double must not be the thing that aborts a consumer's test run.
    fn predict_proba(&self, row: &[f64]) -> f64 {
        match &self.kind {
            ModelKind::Constant(p) => *p,
            ModelKind::PerRow(ps) => {
                let Some(ord) = row.first().copied() else { return f64::NAN };
                if !ord.is_finite() || ord < 0.0 {
                    return f64::NAN;
                }
                ps.get(ord as usize).copied().unwrap_or(f64::NAN)
            }
        }
    }
}

impl Learner for ScriptedLearner {
    type Model = ScriptedModel;

    fn fit(&self, _d: &TrainData<'_>, p: &GridPoint, _seed: u64) -> Result<Self::Model, String> {
        self.fits.fetch_add(1, Ordering::Relaxed);
        Ok(ScriptedModel {
            kind: match &self.kind {
                Kind::Constant(v) => ModelKind::Constant(*v),
                Kind::PerRow(ps) => ModelKind::PerRow(ps.clone()),
                Kind::ByPoint(f) => ModelKind::Constant(f(p)),
                Kind::Err(msg) => return Err(msg.clone()),
            },
        })
    }

    /// Honours `want` the way a real learner does — each artifact computed only when it was asked
    /// for — with ONE artifact it can never answer.
    ///
    /// ⚠ **The model TEXT is always `None`, and that is honesty rather than laziness.** A double
    /// fits nothing, so it HAS no serialised model; the only text it could hand back is a string
    /// it invented. [`Capture::text`]'s contract is the learner's own bytes, and the property
    /// every consumer of that flag actually needs to test — "the export is the trainer's own
    /// bytes, and it reloads through [`crate::parse_model_text`]" — is precisely the one a
    /// fabricated string would let pass vacuously. So this double reports the absence, which is
    /// the other branch worth testing: a caller must be able to prove that a learner which cannot
    /// serialise costs the EXPORT and not the run. A consumer that needs a learner which DOES
    /// report a text writes its own — the same fifteen-line newtype this module's doc already
    /// prescribes for [`Learner::fit_identity`], and for the same reason: the claim belongs to
    /// whoever can make it.
    fn fit_captured(
        &self,
        d: &TrainData<'_>,
        p: &GridPoint,
        seed: u64,
        want: Capture,
    ) -> Result<CapturedFit<Self::Model>, String> {
        let model = self.fit(d, p, seed)?;
        let importance = if want.importance { self.importance.clone() } else { None };
        Ok((model, importance, None))
    }

    // ⚠ `fit_identity` is deliberately NOT implemented — the module doc carries the argument, and
    // `crates/vike-ml/tests/learner_double.rs` pins what the default `None` costs a caller.
}

#[cfg(test)]
mod tests {
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
}
