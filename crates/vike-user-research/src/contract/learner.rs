//! The learner seam, erased: [`StudyLearner`] is every real [`vike_ml::Learner`] behind a box.

#[cfg(doc)]
use super::StudyContext;
use vike_ml::{Capture, FitImportance, GridPoint, Learner, ProbaModel, TrainData};

/// What [`StudyLearner::fit_captured`] hands back — [`vike_ml::CapturedFit`] with the model boxed.
pub type CapturedStudyFit = (Box<dyn ProbaModel>, Option<FitImportance>, Option<String>);

/// [`vike_ml::Learner`] with its associated `Model` type erased, so it can cross a NON-GENERIC
/// boundary.
///
/// `type Model: ProbaModel` makes `dyn Learner` unusable on [`StudyContext`]; this trait is that
/// seam boxed and NOTHING ELSE — the blanket impl below covers every real `Learner`.
///
/// ⚠ **It erases the WHOLE trait, all three methods, deliberately**: `fit_captured` lets a run
/// EXPORT the model it selected and `fit_identity` keys a cross-run fit cache. An erasure that
/// quietly downgrades the seam it erases is worse than no erasure.
///
/// `Send + Sync` because the workspace pins `rhai` with `features = ["sync"]` (root `Cargo.toml`):
/// a type the Rhai tier registers must be both, and adding the bound later breaks every caller.
pub trait StudyLearner: Send + Sync {
    /// Fit a model at one hyperparameter point. See [`vike_ml::Learner::fit`] — including its rule
    /// that a REFUSED point is an ordinary `Err`, not a reason to stop a search.
    fn fit(
        &self,
        d: &TrainData<'_>,
        p: &GridPoint,
        seed: u64,
    ) -> Result<Box<dyn ProbaModel>, String>;

    /// Fit, keeping whatever `want` asks for beside the model. See
    /// [`vike_ml::Learner::fit_captured`] — ONE fit however many artifacts are requested.
    fn fit_captured(
        &self,
        d: &TrainData<'_>,
        p: &GridPoint,
        seed: u64,
        want: Capture,
    ) -> Result<CapturedStudyFit, String>;

    /// A content digest of everything a fit would consume, or `None` — see
    /// [`vike_ml::Learner::fit_identity`]. `None` means a cache must never serve or store for this
    /// learner, which is the safe direction.
    fn fit_identity(&self, d: &TrainData<'_>, p: &GridPoint, seed: u64) -> Option<[u8; 32]>;
}

/// Every real [`vike_ml::Learner`] is a [`StudyLearner`], and deliberately the only one: a second
/// implementation would be a second learner seam, and this crate is not where a learner is defined.
impl<L> StudyLearner for L
where
    L: Learner + Send + Sync,
    L::Model: 'static,
{
    fn fit(
        &self,
        d: &TrainData<'_>,
        p: &GridPoint,
        seed: u64,
    ) -> Result<Box<dyn ProbaModel>, String> {
        Ok(Box::new(Learner::fit(self, d, p, seed)?))
    }

    fn fit_captured(
        &self,
        d: &TrainData<'_>,
        p: &GridPoint,
        seed: u64,
        want: Capture,
    ) -> Result<CapturedStudyFit, String> {
        let (model, importance, text) = Learner::fit_captured(self, d, p, seed, want)?;
        Ok((Box::new(model), importance, text))
    }

    fn fit_identity(&self, d: &TrainData<'_>, p: &GridPoint, seed: u64) -> Option<[u8; 32]> {
        Learner::fit_identity(self, d, p, seed)
    }
}
