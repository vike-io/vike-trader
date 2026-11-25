//! ENSEMBLES over the learner seam: BAGGING (Breiman, "Bagging predictors", 1996) and STACKING
//! (Wolpert, "Stacked generalization", 1992), each a [`Learner`] whose model is
//! `vike-ml-prediction`'s [`Ensemble`] and whose text is `vike-ml ensemble v1`, carrying every
//! member's OWN text, so an ensemble of LightGBM, linear and Gaussian-process members runs in a
//! strategy file like any other model.
//!
//! - [`AsAnyModel`] wraps any learner as one whose model is an [`AnyModel`], so learners of
//!   different kinds sit in one list.
//! - [`BaggingLearner`] fits one member per BAG of row indices and predicts their mean. The bags are
//!   the CALLER's: a plain bootstrap, or the sequential bootstrap that draws overlapping labels less
//!   often (`vike-analytics`' `sample_weights::sequential_bootstrap`, López de Prado, *Advances in
//!   Financial Machine Learning* ch. 4). This crate draws none: it takes no edge onto
//!   `vike-analytics`, which owns the sampling.
//! - [`StackingLearner`] fits every member on each fold's TRAINING rows, predicts the fold's TEST
//!   rows (out of fold), fits the META learner on those out-of-fold predictions, then refits every
//!   member on all rows. The folds are the caller's too: purged ones
//!   (`vike-analytics`' `validation::purged_kfold_indices`) keep a member from seeing the labels it
//!   is about to be judged on. Each row must be in exactly one test fold.
//!
//! Member `i` is fitted with seed `seed + i` (wrapping), so bagged LightGBM members differ by seed
//! as well as by rows; a deterministic member ignores it. The grid point is handed to every member
//! and to the meta learner unchanged.

use vike_ml_prediction::Predictor;
use vike_ml_prediction::any_model::AnyModel;
use vike_ml_prediction::ensemble::Ensemble;

use crate::seam::fit_cache::Transcript;
use crate::{Capture, CapturedFit, GridPoint, Learner, TrainData};

/// Any learner, answering its model as an [`AnyModel`]. Identity, captured artifacts and errors
/// pass through unchanged.
pub struct AsAnyModel<L>(pub L);

impl<L> Learner for AsAnyModel<L>
where
    L: Learner,
    L::Model: Into<AnyModel>,
{
    type Model = AnyModel;

    fn fit(&self, d: &TrainData<'_>, p: &GridPoint, seed: u64) -> Result<AnyModel, String> {
        self.0.fit(d, p, seed).map(Into::into)
    }

    fn fit_identity(&self, d: &TrainData<'_>, p: &GridPoint, seed: u64) -> Option<[u8; 32]> {
        self.0.fit_identity(d, p, seed)
    }

    fn fit_captured(
        &self,
        d: &TrainData<'_>,
        p: &GridPoint,
        seed: u64,
        want: Capture,
    ) -> Result<CapturedFit<AnyModel>, String> {
        let (model, importance, text) = self.0.fit_captured(d, p, seed, want)?;
        Ok((model.into(), importance, text))
    }
}

/// The rows `indices` of `d`, copied, with `d`'s schema. An index past the data is refused.
pub(crate) fn subset(
    d: &TrainData<'_>,
    indices: &[usize],
    what: &str,
) -> Result<(Vec<f64>, Vec<f32>), String> {
    if let Some(bad) = indices.iter().find(|&&i| i >= d.n_rows) {
        return Err(format!("{what} names row {bad} of {} rows", d.n_rows));
    }
    let mut x = Vec::with_capacity(indices.len() * d.n_cols);
    let mut y = Vec::with_capacity(indices.len());
    for &i in indices {
        x.extend_from_slice(d.row(i));
        y.push(d.y[i]);
    }
    Ok((x, y))
}

pub(crate) fn view<'a>(d: &TrainData<'a>, x: &'a [f64], y: &'a [f32]) -> TrainData<'a> {
    TrainData { x, y, n_rows: y.len(), n_cols: d.n_cols, categorical: d.categorical }
}

pub(crate) fn member_seed(seed: u64, member: usize) -> u64 {
    seed.wrapping_add(member as u64)
}

/// One member fit through `fit_captured`, its text kept when `text` asks and the member can.
fn fit_member(
    learner: &dyn Learner<Model = AnyModel>,
    d: &TrainData<'_>,
    p: &GridPoint,
    seed: u64,
    text: bool,
) -> Result<(AnyModel, Option<String>), String> {
    let want = Capture { importance: false, text };
    let (model, _, member_text) = learner.fit_captured(d, p, seed, want)?;
    Ok((model, member_text))
}

