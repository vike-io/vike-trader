//! The EXECUTION STREAM's frames: the FIX drop copy a tradehub sends its datahub after
//! [`Request::ExecutionStreamOpen`](crate::proto::Request::ExecutionStreamOpen), and the datahub's two answers.
//!
//! The tradehub sends every execution and order-lifecycle event its OMS journaled, each record self-contained
//! (the order's state AFTER the event, its account and its mount ride on it) and numbered by its journal
//! sequence. The datahub stores each one idempotently, keeps one cursor per journal and asks "everything since
//! N" after a gap. So neither end holds per-order state, and the tradehub never waits on the datahub.
//!
//! # The grammar after the open
//!
//! The open's reply ([`Response::ExecutionStreamOpened`](crate::proto::Response::ExecutionStreamOpened)) is the
//! last positional frame the socket carries, as the strategy lane's is. After it the tradehub sends only
//! [`ExecutionStreamUp`] and the datahub only [`ExecutionStreamDown`], each one length-prefixed JSON frame
//! (`vike_hub_protocol::frame`). The datahub reads an up frame under
//! [`EXECUTION_STREAM_FRAME_CEILING_BYTES`](crate::proto::EXECUTION_STREAM_FRAME_CEILING_BYTES).
//!
//! # Sequence numbers
//!
//! - [`ExecutionRecordWire::sequence`] is the journal record's own sequence. It strictly rises and is NOT
//!   contiguous, because a record that is no execution is not sent.
//! - [`ExecutionRecordWire::previous_sequence`] is the sequence of the record sent before it. The first record
//!   after an open carries the open reply's `stored_through`, and the first after [`ExecutionStreamDown::Resend`]
//!   carries its `after_sequence`: "since N" is exclusive.
//! - **The chain START is `None`, and only while nothing is stored.** A journal's first record is sequence 0, so no
//!   number can mean "nothing before it": a datahub that holds nothing of a journal answers the open with
//!   `stored_through: None`, the first record sent then carries `previous_sequence: None`, and a `Resend` from
//!   nothing says `after_sequence: None`. The datahub takes a `None` record as the chain's start ONLY while it holds
//!   nothing of that journal; once it holds anything, `None` is a mismatch like any other.
//! - The datahub checks `previous_sequence` against the last record it received. On a mismatch it drops the
//!   rest of the frame and answers `Resend { after_sequence: stored_through }`.
//! - [`ExecutionStreamDown::Stored`] says every record up to its sequence is in the store, or was skipped by a
//!   stated rule (a symbol the store cannot name, a forecast below the disk floor, a [`ExecutionStreamUp::Gap`]).
//!   It is the tradehub's new journal floor.
//!
//! # Versioning
//!
//! The verbs are negotiated by [`FEATURE_EXECUTION_STREAM`](crate::proto::FEATURE_EXECUTION_STREAM) (decision
//! 0112): a client refuses LOCALLY, in words and sending nothing, a datahub that does not advertise it. Inside
//! the stream a field added to a struct here is `#[serde(default)]` and skipped when empty (no
//! `deny_unknown_fields`, the crate's rule), and a new [`ExecutionRecordBodyWire`] kind gets its own feature
//! string: a publisher facing a datahub without it sends nothing past the first such record, so the floor
//! holds. The goldens are `fixtures/datahub_wire/v1_execution_stream_*.jsonl`, read by
//! `tests/execution_stream_wire.rs`.
//!
//! # Names
//!
//! A struct here takes its compiled twin's name plus `Wire`. The two frame enums have no compiled twin, so
//! they follow the tube's `StreamUp` / `StreamDown`. Where the compiled type's serde IS the wire it rides as it is,
//! with no twin: an account's open position ([`AccountPosition`], its side `PositionSide`'s upper-case word) and a
//! report's ownership ([`ExecutionOwnership`]). Every string is the stored row's spelling (an order's `status` is
//! `OrderStatus::as_str`'s word), so the datahub maps a record to its rows with no table.

