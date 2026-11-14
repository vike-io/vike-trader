//! The CONSEQUENCE of the test double answering no fit identity, pinned.
//!
//! `crates/vike-ml/src/seam/test_support.rs`'s `ScriptedLearner` deliberately does NOT implement
//! [`vike_ml::Learner::fit_identity`] — the argument is in that module's doc. This file pins the
//! other half: a cache built over the double can never claim a hit, so nobody writes a test whose
//! hit path is silently the miss path.
//!
//! An INTEGRATION test on purpose: it compiles the library the way a downstream consumer does —
//! without `cfg(test)` — so it also proves the `test-support` feature EXPOSES the double across
//! the crate boundary.

use std::collections::HashMap;

use vike_ml::seam::test_support::ScriptedLearner;
use vike_ml::{DEFAULT_POINT, GridPoint, Learner, ProbaModel, TrainData};

/// The narrowest honest model of a cross-run fit cache: an `identity -> score` map CONSULTED and
/// WRITTEN only when the learner produced an identity, as [`vike_ml::Learner::fit_identity`]'s
/// contract requires.
///
/// ⚠ Deliberately NOT `vike_ml::seam::fit_cache::FitCache`, which is only the STORE: the rule under test
/// lives in whichever search loop drives a learner (a real one is proven in
/// `user_data/research/studies/rust/cohort/search.rs`'s `fit_and_score`).
#[derive(Default)]
struct ScoreMemo {
    entries: HashMap<[u8; 32], f64>,
    hits: usize,
    stores: usize,
}

impl ScoreMemo {
    fn fit_and_score<L: Learner>(
        &mut self,
        l: &L,
        d: &TrainData<'_>,
        p: &GridPoint,
        seed: u64,
    ) -> f64 {
        let id = l.fit_identity(d, p, seed);
        if let Some(k) = id
            && let Some(v) = self.entries.get(&k)
        {
            self.hits += 1;
            return *v;
        }
        let score = l.fit(d, p, seed).expect("the scripted fit succeeds").predict_proba(d.row(0));
        if let Some(k) = id {
            self.entries.insert(k, score);
            self.stores += 1;
        }
        score
    }
}

/// A learner that DOES answer an identity — the control, so a harness whose cache could never hit
/// cannot "prove" the double uncacheable.
///
/// ⚠ Its identity is a constant — the wrong-hit the real contract forbids — which is safe HERE
/// only because this file never varies the inputs it feeds this learner.
struct FixedIdentity;

struct One;

impl ProbaModel for One {
    fn predict_proba(&self, _row: &[f64]) -> f64 {
        1.0
    }
}

impl Learner for FixedIdentity {
    type Model = One;
    fn fit(&self, _d: &TrainData<'_>, _p: &GridPoint, _seed: u64) -> Result<One, String> {
        Ok(One)
    }
    fn fit_identity(&self, _d: &TrainData<'_>, _p: &GridPoint, _seed: u64) -> Option<[u8; 32]> {
        Some([7u8; 32])
    }
}

fn data() -> (Vec<f64>, Vec<f32>) {
    ((0..12).map(|v| v as f64).collect(), vec![0.0, 1.0, 0.0, 1.0, 0.0, 1.0])
}

#[test]
fn a_cache_built_over_the_double_never_claims_a_hit() {
    let (x, y) = data();
    let d = TrainData::new(&x, &y, 2).unwrap();
    let l = ScriptedLearner::constant(0.7);
    let mut cache = ScoreMemo::default();

    for _ in 0..5 {
        assert_eq!(cache.fit_and_score(&l, &d, &DEFAULT_POINT, 0), 0.7);
    }

    assert_eq!(cache.hits, 0, "the cache served a score for a learner that proved no identity");
    assert_eq!(cache.stores, 0, "the cache STORED under a key the learner never produced");
    assert_eq!(cache.entries.len(), 0);
    // ...and read off the learner: every one of the five calls reached `fit`.
    assert_eq!(l.fits(), 5, "a call was answered without reaching the learner");
}

#[test]
fn the_same_cache_does_hit_for_a_learner_that_answers_an_identity() {
    let (x, y) = data();
    let d = TrainData::new(&x, &y, 2).unwrap();
    let mut cache = ScoreMemo::default();

    for _ in 0..5 {
        assert_eq!(cache.fit_and_score(&FixedIdentity, &d, &DEFAULT_POINT, 0), 1.0);
    }

    // Without this the test above could be a harness that cannot hit anything.
    assert_eq!(cache.hits, 4, "the control cache never hit — the harness proves nothing");
    assert_eq!(cache.stores, 1);
}

#[test]
fn no_constructor_of_the_double_answers_an_identity() {
    let (x, y) = data();
    let d = TrainData::new(&x, &y, 2).unwrap();
    let p = DEFAULT_POINT;
    // Every constructor, two (point, seed) pairs each: an input-dependent `None` would look like
    // a cache that merely misses a lot.
    let moved = GridPoint { num_leaves: 63, ..p };
    for l in [
        ScriptedLearner::constant(0.7),
        ScriptedLearner::per_row(vec![0.1, 0.2, 0.3, 0.4, 0.5, 0.6]),
        ScriptedLearner::by_point(|q| q.learning_rate),
        ScriptedLearner::always_err("scripted failure"),
    ] {
        assert_eq!(l.fit_identity(&d, &p, 0), None);
        assert_eq!(l.fit_identity(&d, &moved, 9), None);
    }
}
