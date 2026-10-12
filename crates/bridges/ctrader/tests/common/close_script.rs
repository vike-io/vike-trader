//! Close-by-position-id scripting: the stateful hedging-account serve loop and its reconcile fidelity.

use std::collections::HashSet;
use std::io::Write;
use std::net::TcpStream;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use prost::Message;

use vike_ctrader::framing::{self, FrameReader};
use vike_ctrader::proto::{
    ProtoOaClosePositionReq, ProtoOaDeal, ProtoOaErrorRes, ProtoOaExecutionEvent,
    ProtoOaExecutionType, ProtoOaNewOrderReq, ProtoOaOrder, ProtoOaOrderType, ProtoOaPosition,
    ProtoOaPositionStatus, ProtoOaReconcileRes, ProtoOaTradeData, ProtoOaTradeSide, pt,
};

use super::fake::AccountsScript;
use super::serve::{EofStream, FAKE_FILL_PRICE, FAKE_ORDER_ID, script_reply};

// ============================ close-by-position-id scripting =====================================
//
// `serve_close` models a HEDGING account statefully: every open creates a distinct position; a
// `ProtoOAClosePositionReq` reduces the named one. Used by `tests/exec_close.rs`.

/// Base `positionId` the close server assigns to the FIRST opened position; each later open
/// increments it — so the close test can predict the ids it must see on its close legs.
pub const CLOSE_POSITION_ID_BASE: i64 = 700_001;
/// Base `positionId` for the ERROR-status rows
/// [`FakeCtrader::start_close_scripted_with_unreadable_positions`] injects into every
/// `RECONCILE_RES`. Deliberately far from [`CLOSE_POSITION_ID_BASE`] so a stray id in a failure
/// message says which kind of row produced it.
pub const UNREADABLE_POSITION_ID_BASE: i64 = 900_001;
/// The `ctidTraderAccountId` the fake venue authenticates — every faithful `RECONCILE_RES` carries
/// it, and [`ReconcileFidelity::ForAnotherAccount`] carries [`OTHER_CTID`] instead.
pub const FAKE_CTID: i64 = 99;
/// A `ctidTraderAccountId` that is NOT this connection's — the wrong-account reconcile answer.
pub const OTHER_CTID: i64 = 12_345;

/// How a `serve_close` server's `RECONCILE_RES` relates to the book the venue ACTUALLY holds.
///
/// ⚠ **This axis exists because a server that always tells the truth cannot test a rule about
/// EVIDENCE.** `serve_close` used to answer every reconcile with its own real, complete,
/// fully-classifiable book, so every property that depends on an answer being WRONG — a hole in it,
/// a position missing from it, an answer about somebody else's account — was unrepresentable from a
/// test, and a position-VERIFIED halt is exactly a rule about how much an answer may be trusted. A
/// rig that cannot express the failure it is asked to rule out proves nothing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ReconcileFidelity {
    /// The answer reports every OPEN position the venue holds, all `PositionStatusOpen`, under this
    /// connection's own [`FAKE_CTID`]. The truthful server, and the default.
    #[default]
    Faithful,
    /// [`Self::Faithful`] plus `n` extra **ERROR-status** rows the bridge cannot classify — the
    /// honest shape of a HOLE (`ProtoOAPositionStatus.POSITION_STATUS_ERROR` is a real cTrader
    /// state this bridge genuinely cannot map to exposure), not a synthetic one.
    Unreadable(usize),
    /// ⚠ **The answer OMITS every position the venue actually holds.** Well-formed, fully
    /// classifiable, correct account — and EMPTY, from an account that is not flat. Nothing about
    /// this frame is decodably wrong, which is the entire point: a completeness check that counts
    /// undecodable ROWS cannot see a row that was never sent.
    OmitsEveryPosition,
    /// The answer belongs to [`OTHER_CTID`] — ANOTHER account's (flat) book arriving on this
    /// socket. Every row in it is well-formed and there are none, so a reader that never checks
    /// `ctidTraderAccountId` folds a STRANGER'S emptiness in and calls it authoritative for us.
    ForAnotherAccount,
}

impl ReconcileFidelity {
    /// How many unclassifiable ERROR rows ride along on each answer.
    fn unreadable(self) -> usize {
        match self {
            ReconcileFidelity::Unreadable(n) => n,
            _ => 0,
        }
    }

