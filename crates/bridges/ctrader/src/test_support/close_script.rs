//! Close-by-position-id scripting: the stateful hedging-account serve loop and its reconcile fidelity.

use std::collections::HashSet;
use std::io::Write;
use std::net::TcpStream;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use prost::Message;

use crate::transport::framing::{self, FrameReader};
use crate::transport::proto::{
    ProtoOaClosePositionDetail, ProtoOaClosePositionReq, ProtoOaDeal, ProtoOaDealListRes,
    ProtoOaDealStatus, ProtoOaErrorRes, ProtoOaExecutionEvent, ProtoOaExecutionType,
    ProtoOaNewOrderReq, ProtoOaOrder, ProtoOaOrderListByPositionIdReq,
    ProtoOaOrderListByPositionIdRes, ProtoOaOrderListRes, ProtoOaOrderType, ProtoOaPosition,
    ProtoOaPositionStatus, ProtoOaReconcileRes, ProtoOaTradeData, ProtoOaTradeSide, pt,
};

use super::fake::AccountsScript;
use super::serve::{
    FAKE_FILL_PRICE, FAKE_ORDER_ID, RECONCILE_PENDING_ORDER_CLIENT_ID, RECONCILE_PENDING_ORDER_ID,
    execution_events, script_reply,
};
use crate::transport::connection::stream::EofStream;

// ============================ close-by-position-id scripting =====================================
//
// `serve_close` models a HEDGING account statefully: every open creates a distinct position; a
// `ProtoOAClosePositionReq` reduces the named one. Used by `tests/offline/execution_close.rs`.

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
    /// The answer belongs to [`OTHER_CTID`] — ANOTHER account's book arriving on this socket: no
    /// positions, and ONE pending order (the stranger's [`RECONCILE_PENDING_ORDER_CLIENT_ID`], on the venue
    /// order id [`RECONCILE_PENDING_ORDER_ID`]). Every row in it is well-formed, so a reader that
    /// never checks `ctidTraderAccountId` folds a STRANGER'S emptiness in and calls it
    /// authoritative for us, and learns an order id for an order client id that is not ours.
    ForAnotherAccount,
    /// The FIRST reconcile (the connect-time seed) is refused with an `ERROR_RES`; every later one is
    /// answered faithfully. The venue holds its book and the client's seed failed: the start the
    /// 0.1.97 fix is for, where the book is EMPTY and NOT authoritative while the account is not flat.
    RefusesTheSeed,
    /// EVERY reconcile is refused with an `ERROR_RES` (fast, not a timeout): the seed fails and so does any
    /// later read, so the client cannot learn what the venue holds.
    RefusesEveryReconcile,
    /// The venue holds NO position and reports ONE resting STOP (a STOP_LIMIT for
    /// [`ExitLookup::OutlivedStopLimit`]; [`RESTING_EXIT_ORDER_CLIENT_ID`], venue order
    /// [`RESTING_EXIT_ORDER_ID`]) that names a position it does not list as open ([`GONE_POSITION_ID`]): either a
    /// bound exit that outlived its position or a plain pending order, as the [`ExitLookup`] says when the
    /// client asks for that position's orders (`ProtoOAOrderListByPositionIdReq`).
    RestingExit(ExitLookup),
}

/// The `positionId` the resting exit of [`ReconcileFidelity::RestingExit`] names and the venue does not list as open.
pub const GONE_POSITION_ID: i64 = 770_001;
/// Its order client id.
pub const RESTING_EXIT_ORDER_CLIENT_ID: &str = "orphan-stop-1";
/// Its venue order id.
pub const RESTING_EXIT_ORDER_ID: i64 = 880_001;

