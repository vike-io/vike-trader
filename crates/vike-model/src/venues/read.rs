//! What an operator may ASK a venue for through the node, and the words it answers in: the account's
//! balance (with the per-exchange allowance where the venue has one), the venue's OWN list of the
//! account's open orders and the positions it holds.
//!
//! These are READS. Nothing here can place, move or cancel an order, and a bridge that answers one
//! changes no state at the venue. They are asked, never pushed: the reconcile pass (which folds what it
//! finds) is a different thing and stays under its own gate; a read the operator asks for folds nothing.
//!
//! Plain data with serde, shared by the bridge seam (`vike_bridge_core::venue_adapter::VenueAdapter::venue_read`),
//! the gatewayhub's trade-lease frames and the node's own wire (`vike_tradehub_client::proto::Response::VenueRead`
//! carries a [`VenueRead`], and `Request::VenueRead` a [`VenueReadKind`]): one type, the same bytes on both wires.

use serde::{Deserialize, Serialize};

use crate::OrderStatusReport;

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
/// string (`vike_gatewayhub_client::proto::FEATURE_GATEWAY_TRADE_VENUE_POSITIONS` is `Positions`'s) and the
/// client sends nothing without it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum VenueReadKind {
    /// The account's balance and, where the venue states them, its per-exchange allowances.
    Balance,
    /// The venue's own list of the account's open (resting) orders.
    OpenOrders,
    /// The positions the venue says the account holds (a Polymarket outcome-token holding, a Deribit
    /// contract), account-wide.
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
pub struct VenueReadAllowance {
    /// The approved contract's address, as the venue states it.
    pub spender: String,
    /// The contract in words (`CTF exchange`), or empty when the venue names none.
    pub name: String,
    /// The approved amount in whole units of the balance's asset. Meaningless when `unlimited`.
    pub amount: f64,
    /// The approval is the maximum the chain can state (an "approve forever").
    pub unlimited: bool,
}

/// What an account holds of one asset, beside [`VenueReadBalance::asset`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VenueReadAssetBalance {
    /// The asset, as the venue names it.
    pub asset: String,
    /// The balance in whole units of `asset`.
    pub balance: f64,
}

/// The account's balance as the venue states it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VenueReadBalance {
    /// The asset the figure is in (`pUSD`, `USDC`), or empty when the venue's answer names none.
    pub asset: String,
    /// The balance in whole units of `asset`.
    pub balance: f64,
    /// Per-exchange approvals, empty for a venue that has none.
    pub allowances: Vec<VenueReadAllowance>,
    /// What else the account holds, on a venue whose account keeps a wallet per currency and states no
    /// total (Deribit). Empty for every other venue, and then absent from the frame, so their bytes did
    /// not move and an older reader skips the key.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub other_assets: Vec<VenueReadAssetBalance>,
}

/// One resting order as the venue lists it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VenueReadOpenOrder {
    /// The venue's id for the order.
    pub venue_order_id: String,
    /// Our order client id when the venue echoes it or this process remembers it; `None` for an order
    /// placed anywhere else.
    pub order_client_id: Option<String>,
    /// The instrument, as the venue names it.
    pub symbol: String,
    /// `+1` buy, `-1` sell (the sign the reconcile reports use).
    pub side: i32,
    /// The resting limit price when the venue's list carries one; `None` when this read cannot say.
    pub price: Option<f64>,
    /// A trigger kind's trigger level (a stop's, a take-profit's) when the venue's list carries one, in
    /// [`crate::OrderRequest::trigger_price`]'s meaning; `None` when not stated. This and `order_type` are
    /// absent from the frame when unstated, so a row without them keeps its bytes and an older reader
    /// skips the keys.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_price: Option<f64>,
    /// The order kind as [`crate::OrderType::as_str`] spells it (`limit`, `stop`, `take_profit`; a venue
    /// word with no twin verbatim); empty when the venue's list does not say.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub order_type: String,
    /// The order's size.
    pub qty: f64,
    /// How much of it has filled.
    pub filled_qty: f64,
    /// The order-status word (`ACCEPTED`, `PARTIALLY_FILLED`).
    pub status: String,
    /// When the venue says the order was placed, epoch ms; `0` when it says nothing.
    pub timestamp_ms: i64,
}

