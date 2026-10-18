//! `eval_event_rule`: fills and order rejects/denials over one `AlertEvent`.

use crate::delivery::FiredAlert;
use crate::rule::{AlertRule, RuleTrigger};

use super::gate::{filter_matches, maybe_fire};
use super::{AlertEvent, RuleState};

#[cfg(doc)]
use super::SnapshotFacts;

/// Evaluate an event-driven rule ([`RuleTrigger::Fill`] / [`OrderRejected`](RuleTrigger::OrderRejected))
/// against one [`AlertEvent`]. Returns `None` for any other trigger or a non-matching event.
///
/// ⚠ Takes [`AlertEvent`], NOT `vike_model::events::Event` — see [`SnapshotFacts`] for the whole
/// argument.
pub fn eval_event_rule(
    rule: &AlertRule,
    st: &mut RuleState,
    ev: &AlertEvent<'_>,
    now_ms: i64,
) -> Option<FiredAlert> {
    match (&rule.trigger, ev) {
        (
            RuleTrigger::Fill { venue, symbol },
            AlertEvent::Fill { venue: v, symbol: s, side, last_qty, last_px },
        ) => {
            if filter_matches(venue, v) && filter_matches(symbol, s) {
                maybe_fire(
                    rule,
                    st,
                    now_ms,
                    format!("fill {v} {s} side {side} qty {last_qty} @ {last_px}"),
                )
            } else {
                None
            }
        }
        (RuleTrigger::OrderRejected, AlertEvent::OrderRejected { client_order_id, reason }) => {
            maybe_fire(rule, st, now_ms, format!("order {client_order_id} rejected: {reason}"))
        }
        (RuleTrigger::OrderRejected, AlertEvent::OrderDenied { client_order_id, reason }) => {
            maybe_fire(rule, st, now_ms, format!("order {client_order_id} denied: {reason}"))
        }
        _ => None,
    }
}