/// What the fake venue answers to `ProtoOAOrderListByPositionIdReq` for [`GONE_POSITION_ID`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExitLookup {
    /// A filled entry order, a filled CLOSING order and the pending exit: the position existed and closed, so the
    /// exit outlived it.
    Outlived,
    /// The pending order alone: the position never had a fill, so it is a plain pending order's own empty one.
    PlainPending,
    /// An `ERROR_RES` to every form of the request.
    Refused,
    /// An `ERROR_RES` to the request without bounds and the `Outlived` list to the one with a window.
    RefusesTheUnboundedForm,
    /// The pending exit alone but `hasMore`: no fill among the orders a truncated chunk carries is not a proof
    /// that there was none.
    Truncated,
    /// [`Self::Outlived`], with the resting exit a SELL STOP_LIMIT instead of the STOP (the same stop price, its
    /// limit `RESTING_EXIT_SLIPPAGE_POINTS` past it): a stop-limit binds as its position's stop, so it outlives
    /// its position the same way.
    OutlivedStopLimit,
}

/// The `slippageInPoints` of the resting STOP_LIMIT of [`ExitLookup::OutlivedStopLimit`].
const RESTING_EXIT_SLIPPAGE_POINTS: i64 = 10;

impl ReconcileFidelity {
    /// Whether the `n`-th `RECONCILE_REQ` (1-based) is answered with an `ERROR_RES` instead of a book.
    fn refuses(self, n: usize) -> bool {
        match self {
            ReconcileFidelity::RefusesTheSeed => n == 1,
            ReconcileFidelity::RefusesEveryReconcile => true,
            _ => false,
        }
    }

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

    /// The PENDING orders the answer lists: none, except the one the stranger of
    /// [`ReconcileFidelity::ForAnotherAccount`] holds.
    fn pending_orders(self) -> Vec<ProtoOaOrder> {
        match self {
            ReconcileFidelity::ForAnotherAccount => vec![ProtoOaOrder {
                order_id: RECONCILE_PENDING_ORDER_ID,
                trade_data: ProtoOaTradeData {
                    symbol_id: 1,
                    volume: 100,
                    trade_side: ProtoOaTradeSide::Buy as i32,
                    label: Some(RECONCILE_PENDING_ORDER_CLIENT_ID.to_string()),
                    ..Default::default()
                },
                order_type: ProtoOaOrderType::Limit as i32,
                order_status: 1,
                client_order_id: Some(RECONCILE_PENDING_ORDER_CLIENT_ID.to_string()),
                limit_price: Some(1.10),
                ..Default::default()
            }],
            ReconcileFidelity::RestingExit(lookup) => vec![resting_exit(lookup)],
            _ => Vec::new(),
        }
    }
}

/// The resting exit of [`ReconcileFidelity::RestingExit`]: a SELL STOP of 1000 EURUSD units that names
/// [`GONE_POSITION_ID`] and carries our order client id in both fields; a SELL STOP_LIMIT at the same stop price for
/// [`ExitLookup::OutlivedStopLimit`].
fn resting_exit(lookup: ExitLookup) -> ProtoOaOrder {
    let stop = ProtoOaOrder {
        order_id: RESTING_EXIT_ORDER_ID,
        trade_data: ProtoOaTradeData {
            symbol_id: 1,
            volume: 100_000,
            trade_side: ProtoOaTradeSide::Sell as i32,
            label: Some(RESTING_EXIT_ORDER_CLIENT_ID.to_string()),
            ..Default::default()
        },
        order_type: ProtoOaOrderType::Stop as i32,
        order_status: 1, // ACCEPTED
        client_order_id: Some(RESTING_EXIT_ORDER_CLIENT_ID.to_string()),
        stop_price: Some(1.05),
        position_id: Some(GONE_POSITION_ID),
        ..Default::default()
    };
    match lookup {
        ExitLookup::OutlivedStopLimit => ProtoOaOrder {
            order_type: ProtoOaOrderType::StopLimit as i32,
            slippage_in_points: Some(RESTING_EXIT_SLIPPAGE_POINTS),
            ..stop
        },
        _ => stop,
    }
}