    /// Whether the venue's REAL open positions appear in the answer at all. `false` for both
    /// misrepresentations that matter here: the answer that silently drops them, and the answer
    /// that is about somebody else's (flat) account.
    fn reports_real_positions(self) -> bool {
        !matches!(
            self,
            ReconcileFidelity::OmitsEveryPosition | ReconcileFidelity::ForAnotherAccount
        )
    }

    /// The `ctidTraderAccountId` stamped on the answer.
    fn ctid(self) -> i64 {
        match self {
            ReconcileFidelity::ForAnotherAccount => OTHER_CTID,
            _ => FAKE_CTID,
        }
    }
}
/// The absolute fill price the close server reports on a CLOSING deal (distinct from the opening
/// [`FAKE_FILL_PRICE`] so a test could tell an open fill from a close fill if it needed to).
pub const CLOSE_FILL_PRICE: f64 = 1.14050;

/// One position the stateful [`serve_close`] server remembers between frames.
struct SrvPosition {
    symbol_id: i64,
    /// `ProtoOATradeSide` raw value (BUY/SELL) — the position's own direction.
    trade_side: i32,
    /// Remaining open volume in centi-units (reduced by each close).
    volume: i64,
    open_ts: i64,
}

/// How the close server answers each `CLOSE_POSITION_REQ` (by 1-based arrival index), so a test can
/// script the failure/concurrency edges the correlation fixes cover.
#[derive(Clone, Copy)]
pub enum CloseServePolicy {
    /// Every close FILLS (the happy path — `tests/exec_close.rs`'s open/close ladders).
    Fill,
    /// Every close is HELD (recorded, never answered) — leaves the coid in flight so a SECOND reduce
    /// on the same position hits the "already closing" exclusion deterministically.
    Hold,
    /// The FIRST close fills, the SECOND (and later) rejects via an `ORDER_REJECTED` execution event
    /// — the leg-reject-after-partial-progress case.
    FillThenReject,
    /// The FIRST close is refused with a protocol `ERROR_RES` (echoing the coid), later ones fill —
    /// the wire-level failure + tracker-forget case.
    ErrorResThenFill,
}

/// What to do for the `n`-th close (1-based).
enum CloseAction {
    Fill,
    Hold,
    RejectEvent,
    ErrorRes,
}

impl CloseServePolicy {
    fn action(self, n: usize) -> CloseAction {
        match self {
            CloseServePolicy::Fill => CloseAction::Fill,
            CloseServePolicy::Hold => CloseAction::Hold,
            CloseServePolicy::FillThenReject => {
                if n == 1 {
                    CloseAction::Fill
                } else {
                    CloseAction::RejectEvent
                }
            }
            CloseServePolicy::ErrorResThenFill => {
                if n == 1 {
                    CloseAction::ErrorRes
                } else {
                    CloseAction::Fill
                }
            }
        }
    }
}

/// An `ORDER_REJECTED` execution event for a closing order on `position_id` — cTrader rejecting a
/// `ProtoOAClosePositionReq` at the execution layer. Carries the position ref (via `order.positionId`)
/// so the bridge correlates it back to the reduce coid.
fn close_reject_event(position_id: i64, order_id: i64) -> (u32, Vec<u8>) {
    let order = ProtoOaOrder {
        order_id,
        trade_data: ProtoOaTradeData::default(),
        order_type: ProtoOaOrderType::Market as i32,
        order_status: 3, // REJECTED
        position_id: Some(position_id),
        closing_order: Some(true),
        ..Default::default()
    };
    let ev = ProtoOaExecutionEvent {
        ctid_trader_account_id: 99,
        execution_type: ProtoOaExecutionType::OrderRejected as i32,
        order: Some(order),
        error_code: Some("CLOSE_REJECTED".to_string()),
        ..Default::default()
    };
    (pt::EXECUTION_EVENT, ev.encode_to_vec())
}

/// A protocol `ERROR_RES` body for a refused `ProtoOAClosePositionReq` — the serve loop encodes it
/// with the request's `clientMsgId` (the coid) so `on_error_res` correlates it. The error code
/// deliberately contains neither "AUTH" nor "TOKEN", so the actor does NOT divert it to token
/// refresh (`is_auth_error`).
fn close_error_res_body() -> (u32, Vec<u8>) {
    let err = ProtoOaErrorRes {
        error_code: "POSITION_NOT_FOUND".to_string(),
        description: Some("close rejected by venue".to_string()),
        ..Default::default()
    };
    (pt::ERROR_RES, err.encode_to_vec())
}