/// **The reconcile reports as an open-orders read's rows**, cut at [`MAX_VENUE_OPEN_ORDERS`] with the
/// cut said (`true` when there were more). The ONE copy of the report-to-row mapping: every bridge whose
/// read goes through its reconcile parser calls it, so a column added here reaches every venue at once. A
/// venue's own cut (a further page) or a column its raw list states better than the report is that
/// bridge's, applied to what this returns.
#[must_use]
pub fn open_orders_read(reports: Vec<OrderStatusReport>) -> (Vec<VenueReadOpenOrder>, bool) {
    let truncated = reports.len() > MAX_VENUE_OPEN_ORDERS;
    let orders = reports
        .into_iter()
        .take(MAX_VENUE_OPEN_ORDERS)
        .map(|report| VenueReadOpenOrder {
            venue_order_id: report.venue_order_id.to_string(),
            order_client_id: report.order_client_id,
            symbol: report.symbol,
            side: report.side,
            price: report.price,
            trigger_price: report.trigger_price,
            order_type: report.order_type.as_str().to_string(),
            qty: report.qty,
            filled_qty: report.filled_qty,
            status: report.status.as_str().to_string(),
            timestamp_ms: report.timestamp,
        })
        .collect();
    (orders, truncated)
}

/// One position as the venue lists it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VenueReadPosition {
    /// The instrument, as the venue names it (a Polymarket outcome token id).
    pub symbol: String,
    /// The market in words (`Solana Up or Down - June 9, 3:40AM-3:45AM ET`), or empty when the venue
    /// names none.
    pub title: String,
    /// Which side of the market is held (`Up`, `Yes`), or empty when the venue names none.
    pub outcome: String,
    /// How many shares are held; negative for a short, on a venue that holds shorts.
    pub qty: f64,
    /// The average price paid per share, when the venue states it.
    pub average_price: Option<f64>,
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
    Balance(VenueReadBalance),
    /// The answer to [`VenueReadKind::OpenOrders`]. An empty list is an answer ("nothing rests").
    OpenOrders {
        /// At most [`MAX_VENUE_OPEN_ORDERS`] rows.
        orders: Vec<VenueReadOpenOrder>,
        /// The venue has more than `orders` holds (a further page, or more rows than the cap).
        truncated: bool,
    },
    /// The answer to [`VenueReadKind::Positions`]. An empty list is an answer ("the wallet holds nothing").
    Positions {
        /// At most [`MAX_VENUE_POSITIONS`] rows.
        positions: Vec<VenueReadPosition>,
        /// The venue has more than `positions` holds (more rows than the cap).
        truncated: bool,
    },
}

impl VenueRead {
    /// The kind of read this answers.
    #[must_use]
    pub const fn kind(&self) -> VenueReadKind {
        match self {
            VenueRead::Balance(_) => VenueReadKind::Balance,
            VenueRead::OpenOrders { .. } => VenueReadKind::OpenOrders,
            VenueRead::Positions { .. } => VenueReadKind::Positions,
        }
    }