/// A FILLED order on [`GONE_POSITION_ID`]: the entry (`closing == false`) or the closing order.
fn filled_order_of_gone_position(order_id: i64, closing: bool) -> ProtoOaOrder {
    ProtoOaOrder {
        order_id,
        trade_data: ProtoOaTradeData {
            symbol_id: 1,
            volume: 100_000,
            trade_side: if closing {
                ProtoOaTradeSide::Sell as i32
            } else {
                ProtoOaTradeSide::Buy as i32
            },
            ..Default::default()
        },
        order_type: ProtoOaOrderType::Market as i32,
        order_status: 2, // FILLED
        position_id: Some(GONE_POSITION_ID),
        closing_order: Some(closing),
        ..Default::default()
    }
}

/// The `ProtoOAOrderListByPositionIdReq` answer `lookup` scripts, or `None` for an `ERROR_RES`. `bounded` is whether
/// the request carried a window.
fn orders_of_gone_position(
    lookup: ExitLookup,
    bounded: bool,
) -> Option<ProtoOaOrderListByPositionIdRes> {
    let outlived = vec![
        resting_exit(lookup),
        filled_order_of_gone_position(880_003, true),
        filled_order_of_gone_position(880_002, false),
    ];
    let (order, has_more) = match lookup {
        ExitLookup::Outlived | ExitLookup::OutlivedStopLimit => (outlived, false),
        ExitLookup::PlainPending => (vec![resting_exit(lookup)], false),
        ExitLookup::Refused => return None,
        ExitLookup::RefusesTheUnboundedForm if !bounded => return None,
        ExitLookup::RefusesTheUnboundedForm => (outlived, false),
        // Truncated: the chunk carries the pending exit alone and says there is more.
        ExitLookup::Truncated => (vec![resting_exit(lookup)], true),
    };
    Some(ProtoOaOrderListByPositionIdRes {
        ctid_trader_account_id: FAKE_CTID,
        order,
        has_more,
        ..Default::default()
    })
}

/// What the venue did with an order or close whose answer died with the socket, as its lists say on the next connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnheardOrder {
    /// It FILLED: listed under its order client id, its deal listed, the book moved.
    Filled,
    /// It FILLED, and the deal list is refused.
    FilledDealsRefused,
    /// It was CANCELLED: listed under its order client id, the book unmoved.
    Cancelled,
    /// Not listed: a refused order is absent exactly like one the venue never received.
    NotListed,
}

/// The venue order id and deal id of the order [`serve_unheard`] lists.
pub const UNHEARD_ORDER_ID: i64 = 880_100;
pub const UNHEARD_DEAL_ID: i64 = 990_100;
/// The venue order id and deal id of the order that OPENED the first seeded position, which the deal list of a venue
/// given a position label ([`FakeCtrader::start_unheard_close`](super::FakeCtrader::start_unheard_close)) holds too.
pub const UNHEARD_OPENER_ORDER_ID: i64 = 880_099;
pub const UNHEARD_OPENER_DEAL_ID: i64 = 990_099;

/// What [`serve_unheard`] remembers across connections: its book, and the one order the dead socket carried.
pub(super) struct UnheardVenue {
    pub(super) book: Vec<(i64, i32, i64)>,
    written: Option<UnheardWrite>,
    /// The label the first seeded position was opened under, with that position, or `None`: no opening deal is listed
    /// then, and no deal carries a label.
    opener: Option<(String, (i64, i32, i64))>,
}

impl UnheardVenue {
    pub(super) fn new(book: Vec<(i64, i32, i64)>) -> Self {
        Self { book, written: None, opener: None }
    }

    /// [`Self::new`] whose first position was opened by an order labelled `label`.
    pub(super) fn opened_under(book: Vec<(i64, i32, i64)>, label: &str) -> Self {
        let opener = book.first().map(|row| (label.to_string(), *row));
        Self { opener, ..Self::new(book) }
    }

    /// Whether the dead socket's order has been taken: a connection after it is a reconnect.
    pub(super) fn lost_an_order(&self) -> bool {
        self.written.is_some()
    }

