//! The LEARNER SEAM: fit a model, read one probability per row.
//!
//! Two traits, plus the request/answer pair one optional method takes. [`Learner`] takes a
//! [`crate::train::TrainData`] plus a [`crate::search::GridPoint`] plus a seed and hands back a
//! [`ProbaModel`]; [`ProbaModel`] turns one feature row into one probability. Everything a real
//! implementation needs beyond that — a driven child process, a scratch directory, a parameter
//! bag, a binned dataset cache — is behind the trait, and every caller that only wants "fit, then
//! predict" is written against these two methods. [`Capture`] and [`CapturedFit`] exist only for
//! [`Learner::fit_captured`], the one method a caller reaches for when it wants to KEEP something
//! from a fit besides the model.
//!
//! # Why the seam is in the library rather than in a caller
//!
//! It was written in the one study that first needed a model, as that study's own view of a
//! learner, deliberately naming no external crate so it could be built and tested while this
//! crate's API was still moving. That reason expired when this crate merged and pinned its format
//! and its binary, and what was left was a general abstraction living in a research crate at
//! layer 45 — reachable by nothing else in the workspace. The seam is what makes a learner
//! SUBSTITUTABLE, which is what lets code that fits models be tested on a machine with no
//! LightGBM binary — every Windows box and every CI runner — so it belongs where anything that
//! fits a model can reach it.
//!
//! # What this file deliberately does NOT do
//!
//! It does not name a trainer, a process, a file or an error enum. [`Learner::fit`] returns
//! `Result<_, String>` rather than [`crate::error::MlError`] on purpose: a caller scoring a grid
//! must be able to treat one rejected point as an ordinary event and keep the other candidates
//! competing, and the whole value of such a rejection is its SENTENCE — a real implementation
//! folds `MlError` through `Display` at the boundary, and `MlError::Backend` already carries the
//! trainer's own stderr tail inside it.

use crate::importance::FitImportance;
use crate::model::GbdtModel;
use crate::search::GridPoint;
use crate::train::TrainData;

/// One probability per feature row.
///
/// `Send + Sync` because a caller scoring a grid predicts from several threads at once. ⚠ If an
/// implementation's model type cannot be `Sync`, this bound is the thing that has to give — and
/// the caller then scores its grid SERIALLY.
pub trait ProbaModel: Send + Sync {
    fn predict_proba(&self, row: &[f64]) -> f64;
}

/// A one-liner, because [`GbdtModel`]'s inherent `predict_proba` is already this trait's method
/// byte for byte: `&[f64]` in, one `f64` out, `1 / (1 + exp(-sigmoid * raw))` for
/// `objective=binary`. The trait existed for a year before the impl did, in the crate that first
/// needed a learner — where it could only be written because both halves were foreign to it. It
/// was an ORPHAN-RULE error the moment the trait came home, which is a fair sign of where it
/// belonged.
///
/// ⚠ Spelled with the fully-qualified call rather than `self.predict_proba(row)` — an inherent
/// method wins method resolution over a trait one, so the short form would be this function
/// calling ITSELF forever, and would silently keep "working" if the two ever stopped agreeing,
/// which is the one thing this impl is here to notice.
impl ProbaModel for GbdtModel {
    fn predict_proba(&self, row: &[f64]) -> f64 {
        GbdtModel::predict_proba(self, row)
    }
}

/// Fit a model.
///
/// `Sync` because a caller scoring a grid calls [`Learner::fit`] from several threads at once. A
/// point whose parameters the learner rejects returns `Err` rather than panicking: one refused
/// candidate must not take the rest of the search down with it.
pub trait Learner: Sync {
    type Model: ProbaModel;