/// Stateful exec serve loop for the close tests: the handshake is delegated to [`script_reply`],
/// then NEW_ORDER_REQ frames OPEN positions and CLOSE_POSITION_REQ frames are answered per `policy`.
pub(super) fn serve_close(
    stream: TcpStream,
    seen: Arc<Mutex<Vec<u32>>>,
    closes: Arc<Mutex<Vec<(i64, i64)>>>,
    open_ids: Arc<Mutex<HashSet<i64>>>,
    script: AccountsScript,
    policy: CloseServePolicy,
    preseed: Vec<(i64, i32, i64)>,
    fidelity: ReconcileFidelity,
) {
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
    let mut writer = stream.try_clone().expect("clone fake stream");
    let (eof_stream, eof) = EofStream::new(stream);
    let mut reader = FrameReader::new(eof_stream);

    let mut positions: std::collections::HashMap<i64, SrvPosition> =
        std::collections::HashMap::new();
    // Positions that already exist before the client connects (the pre-existing-book flatten case).
    for (position_id, trade_side, volume) in &preseed {
        positions.insert(
            *position_id,
            SrvPosition { symbol_id: 1, trade_side: *trade_side, volume: *volume, open_ts: 0 },
        );
    }
    let mut next_position_id = CLOSE_POSITION_ID_BASE + preseed.len() as i64;
    let mut next_order_id = FAKE_ORDER_ID;
    let mut next_deal_id = 900_001_i64;
    let mut next_open_ts = 1_784_000_000_000_i64;
    let mut close_count = 0usize;

    loop {
        match reader.next_frame() {
            Ok(Some(msg)) => {
                let payload_type = msg.payload_type;
                seen.lock().expect("seen lock").push(payload_type);
                // Close responses echo the request's `clientMsgId` (the coid) so an ERROR_RES
                // correlates back to the reduce; everything else uses the generic "srv" id.
                let mut reply_msg_id = "srv".to_string();
                let frames: Vec<(u32, Vec<u8>)> = match payload_type {
                    pt::NEW_ORDER_REQ => {
                        match ProtoOaNewOrderReq::decode(msg.payload.as_deref().unwrap_or(&[])) {
                            Ok(order) => {
                                let position_id = next_position_id;
                                next_position_id += 1;
                                let order_id = next_order_id;
                                next_order_id += 1;
                                let deal_id = next_deal_id;
                                next_deal_id += 1;
                                let open_ts = next_open_ts;
                                next_open_ts += 1000;
                                positions.insert(
                                    position_id,
                                    SrvPosition {
                                        symbol_id: order.symbol_id,
                                        trade_side: order.trade_side,
                                        volume: order.volume,
                                        open_ts,
                                    },
                                );
                                open_position_events(
                                    &order,
                                    position_id,
                                    order_id,
                                    deal_id,
                                    open_ts,
                                )
                            }
                            Err(_) => Vec::new(),
                        }
                    }
                    pt::CLOSE_POSITION_REQ => {
                        match ProtoOaClosePositionReq::decode(msg.payload.as_deref().unwrap_or(&[]))
                        {
                            Ok(req) => {
                                closes
                                    .lock()
                                    .expect("closes lock")
                                    .push((req.position_id, req.volume));
                                close_count += 1;
                                reply_msg_id = msg.client_msg_id.clone().unwrap_or_default();
                                let order_id = next_order_id;
                                next_order_id += 1;
                                let deal_id = next_deal_id;
                                next_deal_id += 1;
                                match policy.action(close_count) {
                                    CloseAction::Fill => {
                                        match positions.get_mut(&req.position_id) {
                                            Some(pos) => {
                                                let frames = close_position_events(
                                                    &req, pos, order_id, deal_id,
                                                );
                                                // Once fully flattened, drop it from the OPEN set so a
                                                // preseeded-book test can watch the venue reach zero.
                                                if pos.volume == 0 {
                                                    open_ids
                                                        .lock()
                                                        .expect("open_ids lock")
                                                        .remove(&req.position_id);
                                                }
                                                frames
                                            }
                                            None => Vec::new(),
                                        }
                                    }
                                    CloseAction::Hold => Vec::new(),
                                    CloseAction::RejectEvent => {
                                        vec![close_reject_event(req.position_id, order_id)]
                                    }
                                    CloseAction::ErrorRes => vec![close_error_res_body()],
                                }
                            }
                            Err(_) => Vec::new(),
                        }
                    }
                    // The STATEFUL reconcile: unlike `script_reply`'s fixed answer (one pending
                    // order, zero positions), this server knows its own book, so it reports the
                    // positions that are actually open — including any PRE-SEEDED ones that
                    // existed before the client connected. That is what makes the connect-time
                    // position seed observable end to end: without it, a fresh mount is blind to
                    // exactly the book `close_all` had to be invented for.
                    pt::RECONCILE_REQ => {
                        // ⚠ The venue's REAL book is `positions`; what it SAYS is `fidelity`'s
                        // business. `OmitsEveryPosition` sends a well-formed, fully-classifiable,
                        // EMPTY list from an account that is not flat — the one misrepresentation
                        // no row-counting completeness check can ever see.
                        let mut open: Vec<ProtoOaPosition> = if fidelity.reports_real_positions() {
                            positions
                                .iter()
                                .filter(|(_, p)| p.volume > 0)
                                .map(|(id, p)| ProtoOaPosition {
                                    position_id: *id,
                                    trade_data: ProtoOaTradeData {
                                        symbol_id: p.symbol_id,
                                        volume: p.volume,
                                        trade_side: p.trade_side,
                                        open_timestamp: Some(p.open_ts),
                                        ..Default::default()
                                    },
                                    position_status: ProtoOaPositionStatus::PositionStatusOpen
                                        as i32,
                                    swap: 0,
                                    ..Default::default()
                                })
                                .collect()
                        } else {
                            Vec::new()
                        };
                        // …and the rows the bridge CANNOT classify, when a test asked for them. A
                        // server that only ever emits OPEN makes "the answer had a hole in it"
                        // unrepresentable, and that is the one state a position-verified halt must
                        // not read as a flat account.
                        for i in 0..fidelity.unreadable() {
                            open.push(ProtoOaPosition {
                                position_id: UNREADABLE_POSITION_ID_BASE + i as i64,
                                trade_data: ProtoOaTradeData {
                                    symbol_id: 1,
                                    volume: 100_000,
                                    trade_side: ProtoOaTradeSide::Buy as i32,
                                    open_timestamp: Some(1),
                                    ..Default::default()
                                },
                                position_status: ProtoOaPositionStatus::PositionStatusError as i32,
                                swap: 0,
                                ..Default::default()
                            });
                        }
                        let res = ProtoOaReconcileRes {
                            ctid_trader_account_id: fidelity.ctid(),
                            order: vec![],
                            position: open,
                            ..Default::default()
                        };
                        vec![(pt::RECONCILE_RES, res.encode_to_vec())]
                    }
                    // Handshake / everything else → the shared happy-path scripting.
                    _ => script_reply(&msg, script, false),
                };
                for (reply_type, body) in frames {
                    let frame = framing::encode(reply_type, &body, &reply_msg_id);
                    if writer.write_all(&frame).is_err() {
                        return;
                    }
                    let _ = writer.flush();
                }
            }
            Ok(None) => {
                if eof.load(Ordering::SeqCst) {
                    break;
                }
            }
            Err(_) => break,
        }
    }
}