    /// The deals the venue lists, each with the label of its order: the opening deal of the first seeded position (with
    /// a label only), and the dead socket's order's. ⚠ The order a close by position id makes carries the POSITION's
    /// label, so its deal carries the opener's (measured on the CI box, 2026-11-09, deal 958656727).
    fn deals(&self) -> Vec<(ProtoOaDeal, Option<&str>)> {
        let label = self.opener.as_ref().map(|(label, _)| label.as_str());
        let opener = self.opener.iter().map(|(label, (position_id, trade_side, volume))| {
            let deal = ProtoOaDeal {
                deal_id: UNHEARD_OPENER_DEAL_ID,
                order_id: UNHEARD_OPENER_ORDER_ID,
                position_id: *position_id,
                volume: *volume,
                filled_volume: *volume,
                symbol_id: 1,
                execution_timestamp: 1_784_017_600_000,
                execution_price: Some(FAKE_FILL_PRICE),
                trade_side: *trade_side,
                deal_status: ProtoOaDealStatus::Filled as i32,
                ..Default::default()
            };
            (deal, Some(label.as_str()))
        });
        let written = self.written.iter().map(|w| (w.deal(), label.filter(|_| w.closing)));
        opener.chain(written).collect()
    }
}

struct UnheardWrite {
    order_client_id: String,
    symbol_id: i64,
    trade_side: i32,
    volume: i64,
    closing: bool,
    position_id: i64,
}

impl UnheardWrite {
    /// The order as the venue lists it under the id it was sent with; a closing one carries `label`, its position's.
    fn order(&self, status: i32, label: Option<&str>) -> ProtoOaOrder {
        ProtoOaOrder {
            order_id: UNHEARD_ORDER_ID,
            trade_data: ProtoOaTradeData {
                symbol_id: self.symbol_id,
                volume: self.volume,
                trade_side: self.trade_side,
                label: label.filter(|_| self.closing).map(str::to_string),
                ..Default::default()
            },
            order_type: ProtoOaOrderType::Market as i32,
            order_status: status,
            client_order_id: Some(self.order_client_id.clone()),
            position_id: Some(self.position_id),
            closing_order: Some(self.closing),
            ..Default::default()
        }
    }

    fn deal(&self) -> ProtoOaDeal {
        ProtoOaDeal {
            deal_id: UNHEARD_DEAL_ID,
            order_id: UNHEARD_ORDER_ID,
            position_id: self.position_id,
            volume: self.volume,
            filled_volume: self.volume,
            symbol_id: self.symbol_id,
            execution_timestamp: 1_784_017_626_500,
            execution_price: Some(CLOSE_FILL_PRICE),
            trade_side: self.trade_side,
            deal_status: ProtoOaDealStatus::Filled as i32,
            // A closing deal states what it closed; every figure zero, so it books no swap.
            close_position_detail: self.closing.then(ProtoOaClosePositionDetail::default),
            ..Default::default()
        }
    }
}