    fn fit(&self, d: &TrainData<'_>, p: &GridPoint, seed: u64) -> Result<Self::Model, String>;

    /// A content digest of EVERYTHING this learner's [`Learner::fit`] would consume for
    /// `(d, p, seed)` — the key half of a cross-run fit cache.
    ///
    /// The contract is that cache's design constraint: two calls may share an identity ONLY when
    /// their fits are bit-identical by construction, because a wrong cache hit silently corrupts
    /// results. `None` — the default — means "this learner cannot prove its inputs' identity",
    /// and a cache must then never serve or store anything for it; that is the safe direction (a
    /// refit, never a wrong answer), so a test double or a future learner that skips this method
    /// costs time and nothing else.
    ///
    /// ⚠ An implementation digests the actual BYTES a fit consumes — the rendered configs, the
    /// data as the trainer will read it, the trainer binary's own provenance — never a
    /// DESCRIPTION of them (a path, a fold id, a config name), and never a value it did not check
    /// (a shape this learner's `fit` would refuse gets no identity, so a refusal is never served
    /// from a cache).
    fn fit_identity(&self, _d: &TrainData<'_>, _p: &GridPoint, _seed: u64) -> Option<[u8; 32]> {
        None
    }

    /// [`Learner::fit`] plus whatever `want` asks be KEPT from it: the fitted model's per-feature
    /// importance, its own serialised text, or both.
    ///
    /// The default answers `None` to every artifact — a learner that cannot report them (every
    /// test double) stays a two-method learner, and a caller that NEEDS one decides for itself
    /// what an absence costs. The two absences are not symmetric in practice and the CALLER is
    /// where that asymmetry belongs: an empty importance table positively misinforms ("no feature
    /// mattered"), so a caller should refuse rather than write one, while an absent model text
    /// misinforms nobody and costs only the export. Only a caller that wants an artifact calls
    /// this; an ordinary fit still calls [`Learner::fit`] and is byte-identical to what it was
    /// before this method existed.
    ///
    /// ⚠ ONE fit, however many artifacts are asked for, which is why the request is a PARAMETER
    /// rather than a second method name: a caller asking for both would otherwise pay the fit
    /// twice — identical trees, twice the wall clock — and the fit this runs on is typically the
    /// one nothing caches.
    ///
    /// ⚠ [`Capture::text`] is the learner's OWN serialised bytes, never a re-serialisation of the
    /// returned [`Learner::Model`]: this crate parses LightGBM's `save_model` text
    /// ([`crate::parse_model_text`]) and deliberately has no WRITER, so a re-serialisation would
    /// be a second spelling of the format to keep in step with the trainer's.
    fn fit_captured(
        &self,
        d: &TrainData<'_>,
        p: &GridPoint,
        seed: u64,
        _want: Capture,
    ) -> Result<CapturedFit<Self::Model>, String> {
        Ok((self.fit(d, p, seed)?, None, None))
    }
}

/// What [`Learner::fit_captured`] hands back: the fitted model, then each artifact [`Capture`]
/// asked for — `None` where it was not asked for, and `None` too where this learner cannot report
/// it at all. The two cases are told apart by the CALLER, which knows what it asked.
pub type CapturedFit<M> = (M, Option<FitImportance>, Option<String>);

/// What a capture fit must KEEP beside the fitted model.
///
/// The flags are answered INDEPENDENTLY and a learner computes only what is asked, which is the
/// whole reason this is a struct rather than a bool: a text-only request must not be FAILED by an
/// importance parse it never wanted, and an importance-only request must not carry a megabyte of
/// model text nobody keeps. [`Capture::default`] asks for neither.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Capture {
    /// Report the fitted model's per-feature importance as a [`FitImportance`].
    pub importance: bool,
    /// Hand back the fitted model's own SERIALISED TEXT — what makes a caller able to EXPORT the
    /// model it selected instead of leaving a consumer to refit it.
    pub text: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The smallest learner that is not the real one: enough to prove the two DEFAULT methods
    /// behave as their docs claim, without importing anything that spawns a process.
    struct Flat(f64);

    struct FlatModel(f64);

    impl ProbaModel for FlatModel {
        fn predict_proba(&self, _row: &[f64]) -> f64 {
            self.0
        }
    }

    impl Learner for Flat {
        type Model = FlatModel;
        fn fit(&self, _d: &TrainData<'_>, _p: &GridPoint, _seed: u64) -> Result<FlatModel, String> {
            if self.0.is_nan() {
                return Err("scripted failure".into());
            }
            Ok(FlatModel(self.0))
        }
    }

    fn data() -> (Vec<f64>, Vec<f32>) {
        ((0..6).map(|v| v as f64).collect(), vec![0.0, 1.0, 0.0])
    }

    /// The default is NO identity: a learner that has not proven its inputs' identity must never
    /// be cached, and "did not implement the method" must not be mistaken for "these inputs are
    /// identical".
    #[test]
    fn fit_identity_defaults_to_none() {
        let (x, y) = data();
        let d = TrainData::new(&x, &y, 2).unwrap();
        assert_eq!(Flat(0.5).fit_identity(&d, &crate::search::DEFAULT_POINT, 0), None);
    }

    /// The default `fit_captured` is `fit` wearing a `None` per artifact — same model, no
    /// importance, no model text — so a learner that never heard of the method is exactly the
    /// learner it was before, and a caller can rely on `None` meaning "cannot report" rather than
    /// "reported nothing".
    #[test]
    fn the_default_fit_captured_is_fit_plus_none_for_every_artifact() {
        let (x, y) = data();
        let d = TrainData::new(&x, &y, 2).unwrap();
        let p = crate::search::DEFAULT_POINT;
        // Asked for BOTH artifacts, and still answering neither: a `Some` out of the default
        // would let every caller's "this learner cannot report one" refusal pass vacuously.
        let want = Capture { importance: true, text: true };
        let (m, imp, text) = Flat(0.7).fit_captured(&d, &p, 0, want).unwrap();
        assert_eq!(imp, None);
        assert_eq!(text, None);
        assert_eq!(m.predict_proba(d.row(0)), 0.7, "the model is the ordinary fit's");
        // ...and an erring learner errs THROUGH the default rather than having its error wrapped.
        assert_eq!(
            Flat(f64::NAN).fit_captured(&d, &p, 0, want).map(|_| ()).unwrap_err(),
            "scripted failure"
        );
    }