/// The `ORDER_ACCEPTED` then `ORDER_FILLED` a fresh OPEN produces, the FILLED carrying both the
/// `deal` and the new OPEN `position` ref (so the bridge tracks its `positionId` for later reduce
/// routing). The position materializes at fill time, so only the FILLED event carries the ref.
fn open_position_events(
    order: &ProtoOaNewOrderReq,
    position_id: i64,
    order_id: i64,
    deal_id: i64,
    open_ts: i64,
) -> Vec<(u32, Vec<u8>)> {
    let coid = order.client_order_id.clone().or_else(|| order.label.clone());
    let trade_data = ProtoOaTradeData {
        symbol_id: order.symbol_id,
        volume: order.volume,
        trade_side: order.trade_side,
        label: order.label.clone(),
        open_timestamp: Some(open_ts),
        ..Default::default()
    };
    let accepted_order = ProtoOaOrder {
        order_id,
        trade_data: trade_data.clone(),
        order_type: order.order_type,
        order_status: 1, // ACCEPTED
        client_order_id: coid.clone(),
        position_id: Some(position_id),
        ..Default::default()
    };
    let accepted = ProtoOaExecutionEvent {
        ctid_trader_account_id: 99,
        execution_type: ProtoOaExecutionType::OrderAccepted as i32,
        order: Some(accepted_order.clone()),
        ..Default::default()
    };
    let mut filled_order = accepted_order;
    filled_order.order_status = 2; // FILLED
    let position = ProtoOaPosition {
        position_id,
        trade_data,
        position_status: ProtoOaPositionStatus::PositionStatusOpen as i32,
        swap: 0,
        ..Default::default()
    };
    let filled = ProtoOaExecutionEvent {
        ctid_trader_account_id: 99,
        execution_type: ProtoOaExecutionType::OrderFilled as i32,
        order: Some(filled_order),
        deal: Some(ProtoOaDeal {
            deal_id,
            order_id,
            position_id,
            volume: order.volume,
            filled_volume: order.volume,
            symbol_id: order.symbol_id,
            execution_timestamp: 1_784_017_625_777,
            execution_price: Some(FAKE_FILL_PRICE),
            trade_side: order.trade_side,
            ..Default::default()
        }),
        position: Some(position),
        ..Default::default()
    };
    vec![
        (pt::EXECUTION_EVENT, accepted.encode_to_vec()),
        (pt::EXECUTION_EVENT, filled.encode_to_vec()),
    ]
}

