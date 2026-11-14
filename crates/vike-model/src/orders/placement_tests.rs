use super::OrderPlacement;
use crate::{OrderPeg, OrderPegAnchor, OrderType};

/// Every kind's terms, written out: the table `LiveBroker` and `OrderBrokerSimulator` each spelled by hand before
/// the two read this one. A row that moves here moves both engines' requests.
#[test]
fn every_kind_states_the_terms_its_request_carries() {
    let peg = OrderPeg { anchor: OrderPegAnchor::Mid, offset: 0.0 };
    let rows = [
        (OrderPlacement::Market, (OrderType::Market, None, None, None)),
        (OrderPlacement::Limit { price: 100.0 }, (OrderType::Limit, Some(100.0), None, None)),
        (OrderPlacement::Stop { trigger: 95.0 }, (OrderType::Stop, None, Some(95.0), None)),
        (
            OrderPlacement::StopLimit { trigger: 95.0, limit: 94.5 },
            (OrderType::StopLimit, Some(94.5), Some(95.0), None),
        ),
        (
            OrderPlacement::TakeProfit { trigger: 105.0, limit: Some(105.5) },
            (OrderType::TakeProfit, Some(105.5), Some(105.0), None),
        ),
        (
            OrderPlacement::TakeProfit { trigger: 105.0, limit: None },
            (OrderType::TakeProfit, None, Some(105.0), None),
        ),
        (
            OrderPlacement::Trailing { trail: 2.0, activation: Some(110.0) },
            (OrderType::Trailing, None, Some(110.0), Some(2.0)),
        ),
        (
            OrderPlacement::Trailing { trail: 2.0, activation: None },
            (OrderType::Trailing, None, None, Some(2.0)),
        ),
        (OrderPlacement::Pegged { peg }, (OrderType::Pegged, None, None, None)),
        (
            OrderPlacement::Iceberg { price: 99.0, display: 1.0 },
            (OrderType::Limit, Some(99.0), None, None),
        ),
    ];
    for (order, terms) in rows {
        assert_eq!(order.request_terms(), terms, "{order:?}");
    }
}
