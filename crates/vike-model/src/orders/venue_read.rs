//! What an operator may ASK a venue for through the node, and the words it answers in: the account's
//! balance (with the per-exchange allowance where the venue has one), the venue's OWN list of the
//! account's open orders and the positions it holds.
//!
//! These are READS. Nothing here can place, move or cancel an order, and a bridge that answers one
//! changes no state at the venue. They are asked, never pushed: the reconcile pass (which folds what it
//! finds) is a different thing and stays under its own gate; a read the operator asks for folds nothing.
//!
//! Plain data with serde, shared by the bridge seam (`vike_bridge_core::venue_adapter::VenueAdapter::venue_read`)
//! and the gatewayhub's trade-lease frames; the node's own wire mirrors them as `Wire*` types
//! (`vike_tradehub_client::wire`), as it does every report.

use serde::{Deserialize, Serialize};

/// The most open orders one read carries. A venue that lists more is answered with the first
/// [`MAX_VENUE_OPEN_ORDERS`] and `truncated` set, never silently cut.
pub const MAX_VENUE_OPEN_ORDERS: usize = 500;

/// The most positions one read carries; past it the first [`MAX_VENUE_POSITIONS`] are answered with
/// `truncated` set, never silently cut.
pub const MAX_VENUE_POSITIONS: usize = 500;

/// Which read.
///
/// ⚠ A kind added here is a WIRE change on the gatewayhub's lease frames: a daemon that predates the
/// variant cannot decode `TradeUp::VenueRead` and ENDS the lease, so a new kind owes its own capability
/// string (`vike_gatewayhub_client::proto::FEATURE_GW_TRADE_VENUE_POSITIONS` is `Positions`'s) and the
/// client sends nothing without it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum VenueReadKind {
    /// The account's balance and, where the venue states them, its per-exchange allowances.
    Balance,
    /// The venue's own list of the account's open (resting) orders.
    OpenOrders,
    /// The positions the venue says the account holds (a Polymarket outcome-token holding), account-wide.
    Positions,
}

impl VenueReadKind {
    /// The operator's word for it (what a refusal names, and the verb a command line would take).
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            VenueReadKind::Balance => "balance",
            VenueReadKind::OpenOrders => "open-orders",
            VenueReadKind::Positions => "positions",
        }
    }
}

/// The sentence for a bridge that offers no such read: ONE spelling, so the daemon without an adapter
/// for the venue and a bridge that never wrote the read answer alike.
#[must_use]
pub fn not_offered_words(venue: &str, kind: VenueReadKind) -> String {
    format!("not offered by {venue}: its bridge has no account-wide {} read", kind.word())
}

/// One approval the account has given a contract to move its money.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VenueAllowance {
    /// The approved contract's address, as the venue states it.
    pub spender: String,
    /// The contract in words (`CTF exchange`), or empty when the venue names none.
    pub name: String,
    /// The approved amount in whole units of the balance's asset. Meaningless when `unlimited`.
    pub amount: f64,
    /// The approval is the maximum the chain can state (an "approve forever").
    pub unlimited: bool,
}

/// The account's balance as the venue states it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VenueBalance {
    /// The asset the figure is in (`pUSD`, `USDC`), or empty when the venue's answer names none.
    pub asset: String,
    /// The balance in whole units of `asset`.
    pub balance: f64,
    /// Per-exchange approvals, empty for a venue that has none.
    pub allowances: Vec<VenueAllowance>,
}

/// One resting order as the venue lists it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VenueOpenOrder {
    /// The venue's id for the order.
    pub venue_order_id: String,
    /// Our client order id when the venue echoes it or this process remembers it; `None` for an order
    /// placed anywhere else.
    pub client_order_id: Option<String>,
    /// The instrument, as the venue names it.
    pub symbol: String,
    /// `+1` buy, `-1` sell (the sign the reconcile reports use).
    pub side: i32,
    /// The resting limit price when the venue's list carries one; `None` when this read cannot say.
    pub price: Option<f64>,
    /// The order's size.
    pub qty: f64,
    /// How much of it has filled.
    pub filled_qty: f64,
    /// The order-status word (`ACCEPTED`, `PARTIALLY_FILLED`).
    pub status: String,
    /// When the venue says the order was placed, epoch ms; `0` when it says nothing.
    pub ts_ms: i64,
}