use serde::{Deserialize, Serialize};
use vike_model::AccountPosition;
use vike_model::orders::ExecutionOwnership;

/// The digits of a journal id: 32 lowercase hex, the identity of one journal DIRECTORY. It is the id the
/// journal's own `execution_stream.json` holds (`vike_journal::JournalExecutionStream`'s `journal_id`), minted
/// once as 16 random bytes in hex by `vike_journal::JournalExecutionStream::mint_journal_id`, the one mint the
/// tradehub's publisher and the core's boot both call.
pub const EXECUTION_STREAM_JOURNAL_ID_DIGITS: usize = 32;

/// A frame the TRADEHUB sends after the open.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ExecutionStreamUp {
    /// Records in ascending sequence, at most
    /// [`EXECUTION_STREAM_RECORDS_PER_FRAME`](crate::proto::EXECUTION_STREAM_RECORDS_PER_FRAME), chained by
    /// their `previous_sequence`.
    Records(Vec<ExecutionRecordWire>),
    /// The journal no longer holds what the datahub lacks (it was pruned past the hold): the records after
    /// `after_sequence` and before `resumes_at_sequence` are lost, and the next record chains from
    /// `after_sequence`. The datahub records the gap and says it at ERROR.
    Gap {
        /// The datahub's `stored_through` when the gap was found.
        after_sequence: u64,
        /// The journal's oldest record: the first one the stream still holds.
        resumes_at_sequence: u64,
        /// The time of the last record the datahub stored, when it said one.
        lost_from_ms: Option<i64>,
        /// The time of the journal's oldest record.
        lost_to_ms: Option<i64>,
    },
    /// Nothing to send for [`EXECUTION_STREAM_HEARTBEAT`](crate::proto::EXECUTION_STREAM_HEARTBEAT): the
    /// journal's newest durable record, so the datahub can say how far behind it is.
    Heartbeat {
        /// The newest DURABLE record of the journal.
        head_sequence: u64,
    },
}

/// A frame the DATAHUB sends after the open.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExecutionStreamDown {
    /// The acknowledgement: every record up to `through_sequence` is in the store (or skipped by a stated
    /// rule). The tradehub's new journal floor.
    Stored {
        /// The last record stored.
        through_sequence: u64,
    },
    /// "Everything since N": resend from the record after `after_sequence` (exclusive).
    Resend {
        /// The last record the datahub holds; the next record sent chains from it. `None`: it holds nothing of the
        /// journal, so the resend starts at its first record and chains from `None`.
        after_sequence: Option<u64>,
    },
}

/// One journal record the stream carries.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecutionRecordWire {
    /// The journal record's own sequence (`vike_journal::record_sequence`).
    pub sequence: u64,
    /// The sequence of the record sent before this one, or the `N` the stream resumed from; `None` for the chain's
    /// start, the first record sent while the datahub holds nothing of the journal.
    pub previous_sequence: Option<u64>,
    /// The journal record's `now_ms`: when vike recorded it.
    pub recorded_ms: i64,
    /// What the record says.
    pub body: ExecutionRecordBodyWire,
}

/// What one streamed record says.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ExecutionRecordBodyWire {
    /// What the OMS stated after one event: an execution, an order's new state, or both. BOXED for
    /// `clippy::large_enum_variant`; the wire shape is unchanged.
    Report(Box<ExecutionReportWire>),
    /// A forecast a live strategy published.
    Forecast(ExecutionForecastWire),
    /// Reports the boot could neither regenerate nor rule out: the executions in the window may be incomplete.
    Unverified(ExecutionUnverifiedWire),
    /// Every account's open positions at a tradehub boot, or why they are unknown: the statement an account's
    /// window opens from (`vike_journal::JournalRecord::PositionSnapshot`). Its time is the record's `recorded_ms`.
    /// It rides [`FEATURE_EXECUTION_STREAM`](crate::proto::FEATURE_EXECUTION_STREAM) with no feature of its own: no
    /// build that streams had shipped when it was added.
    PositionSnapshot(ExecutionPositionSnapshotWire),
}

