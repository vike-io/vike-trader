//! The walk-forward results: one out-of-sample step's [`TradingResults`]
//! ([`WalkforwardResultsStep`]) and the stitched walk ([`WalkforwardResults`]), whose `overall` is
//! the [`TradingResults`] of every step's trades over the stitched curve. The fold that produces
//! them is `vike_backtest::walkforward`'s; they live in this crate so the desktop's Studio renders
//! them without linking that harness.
//!
//! Since backtesthub protocol 9 a walk-forward reports through the SAME [`TradingResults`] a single
//! run does, where it carried four numbers of its own (`out_of_sample_return`,
//! `out_of_sample_sharpe`, `out_of_sample_equity_curve`, `walkforward_consistency`). A document in
//! that older shape is refused by [`WalkforwardResults::read_stored`], never converted.

use crate::results::TradingResults;

/// One out-of-sample step: its range, what its search chose, and its own [`TradingResults`] (of the
/// step's record, whose source is `crate::record::TradingRecordSource::WalkforwardStep`).
///
/// `Serialize`, so the compute-to-data `RunWalkforwardProfile` verb returns the results as JSON
/// TEXT, and `Deserialize`: the Studio's `RunWalkforward` answer
/// (`vike_backtesthub_client::wire_studio::WalkforwardResultsWire`) carries these steps AS
/// THEMSELVES. A field added here is a field of that wire, and
/// `crates/vike-backtesthub-client/tests/backtest_plane_wire_pin.rs` fails until its frozen line says
/// so.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct WalkforwardResultsStep {
    /// The `[start, end)` bar-index range of this out-of-sample step.
    pub test_range: (usize, usize),
    /// When the window's two halves ran, so a chart draws them on a date axis. `None`, and not written,
    /// for a step whose test half held no bar and in a document saved before the field (2026-11-20).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_times: Option<WalkforwardWindowTimes>,
    /// The parameter overrides this step SELECTED, when the caller searched inside it — `None` on
    /// the fixed-parameter walk, which is every caller that does not optimize.
    ///
    /// This field is the whole diagnostic value of a walk-forward OPTIMIZATION: the stitched number
    /// tells you the procedure's result, while the per-step winners tell you whether the procedure
    /// found anything STABLE or wandered. Our own cohort study is the cautionary case — per-fold
    /// in-sample↔out-of-sample R² came out ≈ 0.01, which is only visible if each fold's choice was
    /// recorded.
    ///
    /// ⚠ `default` + `skip_serializing_if`: a fixed walk's step carries no key at all. Each value
    /// crosses as the `vike_model::params::ParamValue` the strategy was handed, so the client reads
    /// back its kind with no text round trip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chosen_params: Option<Vec<(String, vike_model::params::ParamValue)>>,
    /// The step's own results, as a single run over its out-of-sample bars reports them. Each step
    /// runs fresh at `cash`, so its `total_return` is the step's out-of-sample return and its
    /// `final_equity` an amount at that start, never rebased.
    pub results: TradingResults,
}

/// **The open times (epoch ms, UTC) of the first and the last bar of a walk-forward window's two halves**, both
/// ends INCLUSIVE: the bars of [`WalkforwardResultsStep::test_range`] run from `test_start` to `test_end`. The train
/// pair is absent for a window with no contiguous train half (a combinatorial path's step).
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WalkforwardWindowTimes {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub train_start: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub train_end: Option<i64>,
    pub test_start: i64,
    pub test_end: i64,
}

