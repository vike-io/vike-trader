//! **What the OMS states about one execution or one step of an order's life**: the FIX drop copy's
//! ExecutionReport, written by the live core right after its fold and journaled as
//! `vike_journal::JournalRecord::ExecutionReport`, so a reader of the journal needs no per-order state to
//! know an order's account, its strategy, its venue order id and its state after the event.
//!
//! - **Self-contained.** Every report carries the account, the venue, the symbol and the order as the
//!   engine holds it AFTER the event (`order`), so one record alone makes a whole row.
//! - **Ours or foreign** ([`ExecutionOwnership`]): only an order this install provably minted
//!   (`crate::orders::client_id::order_client_id_minted_by`, or one the live registry holds) carries a mount
//!   and a strategy code. A foreign execution is recorded with the account whose stream carried it and nothing
//!   else.
//! - **Pure data.** The constructors below only copy what an event, a record or a request already says.

use serde::{Deserialize, Serialize};

use crate::events::{FeeAdjustmentEvent, FillEvent, LiquiditySide, PositionSide};
use crate::orders::client_id::OrderClientIdStrategyCode;
use crate::orders::order::{OrderRequest, OrderType};
use crate::orders::record::{OrderRecord, OrderStatus};

/// Whether an execution belongs to this install: `Ours` when the live registry holds the order or one of this
/// install's order client id sessions minted its id, `Foreign` otherwise (another install's id, a venue-UI
/// order, a reconcile `EXT-…` id, an empty or unreadable id). Only `Ours` carries a mount and a strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ExecutionOwnership {
    Ours,
    Foreign,
}

/// One execution or order step, as the OMS states it right after its fold.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecutionReport {
    /// The journal record whose fold this states: the venue event's `Command`, the submit, the `MintedSubmit`.
    pub caused_by_sequence: u64,
    /// A fill's venue time (`FillEvent::timestamp`), and a fee correction's its fill's
    /// (`FeeAdjustmentEvent::timestamp`); otherwise the cause's `now_ms`.
    pub at_ms: i64,
    pub ownership: ExecutionOwnership,
    /// The account's spelling (`DEFAULT` for the unlabelled one); `None` when nothing names it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    pub venue: String,
    pub symbol: String,
    pub order_client_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub venue_order_id: Option<String>,
    /// The mount the id's strategy code names among the core's mounts; `None` for an order no mount placed,
    /// and for a code no current mount carries (then [`Self::strategy_code`] still names it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mount_id: Option<String>,
    /// The strategy code a v2 id of ours carries (never `0000`): what names a mount removed before the event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strategy_code: Option<OrderClientIdStrategyCode>,
    /// The order AFTER the fold; `None` when the engine holds no such order and no request is in hand.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order: Option<ExecutionReportOrder>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fill: Option<ExecutionReportFill>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fee_correction: Option<ExecutionReportFeeCorrection>,
    /// The words the event carried, whoever said them: the gate's for a deny, the venue's for a reject, a cancel
    /// or a refused cancel or modify, the core's for a terminal it made up. `None` when the event says none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// An order's terms and state after a fold.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecutionReportOrder {
    pub side: i32,
    pub qty: f64,
    pub order_type: OrderType,
    pub status: OrderStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub price: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_price: Option<f64>,
    pub filled_qty: f64,
    pub average_fill_price: f64,
}

impl ExecutionReportOrder {
    /// The order as the engine's record holds it.
    pub fn of_record(record: &OrderRecord) -> Self {
        let r = &record.request;
        ExecutionReportOrder {
            side: r.side,
            qty: r.qty,
            order_type: r.order_type.clone(),
            status: record.status,
            price: r.price,
            trigger_price: r.trigger_price,
            filled_qty: record.filled_qty,
            average_fill_price: record.average_fill_price,
        }
    }

    /// An order the engine never registered (a deny), from the request in hand: nothing filled.
    pub fn of_request(request: &OrderRequest, status: OrderStatus) -> Self {
        ExecutionReportOrder {
            side: request.side,
            qty: request.qty,
            order_type: request.order_type.clone(),
            status,
            price: request.price,
            trigger_price: request.trigger_price,
            filled_qty: 0.0,
            average_fill_price: 0.0,
        }
    }
}

/// One fill, as the venue reported it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecutionReportFill {
    /// The venue's execution id.
    pub trade_id: String,
    pub side: i32,
    pub qty: f64,
    pub price: f64,
    pub commission: f64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub commission_asset: String,
    pub liquidity_side: LiquiditySide,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mark_price: Option<f64>,
    pub position_side: PositionSide,
}

impl ExecutionReportFill {
    pub fn of_fill(fill: &FillEvent) -> Self {
        ExecutionReportFill {
            trade_id: fill.trade_id.to_string(),
            side: fill.side,
            qty: fill.last_qty,
            price: fill.last_price,
            commission: fill.commission,
            commission_asset: fill.commission_asset.to_string(),
            liquidity_side: fill.liquidity_side,
            mark_price: fill.mark_price,
            position_side: fill.position_side,
        }
    }
}

/// A later correction of a fill's fee (decision 0238): how much MORE the execution cost, and the corrected fill's
/// price, at which an inverse contract's fee enters the ledger.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecutionReportFeeCorrection {
    pub corrects_trade_id: String,
    pub commission: f64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub commission_asset: String,
    pub price: f64,
}

impl ExecutionReportFeeCorrection {
    pub fn of_adjustment(adjustment: &FeeAdjustmentEvent) -> Self {
        ExecutionReportFeeCorrection {
            corrects_trade_id: adjustment.trade_id.to_string(),
            commission: adjustment.extra_commission,
            commission_asset: adjustment.commission_asset.to_string(),
            price: adjustment.last_price,
        }
    }
}