    /// The empty request is a real value, not a spelling nobody uses: it is what a caller with
    /// neither artifact asked for would pass, and it must still answer the ordinary fit's model.
    #[test]
    fn an_empty_capture_request_asks_for_neither_artifact() {
        assert_eq!(Capture::default(), Capture { importance: false, text: false });
        let (x, y) = data();
        let d = TrainData::new(&x, &y, 2).unwrap();
        let (m, imp, text) = Flat(0.7)
            .fit_captured(&d, &crate::search::DEFAULT_POINT, 0, Capture::default())
            .unwrap();
        assert_eq!((imp, text), (None, None));
        assert_eq!(m.predict_proba(d.row(0)), 0.7);
    }

    /// A model built by hand, so the trait impl's answer on a degenerate fit can be pinned with
    /// no trainer binary anywhere. An empty `split_feature` is LightGBM's own CONSTANT tree — its
    /// `GBDT::TrainOneIter` emits one through `AsConstantTree` when the learner could not split at
    /// all, which is rare, legal, and precisely the shape this is about.
    fn constant_model(leaf: f64) -> GbdtModel {
        use crate::model::{Objective, Tree};
        GbdtModel {
            max_feature_idx: 0,
            objective: Objective::Binary { sigmoid: 1.0 },
            feature_names: vec!["f0".to_string()],
            trees: vec![Tree {
                num_leaves: 1,
                split_feature: Vec::new(),
                threshold: Vec::new(),
                decision_type: Vec::new(),
                left_child: Vec::new(),
                right_child: Vec::new(),
                leaf_value: vec![leaf],
                cat_boundaries: Vec::new(),
                cat_threshold: Vec::new(),
            }],
        }
    }

    /// The impl is a delegation, and this is the assertion that says so — the one thing that impl
    /// exists to notice is the two ever disagreeing.
    ///
    /// ⚠ Asserted through `ProbaModel::` explicitly: [`GbdtModel`] has an INHERENT method of the
    /// same name that wins method resolution, so `m.predict_proba(..)` here would test the walker
    /// and not the impl.
    #[test]
    fn the_trait_method_on_a_gbdt_model_is_the_inherent_one() {
        for leaf in [0.0, 1.5, -2.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let m = constant_model(leaf);
            let (via_trait, inherent) =
                (ProbaModel::predict_proba(&m, &[0.0]), GbdtModel::predict_proba(&m, &[0.0]));
            assert_eq!(via_trait.to_bits(), inherent.to_bits(), "leaf {leaf}");
        }
    }

    /// ⚠ The impl does NOT sanitise, and that is a division of responsibility rather than an
    /// omission: a NaN raw score reaches the caller as a NaN probability, so a caller that ranks
    /// candidates can score the degenerate fit worst and lose exactly one of them. Clamping here
    /// to a plausible `0.5` would make such a fit rank like a mediocre model — and it could WIN,
    /// with nothing anywhere saying a model had failed.
    #[test]
    fn a_non_finite_leaf_reaches_the_caller_unsanitised() {
        assert_eq!(
            ProbaModel::predict_proba(&constant_model(0.0), &[0.0]),
            0.5,
            "the control case — without it every assertion below could be a broken harness"
        );
        assert!(ProbaModel::predict_proba(&constant_model(f64::NAN), &[0.0]).is_nan());
        // ...and the two that are deliberately NOT NaN: `1/(1+exp(-x))` SATURATES rather than
        // diverging, so an infinite raw score is a probability of exactly 1 or 0. Those are
        // usable answers and a blanket "non-finite raw means discard" would throw them away.
        assert_eq!(ProbaModel::predict_proba(&constant_model(f64::INFINITY), &[0.0]), 1.0);
        assert_eq!(ProbaModel::predict_proba(&constant_model(f64::NEG_INFINITY), &[0.0]), 0.0);
    }

    /// Compile-time, and that is the point: a caller that scores a grid in parallel needs these
    /// bounds, and it lives in another crate that would fail with a `rayon` type error instead of
    /// naming the cause.
    #[test]
    fn the_seam_carries_the_bounds_a_parallel_search_needs() {
        fn sync<T: Sync>() {}
        fn send_sync<T: Send + Sync>() {}
        sync::<Flat>();
        send_sync::<FlatModel>();
        sync::<TrainData<'static>>();
        send_sync::<GridPoint>();
    }
}
