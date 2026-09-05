//! `fixture_fit` — the fixture that pins the LEARNER half of the contract, both ways.
//!
//! With a learner it fits, predicts, and exports the two artifacts a real study exports: the
//! model's own serialised text and a per-feature importance table. With none it refuses through
//! [`StudyError::NoLearner`] — the documented ceiling of a host with no LightGBM binary, which is
//! every Windows box, rather than a number computed without a model.
//!
//! The pipeline test drives BOTH arms, so the erasure in `vike_user_research::StudyLearner` is
//! proven against a real `vike_ml::Learner` (`ScriptedLearner`) on a runner that has no trainer.

use vike_ml::{Capture, TrainData, DEFAULT_POINT};
use vike_user_research::{StudyContext, StudyError, StudyOutcome};

/// Three rows of two features, plus their labels. A fixture, not a dataset: what is under test is
/// that a fit REACHES the learner and its artifacts come back, not what the model learned.
const X: [f64; 6] = [0.0, 1.0, 1.0, 0.5, 2.0, 0.25];
const Y: [f32; 3] = [0.0, 1.0, 1.0];
const N_COLS: usize = 2;

pub fn run(ctx: &StudyContext, params: &toml::Value) -> Result<StudyOutcome, StudyError> {
    let seed = params.get("seed").and_then(|v| v.as_integer()).unwrap_or(0).max(0) as u64;

    let learner =
        ctx.learner().ok_or_else(|| StudyError::NoLearner("a 3x2 fixture matrix".to_string()))?;

    let data = TrainData::new(&X, &Y, N_COLS)
        .map_err(|e| StudyError::Study(format!("fixture matrix rejected: {e}")))?;

    let want = Capture { importance: true, text: true };
    let (model, importance, text) = learner
        .fit_captured(&data, &DEFAULT_POINT, seed, want)
        .map_err(|e| StudyError::Study(format!("fit refused: {e}")))?;

    let mut out = StudyOutcome::new();
    out.metric("rows", data.n_rows as f64)?;
    out.metric("p_first_row", model.predict_proba(data.row(0)))?;

    // A learner that cannot report an artifact answers `None` — the trait's own default. The
    // asymmetry is the CALLER's to decide, and this study decides it the way `vike_ml`'s docs
    // describe: an absent model text costs only the export, while an EMPTY importance table would
    // positively misinform, so an absent one is recorded as absent rather than written as zeros.
    if let Some(t) = text {
        out.artifact("model.txt", t)?;
    }
    match importance {
        Some(imp) => {
            let mut tsv = String::from("feature\tgain\tsplits\n");
            for (i, (gain, splits)) in imp.gain.iter().zip(imp.splits.iter()).enumerate() {
                tsv.push_str(&format!("f{i}\t{gain}\t{splits}\n"));
            }
            out.artifact("importance.tsv", tsv)?;
            out.metric("importance_features", imp.n_features as f64)?;
        }
        None => out.metric("importance_features", f64::NAN)?,
    }
    Ok(out)
}