/// A tradehub boot's position snapshot: one row per account.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecutionPositionSnapshotWire {
    /// Every account the boot's core knew of.
    pub accounts: Vec<ExecutionPositionSnapshotAccountWire>,
}

/// One account of a position snapshot (`vike_journal::JournalPositionSnapshotAccount`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecutionPositionSnapshotAccountWire {
    /// The venue's roster slug.
    pub venue: String,
    /// The account, in the `account` column's spelling (`DEFAULT` for the unlabelled one).
    pub account: String,
    /// Its open positions, each on the account's venue; empty with no [`Self::unknown`]: flat.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub positions: Vec<AccountPosition>,
    /// Why the tradehub could not know them, in words; `None` when [`Self::positions`] is the account's book.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unknown: Option<String>,
}

/// **One account's position snapshot as the datahub stores it**, the answer of
/// [`Request::ExecutionAccountPositions`](crate::proto::Request::ExecutionAccountPositions): the latest at or before
/// the asked time, per account (a `vike_secrets::execution_streams::ExecutionAccountPositionsRow`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecutionAccountPositionsWire {
    /// Which of the datahub's journals it was journaled in (`execution_stream`'s `generation`).
    pub generation: i64,
    /// Its journal sequence: with [`Self::generation`], what splits the account's stream rows into before and after
    /// it (`vike_data::ExecFillRow::after_position_snapshot`).
    pub sequence: u64,
    /// When the tradehub took it (the record's `recorded_ms`).
    pub as_of_ms: i64,
    /// The venue's roster slug.
    pub venue: String,
    /// The account, in the `account` column's spelling.
    pub account: String,
    /// Its open positions then, each on the account's venue; empty with no [`Self::unknown`]: flat.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub positions: Vec<AccountPosition>,
    /// Why the tradehub could not know them; `None` when [`Self::positions`] is the account's book.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unknown: Option<String>,
}

/// The OMS's report of one event, self-contained: the datahub writes its rows from this record alone.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecutionReportWire {
    /// The journal record whose fold this states: the venue event's command, the submit, the minted submit.
    pub caused_by_sequence: u64,
    /// A fill's venue time, and a fee correction's its fill's; otherwise the cause's `now_ms`.
    pub at_ms: i64,
    /// Ours or foreign, decided by the tradehub from the order client id alone. Only an
    /// [`ExecutionOwnership::Ours`] report carries a mount.
    pub ownership: ExecutionOwnership,
    /// The account, in the `account` column's spelling (`DEFAULT` for the unlabelled one); `None` when no
    /// record names it.
    pub account: Option<String>,
    /// The venue's roster slug.
    pub venue: String,
    /// The instrument's user name; the store name is the datahub's.
    pub symbol: String,
    /// The order client id the event names; empty for a fill the venue gave none.
    pub order_client_id: String,
    /// The venue's own order id, when the OMS knows it.
    pub venue_order_id: Option<String>,
    /// The strategy mount the order client id names; `None` for an order no strategy placed.
    pub mount_id: Option<String>,
    /// The order AFTER the fold; `None` when the OMS holds no such order.
    pub order: Option<ExecutionReportOrderWire>,
    /// The execution, for a fill.
    pub fill: Option<ExecutionReportFillWire>,
    /// The fee correction, for a fee adjustment (decision 0238).
    pub fee_correction: Option<ExecutionReportFeeCorrectionWire>,
}

