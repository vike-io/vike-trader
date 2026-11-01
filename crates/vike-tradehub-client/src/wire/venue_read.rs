//! The node wire's mirrors of an operator's READ of one account at its venue
//! ([`crate::proto::Request::VenueRead`] → [`crate::proto::Response::VenueRead`]): the account's
//! balance (with the allowance each exchange contract holds, where the venue has one), the venue's
//! OWN list of the account's open orders and the positions it holds.
//!
//! These are the shapes `vike_model::VenueRead` and its parts have on the gatewayhub's lease frames,
//! restated here because this crate links no `vike-model` (the light thin-client wire crate): the node
//! (`vike-tradehub`'s `server/venue_read.rs`) converts one to the other, field for field, and a field
//! added on one side is a field added on the other. Plain data; nothing here can place, move or cancel
//! an order.

use serde::{Deserialize, Serialize};

/// Which read an operator asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WireVenueReadKind {
    /// The account's balance and, where the venue states them, its per-exchange allowances.
    Balance,
    /// The venue's own list of the account's open (resting) orders.
    OpenOrders,
    /// The positions the venue says the account holds. ⚠ Owes its OWN capability word
    /// ([`crate::proto::FEATURE_VENUE_READ_POSITIONS`]): a node that has only `venue-read` cannot decode
    /// this kind's name, so the client asks for it only of a node that advertises the new word.
    Positions,
}

impl WireVenueReadKind {
    /// The operator's word for it: the verb `vike-cli trade venue` takes, and what a refusal names.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            WireVenueReadKind::Balance => "balance",
            WireVenueReadKind::OpenOrders => "open-orders",
            WireVenueReadKind::Positions => "positions",
        }
    }
}

/// One approval the account has given a contract to move its money.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireVenueAllowance {
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
pub struct WireVenueBalance {
    /// The asset the figure is in (`pUSD`, `USDC`), or empty when the venue's answer names none.
    pub asset: String,
    /// The balance in whole units of `asset`.
    pub balance: f64,
    /// Per-exchange approvals, empty for a venue that has none.
    pub allowances: Vec<WireVenueAllowance>,
}

/// One resting order as the venue lists it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireVenueOpenOrder {
    /// The venue's id for the order.
    pub venue_order_id: String,
    /// Our client order id when the venue echoes it or the gatewayhub remembers it; `None` for an
    /// order placed anywhere else.
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
pub struct WireVenuePosition {
    /// The instrument, as the venue names it (a Polymarket outcome token id).
    pub symbol: String,
    /// The market in words, or empty when the venue names none.
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
    /// The market has RESOLVED and the shares can be redeemed. ⚠ A resolution flag, not a winner flag: a
    /// losing leg reads `true` too and pays nothing; `price` and `value` say what a share pays.
    pub redeemable: bool,
}

/// What a read answers: the [`crate::proto::Response::VenueRead`] payload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum WireVenueRead {
    /// The answer to [`WireVenueReadKind::Balance`].
    Balance(WireVenueBalance),
    /// The answer to [`WireVenueReadKind::OpenOrders`]. An empty list is an answer ("nothing rests").
    OpenOrders {
        /// The venue's rows, at most the gatewayhub's cap.
        orders: Vec<WireVenueOpenOrder>,
        /// The venue has more than `orders` holds (a further page, or more rows than the cap).
        truncated: bool,
    },
    /// The answer to [`WireVenueReadKind::Positions`]. An empty list is an answer ("the wallet holds
    /// nothing").
    Positions {
        /// The venue's rows, at most the gatewayhub's cap.
        positions: Vec<WireVenuePosition>,
        /// The venue has more than `positions` holds (more rows than the cap).
        truncated: bool,
    },
}

impl WireVenueRead {
    /// The kind of read this answers.
    #[must_use]
    pub const fn kind(&self) -> WireVenueReadKind {
        match self {
            WireVenueRead::Balance(_) => WireVenueReadKind::Balance,
            WireVenueRead::OpenOrders { .. } => WireVenueReadKind::OpenOrders,
            WireVenueRead::Positions { .. } => WireVenueReadKind::Positions,
        }
    }
}
