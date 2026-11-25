//! [`OrderCancelIntent`], WHY a cancel is being issued: the ONE cross-venue classification.
//!
//! The `ExecutionClient` cancel door takes it (`vike_execution::ExecutionClient::cancel_with_intent`), and the
//! gatewayhub's lease wire carries it as it is (`TradeUp::Cancel` and `TradeUp::CancelBatch`, `intent`). Its
//! serde shape is the plain derive, the bare variant name (`"Unspecified"`, `"Routine"`, `"RiskOff"`), pinned
//! by `fixtures/gatewayhub_wire/v1_trade_up.jsonl`: a variant added here is a wire change, and the fixture
//! gate (`crates/vike-gatewayhub-client/tests/wire_tag_fixtures.rs`'s `WIRE_ENUMS`) refuses one no golden
//! frame carries.

/// WHY a cancel is being issued — the ONE cross-venue classification, so a venue that meters its
/// cancels can tell routine ladder churn from an emergency flatten. A venue-side cancel budget is a
/// liveness hazard: Polymarket meters cancel tokens per signer, and a maker that spends its bucket
/// on requote churn is rate-limit-LOCKED at the moment it needs to pull a book.
///
/// ⚠ **[`OrderCancelIntent::Unspecified`] is the DEFAULT and it is the FLATTEN-SAFE value, not a
/// "don't care".** An unclassified cancel gets the emergency treatment — fire it, never hold it
/// back — because shedding a routine cancel costs one stale quote, while shedding an emergency one
/// leaves a live order resting where the operator wanted out. Only [`OrderCancelIntent::Routine`] may
/// be held back, and [`OrderCancelIntent::may_be_shed`] is the ONE place that says so. It stays the
/// flatten-safe default on the wire too.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum OrderCancelIntent {
    /// The caller could not classify this cancel (an operator's DOM click, a tradehub/CLI ticket,
    /// a book-maintenance sweep). Never shed, like [`OrderCancelIntent::RiskOff`]; a SEPARATE variant
    /// so a venue can later act only on a DECLARED emergency (Polymarket's account-wide `cancel-all`,
    /// say) and tell "the operator said get me out" from "nobody said anything".
    #[default]
    Unspecified,
    /// Maker ladder churn / requote. The ONLY intent a venue may hold back under its own budget,
    /// and only ever by reporting a NON-terminal cancel-reject — a shed cancel must never vanish
    /// silently, because the order is still resting.
    Routine,
    /// A declared risk-off action: flatten, market-exit, dead-man trip, safe-state sweep. Never
    /// shed — being able to fire here is the whole point of any reserve a venue holds back.
    RiskOff,
}

impl OrderCancelIntent {
    /// May a venue hold THIS cancel back under its own rate/credit budget? True for
    /// [`OrderCancelIntent::Routine`] and nothing else — the flatten-safe default and a declared
    /// risk-off action both answer `false`.
    ///
    /// One function rather than a `match` per venue: `Unspecified` shedding like `RiskOff` and NOT
    /// like `Routine` is the whole safety property, one `_ =>` arm away from inverted.
    pub fn may_be_shed(self) -> bool {
        matches!(self, OrderCancelIntent::Routine)
    }
}

#[cfg(test)]
mod tests {
    use super::OrderCancelIntent;

    #[test]
    fn the_default_intent_is_the_flatten_safe_one() {
        assert_eq!(OrderCancelIntent::default(), OrderCancelIntent::Unspecified);
        assert!(
            !OrderCancelIntent::default().may_be_shed(),
            "an unclassified cancel is never held back"
        );
    }

    /// The whole safety property in one assertion: ROUTINE and nothing else may be shed.
    #[test]
    fn only_routine_may_be_shed() {
        assert!(OrderCancelIntent::Routine.may_be_shed());
        assert!(!OrderCancelIntent::Unspecified.may_be_shed());
        assert!(!OrderCancelIntent::RiskOff.may_be_shed());
    }
}