/// The wire form of what the OMS journaled, every word in the stored row's spelling. The report's `reason` rides
/// the order (the `exec_order` row stores it), and its `strategy_code` does not ride at all: the publisher has
/// already named a removed mount from it (`vike_secrets::strategy_code::strategy_name_for`) into `mount_id`.
impl From<&vike_model::orders::ExecutionReport> for ExecutionReportWire {
    fn from(report: &vike_model::orders::ExecutionReport) -> Self {
        use vike_model::events::PositionSide;
        ExecutionReportWire {
            caused_by_sequence: report.caused_by_sequence,
            at_ms: report.at_ms,
            ownership: report.ownership,
            account: report.account.clone(),
            venue: report.venue.clone(),
            symbol: report.symbol.clone(),
            order_client_id: report.order_client_id.clone(),
            venue_order_id: report.venue_order_id.clone(),
            mount_id: report.mount_id.clone(),
            order: report.order.as_ref().map(|o| ExecutionReportOrderWire {
                side: o.side,
                qty: o.qty,
                order_type: o.order_type.as_str().to_owned(),
                status: o.status.as_str().to_owned(),
                price: o.price,
                trigger_price: o.trigger_price,
                filled_qty: o.filled_qty,
                average_fill_price: o.average_fill_price,
                reason: report.reason.clone(),
            }),
            fill: report.fill.as_ref().map(|f| ExecutionReportFillWire {
                trade_id: f.trade_id.clone(),
                side: f.side,
                qty: f.qty,
                price: f.price,
                commission: f.commission,
                commission_asset: f.commission_asset.clone(),
                liquidity_side: f.liquidity_side.to_string(),
                mark_price: f.mark_price,
                // the materializer's spelling: a one-way fill stores no position side
                position_side: (f.position_side != PositionSide::Both)
                    .then(|| f.position_side.to_string()),
            }),
            fee_correction: report.fee_correction.as_ref().map(|c| {
                ExecutionReportFeeCorrectionWire {
                    corrects_trade_id: c.corrects_trade_id.clone(),
                    commission: c.commission,
                    commission_asset: c.commission_asset.clone(),
                    price: c.price,
                }
            }),
        }
    }
}

/// An order's state after the fold: an `exec_order` row's fields.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecutionReportOrderWire {
    /// `+1` buy, `-1` sell.
    pub side: i32,
    /// The order's quantity.
    pub qty: f64,
    /// `OrderType::as_str`'s word.
    pub order_type: String,
    /// `OrderStatus::as_str`'s word.
    pub status: String,
    /// The limit price; `None` for a market order.
    pub price: Option<f64>,
    /// The stop or trigger price; `None` when the order has none.
    pub trigger_price: Option<f64>,
    /// How much of it has filled.
    pub filled_qty: f64,
    /// The average price of what filled.
    pub average_fill_price: f64,
    /// Why the order ended this way, in the words of whoever ended it: the risk gate's for `DENIED` (C.1c), the
    /// venue's for `REJECTED`, the core's for a cancel or a reject it made up. Absent from a report that carries
    /// no reason.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// One execution: an `exec_fill` row's fields.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecutionReportFillWire {
    /// The venue's execution id.
    pub trade_id: String,
    /// `+1` buy, `-1` sell.
    pub side: i32,
    /// The quantity this execution filled.
    pub qty: f64,
    /// Its price.
    pub price: f64,
    /// Its fee, in `commission_asset`.
    pub commission: f64,
    /// The fee's currency; empty when the venue did not say.
    pub commission_asset: String,
    /// `maker`, `taker`, or empty when the venue did not say.
    pub liquidity_side: String,
    /// The venue's mark price at the fill, when it said one.
    pub mark_price: Option<f64>,
    /// The hedge leg, `LONG` or `SHORT`; `None` for a one-way fill.
    pub position_side: Option<String>,
}

/// A fee correction (decision 0238): what `ExecFillRow::fee_correction_row` writes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecutionReportFeeCorrectionWire {
    /// The execution id of the fill it restates.
    pub corrects_trade_id: String,
    /// How much more the execution's fee is than its fill carried, in total.
    pub commission: f64,
    /// The currency of `commission`; empty when the venue did not say.
    pub commission_asset: String,
    /// The corrected fill's price: an inverse contract's fee enters the ledger at it.
    pub price: f64,
}

