//! `evaluation_event_rule`: fills, order rejects/denials and the journal's growth over one `AlertEvent`.

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
pub fn evaluation_event_rule(
    rule: &AlertRule,
    st: &mut RuleState,
    event: &AlertEvent<'_>,
    now_ms: i64,
) -> Option<FiredAlert> {
    match (&rule.trigger, event) {
        (
            RuleTrigger::Fill { venue, symbol },
            AlertEvent::Fill { venue: v, symbol: s, side, last_qty, last_price },
        ) => {
            if filter_matches(venue, v) && filter_matches(symbol, s) {
                maybe_fire(
                    rule,
                    st,
                    now_ms,
                    format!("fill {v} {s} side {side} qty {last_qty} @ {last_price}"),
                )
            } else {
                None
            }
        }
        (
            RuleTrigger::OrderRejected,
            AlertEvent::OrderRejected { order_client_id, venue, refusal_id, reason },
        ) => maybe_fire(
            rule,
            st,
            now_ms,
            match refusal_id {
                Some(id) => format!("order {order_client_id} on {venue} rejected [{id}]: {reason}"),
                None => format!("order {order_client_id} on {venue} rejected: {reason}"),
            },
        ),
        (
            RuleTrigger::OrderRejected,
            AlertEvent::OrderDenied { order_client_id, venue, refusal_id, reason },
        ) => maybe_fire(
            rule,
            st,
            now_ms,
            match refusal_id {
                Some(id) => format!("order {order_client_id} on {venue} denied [{id}]: {reason}"),
                None => format!("order {order_client_id} on {venue} denied: {reason}"),
            },
        ),
        (
            RuleTrigger::JournalGrowth,
            AlertEvent::JournalGrowth { total_bytes, segments, bytes_per_hour },
        ) => maybe_fire(
            rule,
            st,
            now_ms,
            format!(
                "journal at {:.1} MiB in {segments} segment(s), {}",
                mebibytes(*total_bytes),
                bytes_per_hour.map_or("its growth not measured yet".to_string(), |rate| format!(
                    "growing {:.1} MiB an hour",
                    mebibytes(rate)
                ))
            ),
        ),
        (RuleTrigger::JournalFenceHalt, AlertEvent::JournalFenceHalt { fault }) => {
            maybe_fire(rule, st, now_ms, format!("trading HALTED: {fault}"))
        }
        _ => None,
    }
}

fn mebibytes(bytes: u64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}
