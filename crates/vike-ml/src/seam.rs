//! The learner seam and what sits on it: the pair of traits a caller fits through, the shared test
//! double, and the cross-run fit cache.
//!
//! [`learner`] states the seam, `test_support` is the double that makes code which fits models
//! testable on a box with no LightGBM binary, and [`fit_cache`] is the content-addressed cache
//! over it. The one impl that drives a real trainer is [`crate::train::gbdt_learner`].

// NOT re-exported at the crate root: a consumer spells `vike_ml::seam::fit_cache::FitCache`, so
// `Transcript`/`DigestWriter` — hashing tools that exist here only to serve a key — do not read
// as this crate's vocabulary.
pub mod fit_cache;
pub mod learner;
// The shared learner double. Behind `test-support` so a shipped build compiles NONE of it, and
// `any(test, …)` so this crate's own unit tests reach it without the self-referential dev-dep.
// NOT re-exported at the crate root: a consumer spells `vike_ml::seam::test_support::ScriptedLearner`,
// so a double can never be mistaken for the real thing in a call site that ships.
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;
