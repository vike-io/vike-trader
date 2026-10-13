//! The error: [`StudyError`], split by who an operator has to go and talk to.

use vike_data::DataError;

#[cfg(doc)]
use super::StudySim;

/// Why a study could not produce a result.
///
/// The split is by WHO an operator has to go and talk to: a `Study` is the author's, a `Data` is
/// the ingest side's (ADR 0029's item 3: a study missing a window stops, naming the range and the
/// command that fills it), and `NoLearner`/`NoSim` are the HOST's, fixed by running elsewhere.
#[derive(Debug)]
pub enum StudyError {
    /// The study's own refusal: a parameter it will not accept, an assumption violated, a fit it
    /// decided was fatal (a search treats a REJECTED hyperparameter point as an ordinary event).
    Study(String),
    /// A store read failed. Carries `vike-data`'s own error so the caller can tell a query failure
    /// from an I/O one without parsing a sentence.
    Data(DataError),
    /// This study must TRAIN and the host supplied no learner. TYPED because a caller can act on
    /// it mechanically: it is the documented ceiling of a host with no LightGBM binary, not a bug.
    /// The payload names what the study wanted to fit.
    NoLearner(String),

    /// The simulator twin of [`StudyError::NoLearner`]: this study needs an event-driven run and
    /// the caller supplied no [`StudySim`]. The payload names what it was about to run.
    NoSim(String),
}

impl std::fmt::Display for StudyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StudyError::Study(m) => write!(f, "study: {m}"),
            StudyError::Data(e) => write!(f, "store read: {e}"),
            StudyError::NoLearner(what) => write!(
                f,
                "study needs a learner to fit {what}, and this host supplied none \
                 (no LightGBM binary on this platform — inference and importance still work)"
            ),
            StudyError::NoSim(what) => write!(
                f,
                "study needs an event-driven backtest to run {what}, and this host supplied \
                 none — only a caller that links the simulator can, which is the study runner"
            ),
        }
    }
}

impl std::error::Error for StudyError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            StudyError::Data(e) => Some(e),
            _ => None,
        }
    }
}

/// So a study body can `?` a store read straight through.
impl From<DataError> for StudyError {
    fn from(e: DataError) -> Self {
        StudyError::Data(e)
    }
}