/// The ensemble text, or `None` when any part had no text of its own.
fn ensemble_text(
    member_texts: Vec<Option<String>>,
    meta_text: Option<Option<String>>,
) -> Option<String> {
    let members: Option<Vec<String>> = member_texts.into_iter().collect();
    let members = members?;
    let meta = match meta_text {
        Some(text) => Some(text?),
        None => None,
    };
    let borrowed: Vec<&str> = members.iter().map(String::as_str).collect();
    Some(Ensemble::text_from_parts(&borrowed, meta.as_deref()))
}

/// Bagging: one `member` fit per bag of row indices, predicting their mean.
pub struct BaggingLearner<'a> {
    /// The learner every bag fits; [`AsAnyModel`] wraps a learner of any kind.
    pub member: &'a dyn Learner<Model = AnyModel>,
    /// One list of row indices per member; repeats are how a bootstrap weights a row.
    pub bags: Vec<Vec<usize>>,
}

impl BaggingLearner<'_> {
    fn fit_all(
        &self,
        d: &TrainData<'_>,
        p: &GridPoint,
        seed: u64,
        text: bool,
    ) -> Result<(Ensemble, Option<String>), String> {
        if self.bags.is_empty() {
            return Err("bagging needs at least one bag".into());
        }
        let mut members = Vec::with_capacity(self.bags.len());
        let mut texts = Vec::with_capacity(self.bags.len());
        for (i, bag) in self.bags.iter().enumerate() {
            let (x, y) = subset(d, bag, &format!("bag {i}"))?;
            let (model, member_text) =
                fit_member(self.member, &view(d, &x, &y), p, member_seed(seed, i), text)
                    .map_err(|e| format!("bag {i}: {e}"))?;
            members.push(model);
            texts.push(member_text);
        }
        let ensemble = Ensemble::new(members, None).map_err(|e| e.to_string())?;
        Ok((ensemble, if text { ensemble_text(texts, None) } else { None }))
    }
}

impl Learner for BaggingLearner<'_> {
    type Model = Ensemble;

    fn fit(&self, d: &TrainData<'_>, p: &GridPoint, seed: u64) -> Result<Ensemble, String> {
        Ok(self.fit_all(d, p, seed, false)?.0)
    }

    /// The tag `vike-ml bagging fit-identity v1`, then per bag its indices and the member's own
    /// identity on that bag's rows at its seed — `None` as soon as one member has none (the seam's
    /// safe direction) or a bag names a row the data does not have.
    fn fit_identity(&self, d: &TrainData<'_>, p: &GridPoint, seed: u64) -> Option<[u8; 32]> {
        if self.bags.is_empty() {
            return None;
        }
        let mut t = Transcript::new("vike-ml bagging fit-identity v1");
        for (i, bag) in self.bags.iter().enumerate() {
            let (x, y) = subset(d, bag, "a bag").ok()?;
            let indices: Vec<u8> = bag.iter().flat_map(|&r| (r as u64).to_le_bytes()).collect();
            t.part(&indices);
            t.digest_part(self.member.fit_identity(&view(d, &x, &y), p, member_seed(seed, i))?);
        }
        Some(t.finish())
    }

    /// The `vike-ml ensemble v1` text of the members' own texts when asked (`None` if a member has
    /// none); no importance.
    fn fit_captured(
        &self,
        d: &TrainData<'_>,
        p: &GridPoint,
        seed: u64,
        want: Capture,
    ) -> Result<CapturedFit<Ensemble>, String> {
        let (ensemble, text) = self.fit_all(d, p, seed, want.text)?;
        Ok((ensemble, None, text))
    }
}

/// Stacking: `members` fitted out of fold, `meta` fitted on their out-of-fold predictions.
pub struct StackingLearner<'a> {
    /// The base learners, of any kinds ([`AsAnyModel`]).
    pub members: Vec<&'a dyn Learner<Model = AnyModel>>,
    /// The learner fitted on the members' out-of-fold predictions, one column per output.
    pub meta: &'a dyn Learner<Model = AnyModel>,
    /// `(training rows, test rows)` per fold; every row in exactly one test set.
    pub folds: Vec<(Vec<usize>, Vec<usize>)>,
}