/// One position as the venue lists it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VenuePosition {
    /// The instrument, as the venue names it (a Polymarket outcome token id).
    pub symbol: String,
    /// The market in words (`Solana Up or Down - June 9, 3:40AM-3:45AM ET`), or empty when the venue
    /// names none.
    pub title: String,
    /// Which side of the market is held (`Up`, `Yes`), or empty when the venue names none.
    pub outcome: String,
    /// How many shares are held.
    pub qty: f64,
    /// The average price paid per share, when the venue states it.
    pub avg_price: Option<f64>,
    /// The venue's current price of one share, when it states one.
    pub price: Option<f64>,
    /// What the venue says the holding is worth now, when it states it.
    pub value: Option<f64>,
    /// The market has RESOLVED and the shares can be redeemed. ⚠ A resolution flag, not a winner flag:
    /// a losing leg reads `true` too and pays nothing; `price` and `value` say what a share pays.
    pub redeemable: bool,
}

/// What a read answers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum VenueRead {
    /// The answer to [`VenueReadKind::Balance`].
    Balance(VenueBalance),
    /// The answer to [`VenueReadKind::OpenOrders`]. An empty list is an answer ("nothing rests").
    OpenOrders {
        /// At most [`MAX_VENUE_OPEN_ORDERS`] rows.
        orders: Vec<VenueOpenOrder>,
        /// The venue has more than `orders` holds (a further page, or more rows than the cap).
        truncated: bool,
    },
    /// The answer to [`VenueReadKind::Positions`]. An empty list is an answer ("the wallet holds nothing").
    Positions {
        /// At most [`MAX_VENUE_POSITIONS`] rows.
        positions: Vec<VenuePosition>,
        /// The venue has more than `positions` holds (more rows than the cap).
        truncated: bool,
    },
}