/// A forecast a live strategy published (`JournalRecord::Forecast`'s payload). Its decision time is the
/// record's `recorded_ms`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecutionForecastWire {
    /// The mount that published it.
    pub mount_id: String,
    /// The venue it names.
    pub venue: String,
    /// The symbol it names.
    pub symbol: String,
    /// The forecast, in the strategy's own unit.
    pub value: f64,
    /// How far ahead it looks, in milliseconds.
    pub horizon_ms: i64,
    /// How sure the strategy said it was, in `[0, 1]`; `None` when it did not say.
    pub confidence: Option<f64>,
}

/// Reports a boot could not regenerate (a restore that did not refold, a waker in the window, an order
/// outside the restore's base): the executions of the window may be incomplete.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecutionUnverifiedWire {
    /// The journal the window belongs to (a moved-aside predecessor's own, when it is its).
    pub journal_id: String,
    /// The last record whose reports are known whole.
    pub after_sequence: u64,
    /// The last record of the window.
    pub through_sequence: u64,
    /// When the window starts: the `now_ms` of the first record after `after_sequence`. `None` only when that
    /// record could not be read (a moved-aside journal's unreadable rest), as [`ExecutionStreamUp::Gap`]'s twin.
    pub from_ms: Option<i64>,
    /// When the window ends: the `now_ms` of the record at `through_sequence`. `None` only when that record could
    /// not be read.
    pub to_ms: Option<i64>,
    /// The orders whose reports may be missing, when the boot can name them.
    pub order_client_ids: Vec<String>,
    /// Why, in words.
    pub reason: String,
}

/// One journal's stream, as [`Request::ExecutionStreams`](crate::proto::Request::ExecutionStreams) answers it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionStreamStatusWire {
    /// The journal's id.
    pub journal_id: String,
    /// The tradehub's identity line, as its open stated it.
    pub tradehub: String,
    /// Every record up to it is stored; `None` when nothing is.
    pub stored_through: Option<u64>,
    /// When the datahub last stored a window.
    pub last_stored_ms: Option<i64>,
    /// Whether a tradehub is streaming this journal now.
    pub connected: bool,
    /// The windows the datahub knows it lacks.
    pub gaps: Vec<ExecutionStreamGapWire>,
}

/// A window of a journal the datahub knows it lacks: an `execution_stream_gap` row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionStreamGapWire {
    /// `pruned` (the journal pruned it past the hold) or `unverified` (a boot could not regenerate it).
    pub kind: String,
    /// The last record known whole before the window.
    pub after_sequence: u64,
    /// The first record after the window.
    pub resumes_at_sequence: u64,
    /// When the window starts, when known.
    pub lost_from_ms: Option<i64>,
    /// When the window ends, when known.
    pub lost_to_ms: Option<i64>,
    /// When the datahub recorded the gap.
    pub noted_ms: i64,
    /// Why, in the words of whoever found it (an unverified window's boot). Absent for a pruned window.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The orders whose executions the window may lack, when the boot could name them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub order_client_ids: Vec<String>,
}

/// Refuse a journal id that is not [`EXECUTION_STREAM_JOURNAL_ID_DIGITS`] lowercase hex digits. Both ends name
/// this one rule: the client before it sends the open, the datahub before the id reaches a commit key.
pub fn validate_journal_id(journal_id: &str) -> Result<(), String> {
    let hex = journal_id.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    if journal_id.len() == EXECUTION_STREAM_JOURNAL_ID_DIGITS && hex {
        Ok(())
    } else {
        Err(format!(
            "a journal id is {EXECUTION_STREAM_JOURNAL_ID_DIGITS} lowercase hex digits, and this one is not \
             ({} bytes)",
            journal_id.len()
        ))
    }
}
