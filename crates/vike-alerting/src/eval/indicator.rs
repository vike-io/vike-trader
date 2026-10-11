//! `eval_indicator_rule`: a threshold crossing over one consumer-computed `IndicatorSample`.

use crate::delivery::FiredAlert;
use crate::rule::{AlertRule, RuleTrigger};

use super::gate::{maybe_fire, scalar_crossed};
use super::{IndicatorSample, RuleState};

/// Evaluate an [`RuleTrigger::Indicator`] rule against one [`IndicatorSample`]. Returns `None` for
/// any other trigger, or when the sample is for a different instrument/indicator/output.
pub fn eval_indicator_rule(
    rule: &AlertRule,
    st: &mut RuleState,
    sample: &IndicatorSample,
    now_ms: i64,
) -> Option<FiredAlert> {
    let RuleTrigger::Indicator { venue, symbol, indicator, output, params: _, op, threshold } =
        &rule.trigger
    else {
        return None;
    };
    if venue != &sample.venue
        || symbol != &sample.symbol
        || indicator != &sample.indicator
        || *output != sample.output
    {
        return None;
    }
    if scalar_crossed(*op, st, sample.value, *threshold) {
        maybe_fire(
            rule,
            st,
            now_ms,
            format!(
                "{indicator}[{output}] {op} {threshold} on {venue} {symbol} (value {})",
                sample.value
            ),
        )
    } else {
        None
    }
}
