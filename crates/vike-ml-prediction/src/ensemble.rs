//! An ENSEMBLE of models: the MEAN of its members' predictions (bagging), or a META model over
//! their concatenated predictions (stacking). `vike-ml-training`'s `ensemble` fits both
//! (`crates/vike-ml-training/src/ensemble.rs`); this is how either is read and answered.
//!
//! - **Mean**: every member must predict the same [`PredictionKind`]; the ensemble predicts it, each
//!   output the plain mean of the members' (a bagged classifier averages probabilities).
//! - **Stacking**: the members may differ; their outputs, member after member, are the meta
//!   model's feature row, and the ensemble predicts what the meta model predicts.
//!
//! # The text, `vike-ml ensemble v1`
//!
//! ```text
//! vike-ml ensemble v1
//! combination=mean              (or stacking)
//! member_count=<K>
//! member=<byte count>           (K times, each followed by that member's text)
//! meta=<byte count>             (stacking only, followed by the meta model's text)
//! end of model
//! ```
//!
//! A member or meta text is ANY model text [`AnyModel::from_text`] reads, LightGBM's included, so
//! an ensemble is written from its members' OWN texts ([`Ensemble::text_from_parts`]): a LightGBM
//! member has no writer here, and none is needed. The envelope's rules are [`crate::text_format`]'s.

use crate::any_model::AnyModel;
use crate::error::MlError;
use crate::predictor::{PredictionKind, Predictor};
use crate::text_format::{Reader, Writer, nested_error};

/// The kind word of this model's text.
pub const KIND: &str = "ensemble";

/// Members and, for stacking, the meta model over their predictions.
#[derive(Clone, Debug, PartialEq)]
pub struct Ensemble {
    members: Vec<AnyModel>,
    meta: Option<Box<AnyModel>>,
}

impl Ensemble {
    /// `meta = None` is the mean of `members`, which must all predict the same kind; `Some` stacks
    /// them under the meta model. No member at all is refused.
    pub fn new(members: Vec<AnyModel>, meta: Option<AnyModel>) -> Result<Self, MlError> {
        let Some(first) = members.first() else {
            return Err(MlError::Shape("an ensemble needs at least one member".into()));
        };
        if meta.is_none()
            && let Some(other) = members.iter().find(|m| m.kind() != first.kind())
        {
            return Err(MlError::Shape(format!(
                "a mean ensemble's members must predict one kind; found {:?} and {:?}",
                first.kind(),
                other.kind()
            )));
        }
        Ok(Self { members, meta: meta.map(Box::new) })
    }

    pub fn members(&self) -> &[AnyModel] {
        &self.members
    }

    /// The meta model of a stacked ensemble; `None` for a mean.
    pub fn meta(&self) -> Option<&AnyModel> {
        self.meta.as_deref()
    }

    /// The `vike-ml ensemble v1` text of the ensemble whose members' texts are `member_texts`
    /// (and whose meta model's text is `meta_text`, for stacking). Each text is carried byte for
    /// byte.
    pub fn text_from_parts(member_texts: &[&str], meta_text: Option<&str>) -> String {
        let mut w = Writer::new(KIND);
        w.text("combination", if meta_text.is_some() { "stacking" } else { "mean" });
        w.count("member_count", member_texts.len());
        for text in member_texts {
            w.nested("member", text);
        }
        if let Some(text) = meta_text {
            w.nested("meta", text);
        }
        w.finish()
    }

    /// Read a `vike-ml ensemble v1` text, each nested text through [`AnyModel::from_text`].
    pub fn from_text(text: &str) -> Result<Self, MlError> {
        let mut r = Reader::open(text, KIND)?;
        let at = r.line();
        let stacking = match r.text("combination")? {
            "mean" => false,
            "stacking" => true,
            other => {
                return Err(MlError::Parse {
                    line: at,
                    what: format!("`combination={other}` is neither `mean` nor `stacking`"),
                });
            }
        };
        let count = r.count("member_count")?;
        let mut members = Vec::new();
        for i in 0..count {
            let (at, member) = r.nested("member")?;
            members.push(
                AnyModel::from_text(member)
                    .map_err(|e| nested_error(at, &format!("member {i}"), e))?,
            );
        }
        let meta = if stacking {
            let (at, meta) = r.nested("meta")?;
            Some(AnyModel::from_text(meta).map_err(|e| nested_error(at, "the meta model", e))?)
        } else {
            None
        };
        r.finish()?;
        Self::new(members, meta)
    }
}

