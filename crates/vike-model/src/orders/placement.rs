//! One order a strategy places through the portable order seam (`crate::OrderBroker`), and the ONE
//! translation of it into the terms an order request states ([`OrderPlacement::request_terms`]). The
//! simulator's `OrderBrokerSimulator` and the live core's `LiveBroker` both read that translation, so a kind
//! added here, or a price moved from one field to another, reaches both engines at once.

use crate::orders::order::OrderType;

/// One order an [`crate::OrderBroker`] places under a tag: a kind of the order layer
/// ([`crate::OrderKind`]) with the prices that kind states. The side, the size and whether it may
/// only reduce are the verb's ([`crate::OrderBroker::place`]).
///
/// Serde for ONE wire, the pipe between a tradehub and a sandboxed strategy process it spawned (two
/// copies of one binary, nothing stored): `vike-tradehub-core`'s `remote_strategy` carries a placed
/// order across it as it is.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum OrderPlacement {
    /// A market order.
    Market,
    /// A limit at `price`.
    Limit { price: f64 },
    /// A stop at `trigger`: a market once the price trades through it.
    Stop { trigger: f64 },
    /// A stop at `trigger` whose fill is a limit at `limit` (decision 0257's law).
    StopLimit { trigger: f64, limit: f64 },
    /// An if-touched order at `trigger`: with a `limit` it rests as that limit once touched, with none
    /// it is a market once touched.
    TakeProfit { trigger: f64, limit: Option<f64> },
    /// A trailing stop `trail` behind the best price since `activation` was reached (`None`: since it
    /// rests), decision 0261.
    Trailing { trail: f64, activation: Option<f64> },
    /// A pegged order (`crate::OrderType::Pegged`): it rests at `peg`'s anchor moved by its offset
    /// (`crate::OrderRequest::peg`) and states no price.
    Pegged { peg: crate::OrderPeg },
    /// A limit at `price` that shows `display` of its size (`crate::OrderRequest::display_quantity`, an
    /// iceberg; 0 is a hidden order), judged by `crate::display_refusal`.
    Iceberg { price: f64, display: f64 },
}

impl OrderPlacement {
    /// The request this order is, as `(order type, limit price, trigger, trail)`: the limit is what
    /// `crate::OrderRequest::price` carries, the trigger `crate::OrderRequest::trigger_price` (a
    /// trailing kind's activation level), the trail `crate::OrderRequest::trail`. An iceberg is a
    /// limit at its price (its visible size rides `crate::OrderRequest::display_quantity`, which this
    /// leaves to the caller), a pegged order states no price.
    pub fn request_terms(self) -> (OrderType, Option<f64>, Option<f64>, Option<f64>) {
        match self {
            OrderPlacement::Market => (OrderType::Market, None, None, None),
            OrderPlacement::Limit { price } | OrderPlacement::Iceberg { price, .. } => {
                (OrderType::Limit, Some(price), None, None)
            }
            OrderPlacement::Stop { trigger } => (OrderType::Stop, None, Some(trigger), None),
            OrderPlacement::StopLimit { trigger, limit } => {
                (OrderType::StopLimit, Some(limit), Some(trigger), None)
            }
            OrderPlacement::TakeProfit { trigger, limit } => {
                (OrderType::TakeProfit, limit, Some(trigger), None)
            }
            OrderPlacement::Trailing { trail, activation } => {
                (OrderType::Trailing, None, activation, Some(trail))
            }
            OrderPlacement::Pegged { .. } => (OrderType::Pegged, None, None, None),
        }
    }
}

#[cfg(test)]
#[path = "placement_tests.rs"]
mod tests;