/// One connection of a venue that takes the first `NEW_ORDER_REQ` or `CLOSE_POSITION_REQ`, says nothing and loses the
/// socket, and on every later connection states what became of it through its lists, as `outcome` says.
pub(super) fn serve_unheard(
    stream: TcpStream,
    seen: Arc<Mutex<Vec<u32>>>,
    venue: Arc<Mutex<UnheardVenue>>,
    outcome: UnheardOrder,
) {
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
    let mut writer = stream.try_clone().expect("clone fake stream");
    let (eof_stream, eof) = EofStream::new(stream);
    let mut reader = FrameReader::new(eof_stream);
    let filled = matches!(outcome, UnheardOrder::Filled | UnheardOrder::FilledDealsRefused);
    loop {
        let msg = match reader.next_frame() {
            Ok(Some(msg)) => msg,
            Ok(None) if eof.load(Ordering::SeqCst) => return,
            Ok(None) => continue,
            Err(_) => return,
        };
        seen.lock().expect("seen lock").push(msg.payload_type);
        let payload = msg.payload.as_deref().unwrap_or(&[]);
        let mut venue = venue.lock().expect("venue lock");
        let frames: Vec<(u32, Vec<u8>)> = match msg.payload_type {
            pt::NEW_ORDER_REQ => {
                let order = ProtoOaNewOrderReq::decode(payload).expect("a new order");
                venue.written = Some(UnheardWrite {
                    order_client_id: msg.client_msg_id.clone().unwrap_or_default(),
                    symbol_id: order.symbol_id,
                    trade_side: order.trade_side,
                    volume: order.volume,
                    closing: false,
                    position_id: CLOSE_POSITION_ID_BASE + 1,
                });
                if filled {
                    venue.book.push((CLOSE_POSITION_ID_BASE + 1, order.trade_side, order.volume));
                }
                return;
            }
            pt::CLOSE_POSITION_REQ => {
                let req = ProtoOaClosePositionReq::decode(payload).expect("a close");
                venue.written = Some(UnheardWrite {
                    order_client_id: msg.client_msg_id.clone().unwrap_or_default(),
                    symbol_id: 1,
                    trade_side: ProtoOaTradeSide::Sell as i32,
                    volume: req.volume,
                    closing: true,
                    position_id: req.position_id,
                });
                if filled {
                    venue.book.retain(|(id, _, _)| *id != req.position_id);
                }
                return;
            }
            pt::RECONCILE_REQ => {
                let position = venue
                    .book
                    .iter()
                    .map(|(id, trade_side, volume)| ProtoOaPosition {
                        position_id: *id,
                        trade_data: ProtoOaTradeData {
                            symbol_id: 1,
                            volume: *volume,
                            trade_side: *trade_side,
                            open_timestamp: Some(1),
                            ..Default::default()
                        },
                        position_status: ProtoOaPositionStatus::PositionStatusOpen as i32,
                        swap: 0,
                        ..Default::default()
                    })
                    .collect();
                let res = ProtoOaReconcileRes {
                    ctid_trader_account_id: FAKE_CTID,
                    position,
                    ..Default::default()
                };
                vec![(pt::RECONCILE_RES, res.encode_to_vec())]
            }
            pt::ORDER_LIST_REQ => {
                let label = venue.opener.as_ref().map(|(label, _)| label.as_str());
                let order = match (&venue.written, outcome) {
                    (Some(w), UnheardOrder::Filled | UnheardOrder::FilledDealsRefused) => {
                        vec![w.order(2, label)] // FILLED
                    }
                    (Some(w), UnheardOrder::Cancelled) => vec![w.order(5, label)], // CANCELLED
                    _ => Vec::new(),
                };
                let res = ProtoOaOrderListRes {
                    ctid_trader_account_id: FAKE_CTID,
                    order,
                    ..Default::default()
                };
                vec![(pt::ORDER_LIST_RES, res.encode_to_vec())]
            }
            pt::DEAL_LIST_REQ if outcome == UnheardOrder::FilledDealsRefused => {
                vec![close_error_res_body()]
            }
            pt::DEAL_LIST_REQ => vec![(pt::DEAL_LIST_RES, deal_list_frame(&venue.deals()))],
            _ => script_reply(&msg, AccountsScript::OneDemo, false),
        };
        drop(venue);
        for (reply_type, body) in frames {
            let message_id = if reply_type == pt::ERROR_RES { "" } else { "srv" };
            if writer.write_all(&framing::encode(reply_type, &body, message_id)).is_err() {
                return;
            }
            let _ = writer.flush();
        }
    }
}