/// The stitched walk-forward results.
///
/// It crosses as JSON TEXT (`RunWalkforwardProfile`) and is saved as such as the run's
/// `report.json`; the Studio wire carries the same fields in a type of its own
/// (`vike_backtesthub_client::wire_studio::WalkforwardResultsWire`), which adds the cost stamp.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct WalkforwardResults {
    /// Every out-of-sample step, in split order.
    pub steps: Vec<WalkforwardResultsStep>,
    /// The results of the whole walk: every step's trades over the stitched [`Self::equity_curve`],
    /// funding summed over the steps, and the walk's data warnings and strategy file attached by its
    /// driver. Its `total_return` is the stitched curve's and its `sharpe` the stitched curve's,
    /// annualized at the walk's factor.
    ///
    /// ⚠ Its `source` is `None`: it is the stitch of every step's record, no one record's, so
    /// `TradingResults::source_label` has nothing to say of it and [`Self::overall_label`] is its label.
    pub overall: TradingResults,
    /// The stitched out-of-sample equity curve: each step's curve rebased onto one running equity.
    pub equity_curve: Vec<f64>,
    /// Each stitched sample's timestamp (epoch ms), aligned to [`Self::equity_curve`]; empty when a window kept none
    /// (2026-11-20). ⚠ IN MEMORY ONLY, never written or read: the whole report crosses as one frame and is saved
    /// whole, and a time a sample would grow it by about three quarters (five years of minute bars past
    /// `MAX_FRAME_LEN`). The Studio's summary thins them beside its curve
    /// (`vike_backtesthub_client::summary::WalkforwardSummaryWire::equity_curve_timestamp`).
    #[serde(skip)]
    pub equity_timestamp: Vec<i64>,
    /// The share of steps that were profitable out of sample (`total_return > 0`), the
    /// `crate::overfit::overfit_verdict` input.
    pub profitable_step_share: f64,
    /// The currency the walk's money is in WHEN IT IS NOT THE DOLLAR (`JPY` for a profile that stated
    /// `engine.account_currency = "JPY"`): [`Self::equity_curve`] and every `final_equity` are amounts
    /// of it, while the returns and ratios need no unit. `None`, and so not serialized, for the dollar,
    /// which is also what every producer but the profile-driven walk
    /// (`vike_backtest::walkforward::runner::run_walkforward`) leaves.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_currency: Option<String>,
}

/// What every typed reader of a stored walk-forward document says of one saved in the shape before
/// [`WalkforwardResults`] (its `overall` absent, its `out_of_sample_return` present). It is refused,
/// never converted; `vike-cli backtest show` still prints the file as it was saved.
pub const WALKFORWARD_SAVED_BEFORE_STEPS: &str = "this walk-forward run was saved before backtesthub \
     protocol 9 gave walk-forward results their steps and overall results; run it again (`vike-cli \
     backtest show` still prints it as saved)";

impl WalkforwardResults {
    /// **The ONE typed read of a stored walk-forward document** (a walk-forward run's `report.json`,
    /// the backtesthub's `Response::WalkforwardReport` text, one path of a combinatorial walk).
    ///
    /// # Errors
    /// [`WALKFORWARD_SAVED_BEFORE_STEPS`] for a document in the older shape, and the parse error, in
    /// words, for text that is not walk-forward results at all.
    pub fn read_stored(text: &str) -> Result<Self, String> {
        let value: serde_json::Value = serde_json::from_str(text)
            .map_err(|e| format!("not stored walk-forward results: {e}"))?;
        if value.get("overall").is_none() && value.get("out_of_sample_return").is_some() {
            return Err(WALKFORWARD_SAVED_BEFORE_STEPS.to_string());
        }
        let mut results: Self = serde_json::from_value(value)
            .map_err(|e| format!("not stored walk-forward results: {e}"))?;
        // The sources serde skips, re-attached from the file's own data: each step names its window
        // and what it chose. No timestamp is stored, so no period is; `overall` has no one source.
        for step in &mut results.steps {
            step.results.source = Some(crate::record::TradingRecordSource::WalkforwardStep {
                window: step.test_range,
                params: step.chosen_params.clone(),
            });
        }
        Ok(results)
    }