/// The single `ORDER_FILLED` a close produces: a closing deal that reduces `pos` by the requested
/// volume (`tradeSide` OPPOSITE the position's own, so it nets toward flat), plus the updated
/// `position` ref (OPEN with the reduced volume, or CLOSED when fully flattened). Mutates `pos`'s
/// remaining volume in place so a second close of the same position sees the reduced size.
fn close_position_events(
    req: &ProtoOaClosePositionReq,
    pos: &mut SrvPosition,
    order_id: i64,
    deal_id: i64,
) -> Vec<(u32, Vec<u8>)> {
    let close_volume = req.volume.min(pos.volume);
    pos.volume = (pos.volume - close_volume).max(0);
    let closed = pos.volume == 0;
    let close_side = if pos.trade_side == ProtoOaTradeSide::Buy as i32 {
        ProtoOaTradeSide::Sell as i32
    } else {
        ProtoOaTradeSide::Buy as i32
    };
    let position = ProtoOaPosition {
        position_id: req.position_id,
        trade_data: ProtoOaTradeData {
            symbol_id: pos.symbol_id,
            volume: pos.volume,
            trade_side: pos.trade_side,
            open_timestamp: Some(pos.open_ts),
            ..Default::default()
        },
        position_status: if closed {
            ProtoOaPositionStatus::PositionStatusClosed as i32
        } else {
            ProtoOaPositionStatus::PositionStatusOpen as i32
        },
        swap: 0,
        ..Default::default()
    };
    let closing_order = ProtoOaOrder {
        order_id,
        trade_data: ProtoOaTradeData {
            symbol_id: pos.symbol_id,
            volume: close_volume,
            trade_side: close_side,
            ..Default::default()
        },
        order_type: ProtoOaOrderType::Market as i32,
        order_status: 2, // FILLED
        position_id: Some(req.position_id),
        closing_order: Some(true),
        ..Default::default()
    };
    let filled = ProtoOaExecutionEvent {
        ctid_trader_account_id: 99,
        execution_type: ProtoOaExecutionType::OrderFilled as i32,
        order: Some(closing_order),
        deal: Some(ProtoOaDeal {
            deal_id,
            order_id,
            position_id: req.position_id,
            volume: close_volume,
            filled_volume: close_volume,
            symbol_id: pos.symbol_id,
            execution_timestamp: 1_784_017_626_500,
            execution_price: Some(CLOSE_FILL_PRICE),
            trade_side: close_side,
            ..Default::default()
        }),
        position: Some(position),
        ..Default::default()
    };
    vec![(pt::EXECUTION_EVENT, filled.encode_to_vec())]
}