/// **A `ProtoOADealListRes` frame as the venue's Release 100 sends it**: each deal encoded by the generated type with its
/// `label` (tag 18, which the vendored proto predates) appended inside the deal's own bytes, where the wire has it.
pub fn deal_list_frame(deals: &[(ProtoOaDeal, Option<&str>)]) -> Vec<u8> {
    fn varint(mut v: u64, out: &mut Vec<u8>) {
        while v >= 0x80 {
            out.push((v as u8) | 0x80);
            v >>= 7;
        }
        out.push(v as u8);
    }
    // One length-delimited field: key (`tag << 3 | 2`), length, bytes.
    fn field(tag: u64, bytes: &[u8], out: &mut Vec<u8>) {
        varint((tag << 3) | 2, out);
        varint(bytes.len() as u64, out);
        out.extend_from_slice(bytes);
    }
    let mut frame = ProtoOaDealListRes {
        ctid_trader_account_id: FAKE_CTID,
        has_more: false,
        ..Default::default()
    }
    .encode_to_vec();
    for (deal, label) in deals {
        let mut bytes = deal.encode_to_vec();
        if let Some(label) = label {
            field(18, label.as_bytes(), &mut bytes);
        }
        field(3, &bytes, &mut frame);
    }
    frame
}

/// The absolute fill price the close server reports on a CLOSING deal (distinct from the opening
/// [`FAKE_FILL_PRICE`] so a test could tell an open fill from a close fill if it needed to).
pub const CLOSE_FILL_PRICE: f64 = 1.14050;

/// One position the stateful [`serve_close`] server remembers between frames.
struct ServerPosition {
    symbol_id: i64,
    /// `ProtoOATradeSide` raw value (BUY/SELL) — the position's own direction.
    trade_side: i32,
    /// Remaining open volume in centi-units (reduced by each close).
    volume: i64,
    open_timestamp: i64,
}

/// How the close server answers each `CLOSE_POSITION_REQ` (by 1-based arrival index), so a test can
/// script the failure/concurrency edges the correlation fixes cover.
#[derive(Clone, Copy)]
pub enum CloseServePolicy {
    /// Every close FILLS (the happy path — `tests/offline/execution_close.rs`'s open/close ladders).
    Fill,
    /// Every close is HELD (recorded, never answered) — leaves the order client id in flight so a SECOND reduce
    /// on the same position hits the "already closing" exclusion deterministically.
    Hold,
    /// The FIRST close fills, the SECOND (and later) rejects via an `ORDER_REJECTED` execution event
    /// — the leg-reject-after-partial-progress case.
    FillThenReject,
    /// The FIRST close is refused with a protocol `ERROR_RES` (echoing the order client id), later ones fill —
    /// the wire-level failure + tracker-forget case.
    ErrorResThenFill,
    /// The socket dies at the first close, unanswered: the case of a close written just before an outage.
    DropTheSocket,
    /// From the first close on the connection answers NOTHING, the probe neither, and the socket stays up while the
    /// venue's own heartbeat still reaches the client ([`push_while_deaf`]): the way out blackholed and the way in open,
    /// the cut that kept a close unanswered on the CI box (2026-11-09).
    GoDeaf,
    /// The FIRST close is held for the duration and then fills; later ones fill at once: a close still in flight
    /// when the next order arrives, which then completes.
    HoldFirstFor(Duration),
}

/// What to do for the `n`-th close (1-based).
enum CloseAction {
    Fill,
    Hold,
    HoldFor(Duration),
    RejectEvent,
    ErrorRes,
    DropTheSocket,
    GoDeaf,
}

/// How often a deaf connection ([`CloseServePolicy::GoDeaf`]) pushes its heartbeat: well inside any idle bound a test sets.
const DEAF_PUSH_EVERY: Duration = Duration::from_millis(50);

