//! A `type = "model"` key (W32, opt-in): the profile or mount names a RUN ID, and the file is handed that
//! run's model TEXT in its place.
//!
//! A strategy file runs jailed (decision 0281: no file but its artifact), so it never reads a model off
//! disk. Its HOST does, before the file is created: the backtesthub for a backtest (it hands the texts to the
//! sandboxed child), the tradehub for a paper, demo or live mount. Both put the text where the id was with
//! [`StrategyFileHeader::with_model_texts`], so the file reads `params.get(key)` as a string and parses it
//! itself. Where a run's text lives is the run store's (`vike_runs::read_model_text`), not this module's.
//!
//! The profile's own table keeps the id: a run record, a sweep report and a mount row name WHICH model ran,
//! never a copy of it.

use vike_model::params::{ParamValue, Params};

use super::{ParamType, StrategyFileHeader};

impl StrategyFileHeader {
    /// The keys this header declares `type = "model"`, in header order.
    pub fn model_keys(&self) -> impl Iterator<Item = &str> {
        self.params.iter().filter(|(_, p)| p.r#type == ParamType::Model).map(|(k, _)| k.as_str())
    }

    /// `params` with each present model key's run id replaced by the text `text_of` answers for that id.
    /// A model key `params` does not hold stays absent, a value that is not a string is left as it is (the
    /// header's type check refuses it before a file is created), and every other key is untouched: a header
    /// with no model key gives `params` back unchanged.
    ///
    /// # Errors
    ///
    /// `text_of`'s refusal, naming the key and the run.
    pub fn with_model_texts(
        &self,
        params: &Params,
        mut text_of: impl FnMut(&str) -> Result<String, String>,
    ) -> Result<Params, String> {
        let mut resolved = params.clone();
        for key in self.model_keys() {
            let Some(run) = params.get(key).and_then(ParamValue::as_str) else { continue };
            let text =
                text_of(run).map_err(|why| format!("`{key}` names model run {run:?}: {why}"))?;
            resolved.insert(key, ParamValue::Str(text));
        }
        Ok(resolved)
    }
}