impl VenueRead {
    /// Whether every figure of the answer is a finite number. A JSON writer turns `NaN` and `inf` into
    /// `null`, which a reader of an `f64` refuses, so a frame that carried one would fail to decode
    /// and end the connection it rode: the daemon replaces such an answer with an error in words
    /// before it is written.
    #[must_use]
    pub fn is_finite(&self) -> bool {
        match self {
            VenueRead::Balance(b) => {
                b.balance.is_finite() && b.allowances.iter().all(|a| a.amount.is_finite())
            }
            VenueRead::OpenOrders { orders, .. } => orders.iter().all(|o| {
                o.qty.is_finite() && o.filled_qty.is_finite() && o.price.is_none_or(f64::is_finite)
            }),
            VenueRead::Positions { positions, .. } => positions.iter().all(|p| {
                p.qty.is_finite()
                    && p.avg_price.is_none_or(f64::is_finite)
                    && p.price.is_none_or(f64::is_finite)
                    && p.value.is_none_or(f64::is_finite)
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_figure_that_is_not_a_number_is_caught_before_it_is_written() {
        let balance = |balance, amount| {
            VenueRead::Balance(VenueBalance {
                asset: "pUSD".into(),
                balance,
                allowances: vec![VenueAllowance {
                    spender: String::new(),
                    name: String::new(),
                    amount,
                    unlimited: false,
                }],
            })
        };
        assert!(balance(1.0, 2.0).is_finite());
        assert!(!balance(f64::NAN, 2.0).is_finite());
        assert!(!balance(1.0, f64::INFINITY).is_finite());
        let order = |qty, filled_qty, price| VenueOpenOrder {
            venue_order_id: "1".into(),
            client_order_id: None,
            symbol: "X".into(),
            side: 1,
            price,
            qty,
            filled_qty,
            status: "ACCEPTED".into(),
            ts_ms: 0,
        };
        let book = |o| VenueRead::OpenOrders { orders: vec![o], truncated: false };
        assert!(book(order(1.0, 0.0, Some(0.5))).is_finite());
        assert!(book(order(1.0, 0.0, None)).is_finite());
        assert!(!book(order(f64::NAN, 0.0, None)).is_finite());
        assert!(!book(order(1.0, f64::NEG_INFINITY, None)).is_finite());
        assert!(!book(order(1.0, 0.0, Some(f64::NAN))).is_finite());
    }

    fn position(
        qty: f64,
        avg_price: Option<f64>,
        price: Option<f64>,
        value: Option<f64>,
    ) -> VenuePosition {
        VenuePosition {
            symbol: "148".into(),
            title: "Solana Up or Down".into(),
            outcome: "Down".into(),
            qty,
            avg_price,
            price,
            value,
            redeemable: true,
        }
    }

    #[test]
    fn a_position_figure_that_is_not_a_number_is_caught_before_it_is_written() {
        let book = |p| VenueRead::Positions { positions: vec![p], truncated: false };
        assert!(book(position(11.11, Some(0.21), Some(0.0), Some(0.0))).is_finite());
        assert!(book(position(11.11, None, None, None)).is_finite());
        assert!(!book(position(f64::NAN, None, None, None)).is_finite());
        assert!(!book(position(1.0, Some(f64::INFINITY), None, None)).is_finite());
        assert!(!book(position(1.0, None, Some(f64::NAN), None)).is_finite());
        assert!(!book(position(1.0, None, None, Some(f64::NEG_INFINITY))).is_finite());
        assert!(VenueRead::Positions { positions: Vec::new(), truncated: false }.is_finite());
    }

    #[test]
    fn the_operators_words_are_the_cli_verbs() {
        assert_eq!(VenueReadKind::Balance.word(), "balance");
        assert_eq!(VenueReadKind::OpenOrders.word(), "open-orders");
        assert_eq!(VenueReadKind::Positions.word(), "positions");
    }

    /// The kind is a unit variant of an externally tagged enum, so its spelling on the gatewayhub's lease
    /// frame is the bare variant name: a daemon from before the kind cannot decode it (and ends the lease),
    /// which is why the client sends it only against a daemon that advertises the capability.
    #[test]
    fn the_kind_is_spelled_on_the_wire_by_its_variant_name() {
        assert_eq!(serde_json::to_string(&VenueReadKind::Positions).unwrap(), r#""Positions""#);
        assert_eq!(
            serde_json::to_string(&VenueReadKind::OpenOrders).unwrap(),
            r#""OpenOrders""#,
            "an existing kind's bytes did not move"
        );
    }

    #[test]
    fn a_refusal_names_the_venue_and_the_read() {
        let words = not_offered_words("binance", VenueReadKind::OpenOrders);
        assert!(words.starts_with("not offered by binance"), "{words}");
        assert!(words.contains("open-orders"), "{words}");
    }

    #[test]
    fn every_answer_round_trips_through_json() {
        let balance = VenueRead::Balance(VenueBalance {
            asset: "pUSD".into(),
            balance: 12.5,
            allowances: vec![VenueAllowance {
                spender: "0xabc".into(),
                name: "CTF exchange".into(),
                amount: 0.0,
                unlimited: true,
            }],
        });
        let orders = VenueRead::OpenOrders {
            orders: vec![VenueOpenOrder {
                venue_order_id: "0x1".into(),
                client_order_id: None,
                symbol: "123".into(),
                side: 1,
                price: Some(0.05),
                qty: 5.0,
                filled_qty: 0.0,
                status: "ACCEPTED".into(),
                ts_ms: 1_700_000_000_000,
            }],
            truncated: false,
        };
        let positions = VenueRead::Positions {
            positions: vec![position(11.11, Some(0.2099), Some(0.0), Some(0.0))],
            truncated: true,
        };
        for answer in [balance, orders, positions] {
            let json = serde_json::to_string(&answer).unwrap();
            assert_eq!(serde_json::from_str::<VenueRead>(&json).unwrap(), answer, "{json}");
        }
    }
}