/// **The venue reads what the client writes and answers none of it, while its own heartbeat keeps arriving**, until the
/// client closes the socket.
fn push_while_deaf(
    writer: &mut TcpStream,
    reader: &mut FrameReader<EofStream<TcpStream>>,
    eof: &std::sync::atomic::AtomicBool,
) {
    writer.set_read_timeout(Some(DEAF_PUSH_EVERY)).ok();
    loop {
        if writer.write_all(&framing::encode(pt::HEARTBEAT_EVENT, &[], "")).is_err() {
            return;
        }
        let _ = writer.flush();
        match reader.next_frame() {
            Ok(None) if eof.load(Ordering::SeqCst) => return,
            Ok(_) => {}
            Err(_) => return,
        }
    }
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
            CloseServePolicy::DropTheSocket => CloseAction::DropTheSocket,
            CloseServePolicy::GoDeaf => CloseAction::GoDeaf,
            CloseServePolicy::HoldFirstFor(d) if n == 1 => CloseAction::HoldFor(d),
            CloseServePolicy::HoldFirstFor(_) => CloseAction::Fill,
        }
    }
}

/// An `ORDER_REJECTED` execution event for a closing order on `position_id` — cTrader rejecting a
/// `ProtoOAClosePositionReq` at the execution layer. Carries the position ref (via `order.positionId`)
/// so the bridge correlates it back to the reduce order client id.
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
    let event = ProtoOaExecutionEvent {
        ctid_trader_account_id: 99,
        execution_type: ProtoOaExecutionType::OrderRejected as i32,
        order: Some(order),
        error_code: Some("CLOSE_REJECTED".to_string()),
        ..Default::default()
    };
    (pt::EXECUTION_EVENT, event.encode_to_vec())
}