impl Predictor for Ensemble {
    fn kind(&self) -> PredictionKind {
        match &self.meta {
            Some(meta) => meta.kind(),
            None => self.members[0].kind(),
        }
    }

    fn predict_into(&self, row: &[f64], out: &mut [f64]) -> Result<(), String> {
        if let Some(meta) = &self.meta {
            let mut features = Vec::new();
            for member in &self.members {
                let start = features.len();
                features.resize(start + member.kind().output_count(), 0.0);
                member.predict_into(row, &mut features[start..])?;
            }
            return meta.predict_into(&features, out);
        }
        let outputs = self.kind().output_count();
        if out.len() != outputs {
            return Err(format!(
                "this ensemble predicts {outputs} number(s) per row; the buffer holds {}",
                out.len()
            ));
        }
        out.fill(0.0);
        let mut one = vec![0.0; outputs];
        for member in &self.members {
            member.predict_into(row, &mut one)?;
            for (total, value) in out.iter_mut().zip(&one) {
                *total += value;
            }
        }
        for total in out.iter_mut() {
            *total /= self.members.len() as f64;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::linear::{LinearLink, LinearModel};

    fn linear(link: LinearLink, intercept: f64, coefficients: &[f64]) -> LinearModel {
        LinearModel { link, intercept, coefficients: coefficients.to_vec() }
    }

    #[test]
    fn a_mean_ensemble_averages_its_members_by_hand() {
        let a = linear(LinearLink::Identity, 1.0, &[2.0]);
        let b = linear(LinearLink::Identity, 0.0, &[-1.0]);
        let e = Ensemble::new(vec![a.into(), b.into()], None).unwrap();
        // x = 3: (1 + 6) and (−3), mean 2.
        assert_eq!(e.predict_one(&[3.0]), Ok(2.0));
        assert_eq!(e.kind(), PredictionKind::Value);
    }

    #[test]
    fn a_stacked_ensemble_feeds_its_members_outputs_to_the_meta_model() {
        let a = linear(LinearLink::Identity, 1.0, &[2.0]);
        let b = linear(LinearLink::Logistic, 0.0, &[0.0]);
        let meta = linear(LinearLink::Identity, 0.5, &[10.0, 4.0]);
        let e = Ensemble::new(vec![a.into(), b.into()], Some(meta.into())).unwrap();
        // x = 1: a = 3, b = 1/2, meta = 0.5 + 30 + 2.
        assert_eq!(e.predict_one(&[1.0]), Ok(32.5));
    }

    #[test]
    fn a_mean_of_two_kinds_or_of_nothing_is_refused() {
        let value = linear(LinearLink::Identity, 0.0, &[1.0]);
        let probability = linear(LinearLink::Logistic, 0.0, &[1.0]);
        assert!(Ensemble::new(vec![value.clone().into(), probability.into()], None).is_err());
        assert!(Ensemble::new(Vec::new(), Some(value.into())).is_err());
    }

    #[test]
    fn the_text_carries_every_member_text_and_reads_back() {
        let a = linear(LinearLink::Logistic, 1.0, &[2.0]);
        let gbdt = include_str!("../tests/fixtures/two_trees_binary_v4.txt");
        let meta = linear(LinearLink::Logistic, 0.0, &[1.0, -1.0]);
        let (a_text, meta_text) = (a.to_text(), meta.to_text());
        let text = Ensemble::text_from_parts(&[&a_text, gbdt], Some(&meta_text));
        let e = Ensemble::from_text(&text).unwrap();
        assert_eq!(e.members().len(), 2);
        assert_eq!(e.members()[1], AnyModel::from_text(gbdt).unwrap());
        assert_eq!(e.meta(), Some(&AnyModel::Linear(meta)));
        let mean = Ensemble::text_from_parts(&[&a_text, &a_text], None);
        assert!(Ensemble::from_text(&mean).unwrap().meta().is_none());
        // A bad member is reported with the line of the key that holds it.
        let broken = Ensemble::text_from_parts(&[&a_text.replace("link=logistic", "link=x")], None);
        let error = Ensemble::from_text(&broken).unwrap_err().to_string();
        assert!(error.contains("line 4") && error.contains("member 0"), "{error}");
    }
}