    /// Whether every figure of the answer is a finite number. A JSON writer turns `NaN` and `inf` into
    /// `null`, which a reader of an `f64` refuses, so a frame that carried one would fail to decode
    /// and end the connection it rode: the daemon replaces such an answer with an error in words
    /// before it is written.
    #[must_use]
    pub fn is_finite(&self) -> bool {
        match self {
            VenueRead::Balance(b) => {
                b.balance.is_finite()
                    && b.allowances.iter().all(|a| a.amount.is_finite())
                    && b.other_assets.iter().all(|o| o.balance.is_finite())
            }
            VenueRead::OpenOrders { orders, .. } => orders.iter().all(|o| {
                o.qty.is_finite()
                    && o.filled_qty.is_finite()
                    && o.price.is_none_or(f64::is_finite)
                    && o.trigger_price.is_none_or(f64::is_finite)
            }),
            VenueRead::Positions { positions, .. } => positions.iter().all(|p| {
                p.qty.is_finite()
                    && p.average_price.is_none_or(f64::is_finite)
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
            VenueRead::Balance(VenueReadBalance {
                asset: "pUSD".into(),
                balance,
                allowances: vec![VenueReadAllowance {
                    spender: String::new(),
                    name: String::new(),
                    amount,
                    unlimited: false,
                }],
                other_assets: Vec::new(),
            })
        };
        assert!(balance(1.0, 2.0).is_finite());
        assert!(!balance(f64::NAN, 2.0).is_finite());
        assert!(!balance(1.0, f64::INFINITY).is_finite());
        let wallets = |balance| {
            VenueRead::Balance(VenueReadBalance {
                asset: "BTC".into(),
                balance: 1.0,
                allowances: Vec::new(),
                other_assets: vec![VenueReadAssetBalance { asset: "USDC".into(), balance }],
            })
        };
        assert!(wallets(2.0).is_finite());
        assert!(!wallets(f64::NAN).is_finite(), "another wallet's figure is a figure too");
        let order = |qty, filled_qty, price| VenueReadOpenOrder {
            venue_order_id: "1".into(),
            order_client_id: None,
            symbol: "X".into(),
            side: 1,
            price,
            trigger_price: None,
            order_type: String::new(),
            qty,
            filled_qty,
            status: "ACCEPTED".into(),
            timestamp_ms: 0,
        };
        let book = |o| VenueRead::OpenOrders { orders: vec![o], truncated: false };
        assert!(book(order(1.0, 0.0, Some(0.5))).is_finite());
        assert!(book(order(1.0, 0.0, None)).is_finite());
        assert!(!book(order(f64::NAN, 0.0, None)).is_finite());
        assert!(!book(order(1.0, f64::NEG_INFINITY, None)).is_finite());
        assert!(!book(order(1.0, 0.0, Some(f64::NAN))).is_finite());
        let stop = |trigger_price| VenueReadOpenOrder { trigger_price, ..order(1.0, 0.0, None) };
        assert!(book(stop(Some(1.2412))).is_finite());
        assert!(!book(stop(Some(f64::INFINITY))).is_finite(), "a trigger is a figure too");
    }

    /// A row that states no trigger and no kind writes exactly what it wrote before the two fields, and a
    /// frame from before them still reads; a stop's row carries both and round-trips.
    #[test]
    fn the_trigger_and_the_kind_are_additive_on_the_frame() {
        let limit = VenueReadOpenOrder {
            venue_order_id: "0xabc".into(),
            order_client_id: None,
            symbol: "6710259892".into(),
            side: 1,
            price: Some(0.2),
            trigger_price: None,
            order_type: String::new(),
            qty: 5.0,
            filled_qty: 0.0,
            status: "ACCEPTED".into(),
            timestamp_ms: 1_700_000_000_000,
        };
        let old_bytes = r#"{"venue_order_id":"0xabc","order_client_id":null,"symbol":"6710259892","side":1,"price":0.2,"qty":5.0,"filled_qty":0.0,"status":"ACCEPTED","timestamp_ms":1700000000000}"#;
        assert_eq!(serde_json::to_string(&limit).unwrap(), old_bytes);
        assert_eq!(serde_json::from_str::<VenueReadOpenOrder>(old_bytes).unwrap(), limit);
        let stop = VenueReadOpenOrder {
            price: None,
            trigger_price: Some(1.2412),
            order_type: "stop".into(),
            ..limit
        };
        let json = serde_json::to_string(&stop).unwrap();
        assert!(
            json.contains(r#""price":null,"trigger_price":1.2412,"order_type":"stop","#),
            "{json}"
        );
        assert_eq!(serde_json::from_str::<VenueReadOpenOrder>(&json).unwrap(), stop);
    }

    fn position(
        qty: f64,
        average_price_value: Option<f64>,
        price: Option<f64>,
        value: Option<f64>,
    ) -> VenueReadPosition {
        VenueReadPosition {
            symbol: "148".into(),
            title: "Solana Up or Down".into(),
            outcome: "Down".into(),
            qty,
            average_price: average_price_value,
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

    /// Every column of a report lands in its row (the kind and the status in their own words), and a
    /// list one row past the cap is cut to the cap and said to be cut; one AT the cap is not.
    #[test]
    fn a_report_becomes_its_row_and_the_cap_is_said() {
        let report = |n: usize| OrderStatusReport {
            venue: "bybit".into(),
            symbol: "BTCUSDT".into(),
            venue_order_id: format!("v{n}").into(),
            order_client_id: Some("c1".into()),
            side: -1,
            order_type: crate::OrderType::Stop,
            qty: 2.0,
            filled_qty: 0.5,
            average_price: 100.0,
            status: crate::OrderStatus::PartiallyFilled,
            timestamp: 1_700_000_000_000,
            price: Some(99.5),
            trigger_price: Some(98.0),
            trail: Some(1.0),
            trail_percent: None,
        };
        let (rows, cut) = open_orders_read(vec![report(0)]);
        assert!(!cut);
        assert_eq!(
            rows,
            vec![VenueReadOpenOrder {
                venue_order_id: "v0".into(),
                order_client_id: Some("c1".into()),
                symbol: "BTCUSDT".into(),
                side: -1,
                price: Some(99.5),
                trigger_price: Some(98.0),
                order_type: "stop".into(),
                qty: 2.0,
                filled_qty: 0.5,
                status: "PARTIALLY_FILLED".into(),
                timestamp_ms: 1_700_000_000_000,
            }]
        );
        let (rows, cut) = open_orders_read((0..MAX_VENUE_OPEN_ORDERS).map(report).collect());
        assert_eq!((rows.len(), cut), (MAX_VENUE_OPEN_ORDERS, false), "at the cap nothing is cut");
        let (rows, cut) = open_orders_read((0..=MAX_VENUE_OPEN_ORDERS).map(report).collect());
        assert_eq!((rows.len(), cut), (MAX_VENUE_OPEN_ORDERS, true), "one past the cap is cut");
        assert_eq!(
            rows.last().map(|o| o.venue_order_id.as_str()),
            Some("v499"),
            "the first rows stay"
        );
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

    /// A venue with one figure writes exactly what it wrote before the field, and a frame from before
    /// the field still reads. A venue with several round-trips them.
    #[test]
    fn the_other_wallets_are_additive_on_the_frame() {
        let one = VenueReadBalance {
            asset: "USDC".into(),
            balance: 1.5,
            allowances: Vec::new(),
            other_assets: Vec::new(),
        };
        let old_bytes = r#"{"asset":"USDC","balance":1.5,"allowances":[]}"#;
        assert_eq!(serde_json::to_string(&one).unwrap(), old_bytes);
        assert_eq!(serde_json::from_str::<VenueReadBalance>(old_bytes).unwrap(), one);
        let several = VenueReadBalance {
            other_assets: vec![VenueReadAssetBalance { asset: "BTC".into(), balance: 2.0 }],
            ..one
        };
        let json = serde_json::to_string(&several).unwrap();
        assert_eq!(serde_json::from_str::<VenueReadBalance>(&json).unwrap(), several);
    }

    #[test]
    fn every_answer_round_trips_through_json() {
        let balance = VenueRead::Balance(VenueReadBalance {
            asset: "pUSD".into(),
            balance: 12.5,
            allowances: vec![VenueReadAllowance {
                spender: "0xabc".into(),
                name: "CTF exchange".into(),
                amount: 0.0,
                unlimited: true,
            }],
            other_assets: Vec::new(),
        });
        let orders = VenueRead::OpenOrders {
            orders: vec![VenueReadOpenOrder {
                venue_order_id: "0x1".into(),
                order_client_id: None,
                symbol: "123".into(),
                side: 1,
                price: Some(0.05),
                trigger_price: None,
                order_type: String::new(),
                qty: 5.0,
                filled_qty: 0.0,
                status: "ACCEPTED".into(),
                timestamp_ms: 1_700_000_000_000,
            }],
            truncated: false,
        };
        let positions = VenueRead::Positions {
            positions: vec![position(11.11, Some(0.2099), Some(0.0), Some(0.0))],
            truncated: true,
        };
        for (answer, kind) in [
            (balance, VenueReadKind::Balance),
            (orders, VenueReadKind::OpenOrders),
            (positions, VenueReadKind::Positions),
        ] {
            assert_eq!(answer.kind(), kind);
            let json = serde_json::to_string(&answer).unwrap();
            assert_eq!(serde_json::from_str::<VenueRead>(&json).unwrap(), answer, "{json}");
        }
    }
}