/// Refuse `folds` unless every one of `rows` rows is in exactly one TEST set: what an out-of-fold
/// prediction per row needs (stacking's meta fit here, `crate::double_ml`'s cross-fitting).
/// `what` names the caller in the sentence.
pub(crate) fn check_out_of_fold(
    folds: &[(Vec<usize>, Vec<usize>)],
    rows: usize,
    what: &str,
) -> Result<(), String> {
    let mut tested = vec![0usize; rows];
    for (k, (_, test)) in folds.iter().enumerate() {
        for &i in test {
            let Some(count) = tested.get_mut(i) else {
                return Err(format!("fold {k} tests row {i} of {rows} rows"));
            };
            *count += 1;
        }
    }
    if let Some(row) = tested.iter().position(|&c| c != 1) {
        return Err(format!(
            "row {row} is in {} test folds; {what} needs every row out of fold exactly once",
            tested[row]
        ));
    }
    Ok(())
}

impl StackingLearner<'_> {
    fn check_folds(&self, rows: usize) -> Result<(), String> {
        if self.members.is_empty() {
            return Err("stacking needs at least one member".into());
        }
        check_out_of_fold(&self.folds, rows, "stacking")
    }

    fn fit_all(
        &self,
        d: &TrainData<'_>,
        p: &GridPoint,
        seed: u64,
        text: bool,
    ) -> Result<(Ensemble, Option<String>), String> {
        self.check_folds(d.n_rows)?;
        // `widths[m]` is member m's output count, learned from its first fit.
        let mut widths: Vec<Option<usize>> = vec![None; self.members.len()];
        let mut out_of_fold: Vec<Vec<f64>> = vec![Vec::new(); d.n_rows];
        for (k, (train, test)) in self.folds.iter().enumerate() {
            let (x, y) = subset(d, train, &format!("fold {k}"))?;
            let train = view(d, &x, &y);
            for (m, member) in self.members.iter().enumerate() {
                let model = member
                    .fit(&train, p, member_seed(seed, m))
                    .map_err(|e| format!("fold {k}, member {m}: {e}"))?;
                let width = model.kind().output_count();
                if *widths[m].get_or_insert(width) != width {
                    return Err(format!("member {m} changed its output count between folds"));
                }
                let mut one = vec![0.0; width];
                for &i in test {
                    model.predict_into(d.row(i), &mut one)?;
                    out_of_fold[i].extend_from_slice(&one);
                }
            }
        }
        let meta_columns: usize = widths.iter().map(|w| w.unwrap_or(0)).sum();
        let meta_x: Vec<f64> = out_of_fold.concat();
        let meta_data = TrainData {
            x: &meta_x,
            y: d.y,
            n_rows: d.n_rows,
            n_cols: meta_columns,
            categorical: &[],
        };
        meta_data.validate().map_err(|e| format!("the out-of-fold predictions: {e}"))?;
        let (meta, meta_text) = fit_member(self.meta, &meta_data, p, seed, text)
            .map_err(|e| format!("the meta learner: {e}"))?;
        let mut members = Vec::with_capacity(self.members.len());
        let mut texts = Vec::with_capacity(self.members.len());
        for (m, member) in self.members.iter().enumerate() {
            let (model, member_text) = fit_member(*member, d, p, member_seed(seed, m), text)
                .map_err(|e| format!("member {m}: {e}"))?;
            members.push(model);
            texts.push(member_text);
        }
        let ensemble = Ensemble::new(members, Some(meta)).map_err(|e| e.to_string())?;
        Ok((ensemble, if text { ensemble_text(texts, Some(meta_text)) } else { None }))
    }
}

impl Learner for StackingLearner<'_> {
    type Model = Ensemble;

    fn fit(&self, d: &TrainData<'_>, p: &GridPoint, seed: u64) -> Result<Ensemble, String> {
        Ok(self.fit_all(d, p, seed, false)?.0)
    }

    // `fit_identity` stays the seam's `None`: the meta fit consumes the members' out-of-fold
    // PREDICTIONS, whose bytes exist only after the member fits run, so no identity can be proven
    // up front. A stacked fit is never cached; that is the seam's safe direction.

    /// The `vike-ml ensemble v1` text (members refitted on all rows, then the meta model) when
    /// asked (`None` if any part has no text); no importance.
    fn fit_captured(
        &self,
        d: &TrainData<'_>,
        p: &GridPoint,
        seed: u64,
        want: Capture,
    ) -> Result<CapturedFit<Ensemble>, String> {
        let (ensemble, text) = self.fit_all(d, p, seed, want.text)?;
        Ok((ensemble, None, text))
    }
}

#[path = "ensemble_tests.rs"]
#[cfg(test)]
mod ensemble_tests;