    /// **The label of the walk's [`Self::overall`] results**: `walk-forward overall [a, b)`, the bar
    /// span from the first step's start to the last one's end, in the words a step's own label uses
    /// (`walk-forward step [a, b)`). `None` for a walk of no step.
    #[must_use]
    pub fn overall_label(&self) -> Option<String> {
        let (first, last) = (self.steps.first()?, self.steps.last()?);
        Some(format!("walk-forward overall [{}, {})", first.test_range.0, last.test_range.1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A one-step walk of a rising curve: its step and its overall results are the same record's.
    fn one_step_walk() -> WalkforwardResults {
        let record = crate::TradingRecord {
            equity_curve: vec![1000.0, 1050.0],
            final_equity: 1050.0,
            ..Default::default()
        };
        let results = TradingResults::from_record(None, &record, 252.0);
        WalkforwardResults {
            steps: vec![WalkforwardResultsStep {
                test_range: (0, 2),
                window_times: None,
                chosen_params: None,
                results: results.clone(),
            }],
            overall: results,
            equity_curve: vec![1000.0, 1050.0],
            equity_timestamp: Vec::new(),
            profitable_step_share: 1.0,
            account_currency: None,
        }
    }

    /// The shape of protocol 9: `steps` (each its range and its results), `overall`, the stitched
    /// `equity_curve` and `profitable_step_share`; no `chosen_params` on a fixed step and no
    /// `account_currency` for the dollar. It reads back to the same bytes, every extended metric of
    /// a curve that never drew down (`inf`, written `null`) included.
    #[test]
    fn the_results_are_steps_and_overall_and_read_back_to_their_bytes() {
        let walk = one_step_walk();
        let text = serde_json::to_string(&walk).unwrap();
        assert!(
            text.starts_with(r#"{"steps":[{"test_range":[0,2],"results":{"#),
            "steps first, a fixed step carries no chosen_params: {text}"
        );
        assert!(text.contains(r#"}}],"overall":{"#), "{text}");
        assert!(text.ends_with(r#""equity_curve":[1000.0,1050.0],"profitable_step_share":1.0}"#));
        assert!(text.contains(r#""recovery_factor":null"#), "a curve that never drew down: {text}");
        assert_eq!(
            serde_json::to_value(&walk).unwrap()["overall"]["total_return"],
            serde_json::json!(walk.overall.total_return)
        );

        let back: WalkforwardResults = serde_json::from_str(&text).expect("the shape decodes");
        assert_eq!(serde_json::to_string(&back).unwrap(), text);
    }

    /// A walk-forward `report.json` saved before protocol 9 (the shape every walk wrote until then).
    const SAVED_BEFORE_STEPS: &str = r#"{"windows":[{"test_range":[0,10],"out_of_sample_return":0.05}],
        "out_of_sample_equity_curve":[1000.0,1050.0],"out_of_sample_return":0.05,
        "out_of_sample_sharpe":1.5,"walkforward_consistency":1.0,"funding_paid":0.0}"#;

    /// An old document is REFUSED with the one sentence, never converted and never read as empty
    /// results; the current shape reads back to itself; text that is neither is a parse error in
    /// words, not the old-shape sentence.
    #[test]
    fn a_walk_saved_before_the_steps_is_refused_with_run_it_again() {
        let error = WalkforwardResults::read_stored(SAVED_BEFORE_STEPS).unwrap_err();
        assert_eq!(error, WALKFORWARD_SAVED_BEFORE_STEPS);
        assert!(error.contains("run it again"), "{error}");

        let text = serde_json::to_string(&one_step_walk()).unwrap();
        let back = WalkforwardResults::read_stored(&text).expect("the current shape reads");
        assert_eq!(serde_json::to_string(&back).unwrap(), text);
        // The skipped sources come back from the file's own data, so every decoded result is labelled.
        assert_eq!(
            back.steps[0].results.source_label().as_deref(),
            Some("walk-forward step [0, 2)")
        );
        assert_eq!(back.overall_label().as_deref(), Some("walk-forward overall [0, 2)"));

        let error = WalkforwardResults::read_stored(r#"{"unrelated":1}"#).unwrap_err();
        assert!(error.starts_with("not stored walk-forward results"), "{error}");
    }
}
