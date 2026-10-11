//! The outcome: [`StudyOutcome`] and the artifact-name rule.

use super::StudyError;

/// What a study hands back: named numbers, and named text artifacts.
///
/// **Two shapes, because a run listing and a run directory want different things**: the metrics
/// are what a listing row shows without opening anything, the artifacts are the files that land
/// beside the manifest (`per_rule.tsv`, `importance.tsv`, `model.txt`, `report.html`, …).
///
/// Metrics keep EMISSION order, so a study's headline comes first.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StudyOutcome {
    metrics: Vec<(String, f64)>,
    artifacts: Vec<(String, String)>,
}

impl StudyOutcome {
    /// An empty outcome. A study that legitimately found nothing returns one of these with a
    /// metric saying so — never an `Err`, which means the study could not run.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one headline number.
    ///
    /// A NON-FINITE value is ACCEPTED: `NaN` is the honest answer for a Sharpe over a fold that
    /// never traded, and `crates/vike-analytics/src/signal_backtest.rs`'s
    /// `neg_sharpe_mini_backtest` deliberately scores a degenerate slice `INFINITY`. Rounding
    /// either to zero would be the `unwrap_or(0.0)` that turns a gap into an observation. A
    /// DUPLICATE name is refused: two rows called `sharpe` make a listing ambiguous.
    pub fn metric(&mut self, name: &str, value: f64) -> Result<(), StudyError> {
        if self.metrics.iter().any(|(n, _)| n == name) {
            return Err(StudyError::Study(format!("duplicate metric {name:?}")));
        }
        self.metrics.push((name.to_string(), value));
        Ok(())
    }

    /// Record one named text artifact. `name` becomes a FILE NAME inside the run directory, so it
    /// is validated as one: non-empty, at most [`MAX_ARTIFACT_NAME`] bytes, `[A-Za-z0-9._-]` only,
    /// not starting with `.`, and not a duplicate — so `../../secrets.env` is refused by the
    /// charset, not something the writer has to defend against.
    ///
    /// ⚠ **TEXT, not bytes**, deliberately: every artifact the design names is text (`.tsv`,
    /// `.json`, `.html`, a LightGBM `save_model` dump), and a `String` is the one shape the Rhai
    /// tier can build. A study that genuinely needs bytes is the condition that reopens this.
    ///
    /// ⚠ **RESERVED names are the WRITER's business, not this type's.**
    /// `crates/vike-model/src/runs.rs` owns `MANIFEST_FILE` and `REPORT_FILE` at a layer this crate
    /// may not depend on, and restating the literals here would be a second authority. The caller
    /// refuses the collision.
    pub fn artifact(&mut self, name: &str, body: impl Into<String>) -> Result<(), StudyError> {
        if let Err(why) = valid_artifact_name(name) {
            return Err(StudyError::Study(format!("artifact name {name:?}: {why}")));
        }
        if self.artifacts.iter().any(|(n, _)| n == name) {
            return Err(StudyError::Study(format!("duplicate artifact {name:?}")));
        }
        self.artifacts.push((name.to_string(), body.into()));
        Ok(())
    }

    /// The headline numbers, in emission order.
    pub fn metrics(&self) -> &[(String, f64)] {
        &self.metrics
    }

    /// The named artifacts, in emission order.
    pub fn artifacts(&self) -> &[(String, String)] {
        &self.artifacts
    }

    /// One metric by name, or `None`.
    pub fn metric_value(&self, name: &str) -> Option<f64> {
        self.metrics.iter().find(|(n, _)| n == name).map(|(_, v)| *v)
    }
}

/// The longest artifact name accepted. Long enough for any file the design names and short enough
/// that no filesystem in the deployment set can refuse the path the writer builds from it.
pub const MAX_ARTIFACT_NAME: usize = 64;

/// The artifact-name rule, as a function so it is testable without building an outcome. `Ok(())`
/// or a sentence naming what is wrong.
pub fn valid_artifact_name(name: &str) -> Result<(), &'static str> {
    if name.is_empty() {
        return Err("must not be empty");
    }
    if name.len() > MAX_ARTIFACT_NAME {
        return Err("longer than MAX_ARTIFACT_NAME bytes");
    }
    if name.starts_with('.') {
        return Err("must not start with a dot (that is a hidden file, and `..` is a traversal)");
    }
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')) {
        return Err("must match [A-Za-z0-9._-] — it becomes a file name in the run directory");
    }
    Ok(())
}