/// A protocol `ERROR_RES` body for a refused `ProtoOAClosePositionReq` — the serve loop encodes it
/// with the request's `clientMsgId` (the order client id) so `on_error_res` correlates it. The error code
/// is a refusal of the venue's table (`crate::transport::error_codes`), and no refused grant, so the actor
/// opens no new session for it (`is_auth_error`).
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
    // A held close is released on time only if the loop wakes: a short read timeout for that policy alone.
    let wake = if matches!(policy, CloseServePolicy::HoldFirstFor(_)) {
        Duration::from_millis(20)
    } else {
        Duration::from_secs(5)
    };
    stream.set_read_timeout(Some(wake)).ok();
    let mut writer = stream.try_clone().expect("clone fake stream");
    let (eof_stream, eof) = EofStream::new(stream);
    let mut reader = FrameReader::new(eof_stream);
    // The close `HoldFirstFor` holds: the request, its order and deal ids, and when it fills.
    let mut held: Option<(ProtoOaClosePositionReq, i64, i64, std::time::Instant)> = None;

    let mut positions: std::collections::HashMap<i64, ServerPosition> =
        std::collections::HashMap::new();
    // Positions that already exist before the client connects (the pre-existing-book flatten case).
    for (position_id, trade_side, volume) in &preseed {
        positions.insert(
            *position_id,
            ServerPosition {
                symbol_id: 1,
                trade_side: *trade_side,
                volume: *volume,
                open_timestamp: 0,
            },
        );
    }
    let mut next_position_id = CLOSE_POSITION_ID_BASE + preseed.len() as i64;
    let mut next_order_id = FAKE_ORDER_ID;
    let mut next_deal_id = 900_001_i64;
    let mut next_open_timestamp = 1_784_000_000_000_i64;
    let mut close_count = 0usize;
    let mut reconcile_count = 0usize;

    loop {
        if let Some((req, order_id, deal_id, _)) =
            held.take_if(|h| std::time::Instant::now() >= h.3)
            && let Some(pos) = positions.get_mut(&req.position_id)
        {
            for (reply_type, body) in close_position_events(&req, pos, order_id, deal_id) {
                if writer.write_all(&framing::encode(reply_type, &body, "srv")).is_err() {
                    return;
                }
                let _ = writer.flush();
            }
        }
        match reader.next_frame() {
            Ok(Some(msg)) => {
                let payload_type = msg.payload_type;
                seen.lock().expect("seen lock").push(payload_type);
                // Close responses echo the request's `clientMsgId` (the order client id) so an ERROR_RES
                // correlates back to the reduce; everything else uses the generic "srv" id.
                let mut reply_message_id = "srv".to_string();
                let refuse_reconcile = payload_type == pt::RECONCILE_REQ && {
                    reconcile_count += 1;
                    fidelity.refuses(reconcile_count)
                };
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
                                let open_timestamp_value = next_open_timestamp;
                                next_open_timestamp += 1000;
                                positions.insert(
                                    position_id,
                                    ServerPosition {
                                        symbol_id: order.symbol_id,
                                        trade_side: order.trade_side,
                                        volume: order.volume,
                                        open_timestamp: open_timestamp_value,
                                    },
                                );
                                execution_events(
                                    &order,
                                    order_id,
                                    deal_id,
                                    Some((position_id, open_timestamp_value)),
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
                                reply_message_id = msg.client_msg_id.clone().unwrap_or_default();
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
                                    CloseAction::HoldFor(d) => {
                                        held = Some((
                                            req,
                                            order_id,
                                            deal_id,
                                            std::time::Instant::now() + d,
                                        ));
                                        Vec::new()
                                    }
                                    CloseAction::DropTheSocket => return,
                                    CloseAction::GoDeaf => {
                                        push_while_deaf(&mut writer, &mut reader, &eof);
                                        return;
                                    }
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
                    // position seed observable end to end: without it, a fresh start is blind to
                    // exactly the book `close_all` had to be invented for.
                    // The venue REFUSES this reconcile (an `ERROR_RES`, so the client's wait ends on the spot,
                    // not at its deadline).
                    pt::RECONCILE_REQ if refuse_reconcile => vec![close_error_res_body()],
                    // The orders of ONE position, scripted by the [`ExitLookup`] of a `RestingExit` venue
                    // (the only fidelity that reports an order naming a position it does not list open).
                    pt::ORDER_LIST_BY_POSITION_ID_REQ => {
                        let bounded = ProtoOaOrderListByPositionIdReq::decode(
                            msg.payload.as_deref().unwrap_or(&[]),
                        )
                        .map(|req| req.from_timestamp.is_some() || req.to_timestamp.is_some())
                        .unwrap_or(false);
                        let lookup = match fidelity {
                            ReconcileFidelity::RestingExit(lookup) => lookup,
                            _ => ExitLookup::Refused,
                        };
                        match orders_of_gone_position(lookup, bounded) {
                            Some(res) => {
                                vec![(pt::ORDER_LIST_BY_POSITION_ID_RES, res.encode_to_vec())]
                            }
                            None => vec![close_error_res_body()],
                        }
                    }
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
                                        open_timestamp: Some(p.open_timestamp),
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
                            order: fidelity.pending_orders(),
                            position: open,
                            ..Default::default()
                        };
                        vec![(pt::RECONCILE_RES, res.encode_to_vec())]
                    }
                    // Handshake / everything else → the shared happy-path scripting.
                    _ => script_reply(&msg, script, false),
                };
                for (reply_type, body) in frames {
                    // The venue echoes a request's id on its refusal; a reconcile's is empty, and the client reads
                    // that as the round trip's own answer (`read_reply`).
                    let message_id = if reply_type == pt::ERROR_RES {
                        msg.client_msg_id.as_deref().unwrap_or_default()
                    } else {
                        reply_message_id.as_str()
                    };
                    let frame = framing::encode(reply_type, &body, message_id);
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

/// The single `ORDER_FILLED` a close produces: a closing deal that reduces `pos` by the requested
/// volume (`tradeSide` OPPOSITE the position's own, so it nets toward flat), plus the updated
/// `position` ref (OPEN with the reduced volume, or CLOSED when fully flattened). Mutates `pos`'s
/// remaining volume in place so a second close of the same position sees the reduced size.
fn close_position_events(
    req: &ProtoOaClosePositionReq,
    pos: &mut ServerPosition,
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
            open_timestamp: Some(pos.open_timestamp),
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
