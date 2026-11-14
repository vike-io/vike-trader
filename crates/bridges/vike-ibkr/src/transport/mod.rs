//! The transport seam: the ONE trait the (future) three backends implement. Phase 1 ships the
//! socket backend (Task 9) + a `IbkrTransportFake` double so the whole exec lifecycle is testable
//! without a Gateway. `IbkrInbound` is the normalized event set the reader loop folds through the
//! `EventMapper`.
//!
//! The trait is deliberately tiny and blocking: `place_order`/`cancel_order`/`request_open_orders`
//! are fire-and-forget, and `next_recv` is a bounded blocking poll so the exec loop can interleave
//! transport inbound with the command channel (see `execution::run_execution`).

use std::time::Duration;

/// One gate per IB Gateway address: the connect turn, the rebuild schedule of a gateway that lost its link.
#[cfg(feature = "ibkr-socket")]
pub(crate) mod gateway_gate;
/// The link watchdog: does the gateway still have its link to IBKR (the connectivity notices).
#[cfg(feature = "ibkr-socket")]
pub(crate) mod link_watch;
#[cfg(feature = "ibkr-socket")]
pub(crate) mod socket;

pub mod error_codes;

#[cfg(feature = "ibkr-socket")]
pub use socket::IbkrTransportSocket;

#[cfg(feature = "ibkr-cpapi")]
// `pub(crate)`: the cpapi market-data client (`crate::feed::cpapi`) rides the same gateway WS code
// (`connect_websocket`, `websocket_url_from`) the `sor` pump uses (decision 0140).
pub(crate) mod cpapi;
/// Re-exported so the live smoke can do a read-only session-account guard before placing an order
/// (`tests/ibkr_smokes/ibkr_cpapi_smoke.rs`): cpapi routes orders by the gateway's authenticated SESSION, not
/// `config.account`, so the `.env`-string `DU` guard alone cannot prove the session is paper.
#[cfg(feature = "ibkr-cpapi")]
pub use cpapi::CpapiRest;
#[cfg(feature = "ibkr-cpapi")]
pub use cpapi::IbkrTransportCpapi;
/// Re-exported pure decoder surface for the no-network fixture tests (`tests/ibkr_cpapi_decode.rs`).
#[cfg(feature = "ibkr-cpapi")]
pub use cpapi::decode as cpapi_decode;

use crate::execution::event_mapper::{
    IbkrCommissionReport, IbkrExecutionDetails, IbkrOpenOrder, IbkrOrderStatus,
};
use crate::execution::order::IbkrOrderSpec;
use crate::instrument::contract::IbkrContract;

/// Which request a [`IbkrInbound::SnapshotEnd`] closes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SnapshotRequest {
    /// `reqAllOpenOrders`.
    OpenOrders,
    /// `reqExecutions` (with the commission reports).
    Executions,
}

/// Who an order has to belong to for this connection to rebind it: THIS API client, in THIS account.
/// `reqAllOpenOrders` answers for every client of the login (and, on a login that manages several
/// accounts, for every account), including orders made by hand in TWS, which carry client id 0 and no
/// `orderRef`. Binding such a row as ours would corrupt the id map (an `orderId` 0 row bound to an empty
/// ref swallows the next id-less rejection) and attribute a stranger's fills to an order client id nobody knows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OwnOrders {
    pub client_id: i32,
    /// The configured account; EMPTY = the login's default (any account the client id placed in).
    pub account: String,
}

impl OwnOrders {
    /// Whether `row` is an order this client placed: its client id, a non-empty `orderRef` (ours always
    /// carries the order client id) and, when an account is configured, that account.
    pub fn owns(&self, row: &IbkrOpenOrder) -> bool {
        row.client_id == self.client_id
            && looks_like_an_order_client_id(&row.order_ref)
            && (self.account.trim().is_empty() || row.account.trim() == self.account.trim())
    }
}

/// The shape of an `orderRef` THIS bridge sets (`execution::order::map_order_request`: the order client id, verbatim): a machine
/// token, never blank, at most 128 characters, no whitespace or control characters. A human's hand-typed
/// `Order Ref` on a manual order ("my hedge") does not pass, so even a client id the operator shares with
/// TWS's own manual orders does not make one adoptable (review 18, R4; the master client is refused too).
pub(crate) fn looks_like_an_order_client_id(order_ref: &str) -> bool {
    !order_ref.is_empty()
        && order_ref.len() <= 128
        && order_ref.chars().all(|c| !c.is_whitespace() && !c.is_control())
}

/// Whether `row` has the shape a BLIND decoder gives every row: no client id (0), no account, and an `orderRef` that
/// looks like one of ours. A row whose client id or account DID decode is a decoded foreign row (another API
/// client's, a hand-made TWS order, another account's), whatever its `orderRef` says, and is not this: it is what
/// the learning's "suspicious snapshot" judgement counts (review of W14e, finding 2).
pub(crate) fn is_undecoded_tagged(row: &IbkrOpenOrder) -> bool {
    row.client_id == 0
        && row.account.trim().is_empty()
        && looks_like_an_order_client_id(&row.order_ref)
}

#[cfg(test)]
mod own_tests {
    use super::*;

    fn row(client_id: i32, order_ref: &str, account: &str) -> IbkrOpenOrder {
        IbkrOpenOrder {
            order_id: 5,
            order_ref: order_ref.into(),
            client_id,
            account: account.into(),
            total_qty: 1.0,
            ..Default::default()
        }
    }

    fn own() -> OwnOrders {
        OwnOrders { client_id: 7, account: "DU1".into() }
    }

    #[test]
    fn an_order_is_ours_by_client_id_account_and_an_order_client_id_shaped_ref() {
        assert!(own().owns(&row(7, "order-client-id-1", "DU1")));
        assert!(own().owns(&row(7, "019a-bc_12.x", " DU1 ")));
        assert!(!own().owns(&row(3, "order-client-id-1", "DU1")), "another client");
        assert!(!own().owns(&row(7, "order-client-id-1", "DU2")), "another account");
        assert!(!own().owns(&row(7, "", "DU1")), "no ref: a manual order");
        assert!(!own().owns(&row(0, "", "DU1")));
    }

    /// R4: a human's hand-typed `Order Ref` on a manual order in the same account, even under our client id.
    #[test]
    fn a_hand_typed_order_ref_is_not_an_order_client_id() {
        assert!(!own().owns(&row(7, "my hedge", "DU1")), "spaces");
        assert!(!own().owns(&row(7, "tab	here", "DU1")));
        assert!(!own().owns(&row(7, &"x".repeat(129), "DU1")), "too long");
        let any_account = OwnOrders { client_id: 7, account: String::new() };
        assert!(
            any_account.owns(&row(7, "order-client-id-1", "DU9")),
            "no configured account: any"
        );
    }

    /// Finding 2: only the shape a BLIND decoder gives every row counts as undecoded and tagged. A row whose
    /// client id or account decoded is somebody else's order, whatever its `orderRef` says.
    #[test]
    fn only_a_row_with_nothing_decoded_and_an_order_client_id_shaped_ref_is_undecoded_and_tagged() {
        assert!(is_undecoded_tagged(&row(0, "order-client-id-1", "")), "the blind decoder's shape");
        assert!(is_undecoded_tagged(&row(0, "order-client-id-1", "  ")));
        assert!(
            !is_undecoded_tagged(&row(0, "my hedge", "")),
            "a hand-typed note is no order client id"
        );
        assert!(!is_undecoded_tagged(&row(0, "", "")), "no ref");
        assert!(
            !is_undecoded_tagged(&row(0, "note", "DU1")),
            "TWS's own order: the account decoded"
        );
        assert!(
            !is_undecoded_tagged(&row(3, "theirs", "")),
            "another API client: the client id decoded"
        );
        assert!(!is_undecoded_tagged(&row(3, "theirs", "DU2")));
    }
}

/// What a SOCKET connection queues the moment it is established: IB's next valid id, the account
/// readiness, and `Connected`, which makes the exec loop LEARN the venue (the open orders first, then
/// the day's executions). One function for the production `IbkrTransportSocket::start` and the test double
/// (`IbkrTransportFake::script_socket_connect`), so the two cannot drift.
pub fn socket_connect_seed(next_valid_id: i32) -> Vec<IbkrInbound> {
    vec![
        IbkrInbound::NextValidId(next_valid_id),
        IbkrInbound::AccountsReady,
        IbkrInbound::Connected,
    ]
}

/// Normalized inbound from any backend — the reader loop folds each through the `EventMapper`.
pub enum IbkrInbound {
    NextValidId(i32),
    AccountsReady,
    OrderStatus(IbkrOrderStatus),
    ExecutionDetails(IbkrExecutionDetails),
    Commission(IbkrCommissionReport),
    Error {
        code: i32,
        order_id: i32,
        msg: String,
    },
    /// The inbound stream dropped and the transport IS RE-ESTABLISHING it — the exec loop answers
    /// by re-requesting the state IB does not replay on its own (open orders + the day's
    /// executions). Only a backend that genuinely reconnects may send this: today that is the cpapi
    /// pump alone (`transport/cpapi`'s `pump_loop` re-dials in its own `while !stop` loop, and its
    /// resync requests go out over REST, a connection the WS drop did not touch).
    ///
    /// ⚠ This variant was called `Reconnected` and was sent by BOTH backends, including the socket
    /// one, which has no reconnect at all — see [`IbkrInbound::StreamDead`] for what that cost.
    StreamResync,
    /// The inbound stream terminated and NOTHING will re-establish it: this transport is dead for
    /// the remainder of its life, and every order still in flight has an UNKNOWN venue state.
    ///
    /// This exists because the socket backend used to report exactly this condition as
    /// `Reconnected`. Nothing reconnected. The exec loop stayed alive, kept accepting submits, and
    /// pushed them at a socket whose reader thread had already exited — where a first write lands in
    /// the kernel buffer and returns `Ok`, so not even the synchronous-failure reject fired. The
    /// order was then unreachable in both directions: no accept, no fill, no terminal, and no way to
    /// cancel it. A name that lied about its meaning is what let that sit, so the two conditions are
    /// two variants now and the transport must say which one it means.
    ///
    /// `reason` is the transport's own diagnostic, carried so the refusal an operator sees names the
    /// original fault rather than a generic "not connected".
    StreamDead {
        reason: String,
    },
    /// One row of an open-order snapshot (IB `openOrder` callback), replayed in response to
    /// `request_open_orders` — the reconnect-resync payload the mapper rebuilds its order client id⇄orderId
    /// map from (Task 10; `order_ref` IS the order client id).
    OpenOrder {
        order_id: i32,
        order_ref: String,
    },
    /// One row of the VENUE's open-order snapshot, with the identity of the order's owner and its
    /// quantity (the socket backend; see [`IbkrOpenOrder`]). Whether it is OURS is the exec loop's question
    /// ([`IbkrTransport::ownership`]).
    VenueOrder(IbkrOpenOrder),
    /// A fresh connection was established (socket backend): the exec loop starts LEARNING the venue.
    /// Never sent by a backend that re-dials by itself ([`IbkrInbound::StreamResync`] is theirs).
    Connected,
    /// A snapshot request ended: `complete` = the venue said so (`openOrderEnd`/`execDetailsEnd`),
    /// false = the request failed or timed out. Sent AFTER every row of that snapshot.
    SnapshotEnd {
        snapshot: SnapshotRequest,
        complete: bool,
    },
}

/// What a backend answers to [`IbkrTransport::modify_order`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AmendOutcome {
    /// The request was written and the venue will confirm it by re-sending the order (TWS answers a
    /// re-placed order with an `openOrder` row carrying the new terms): the exec loop waits for that echo and
    /// emits `OrderModified` from it, or `OrderModifyRejected` when it does not come in time.
    Sent,
    /// The backend vouches for the amend synchronously (the cpapi REST reply accepted it): `OrderModified` now.
    Acknowledged,
    /// Nothing was sent, or the venue refused it: `OrderModifyRejected` with these words. The order keeps its terms.
    Refused(String),
}

/// The ONE seam every IBKR backend (socket now; cpapi/oauth later) implements. `Send` so the exec
/// loop can own it on the `ExecutionClientActor` thread.
pub trait IbkrTransport: Send {
    /// Place `spec`/`contract` under the numeric IB `order_id` (already allocated by the caller).
    fn place_order(&mut self, order_id: i32, spec: &IbkrOrderSpec, contract: &IbkrContract);
    /// Cancel by numeric IB `order_id`. `Err` = the cancel DEFINITELY did not take, in words: nothing was sent (the
    /// cpapi backend that does not know the gateway's own id of the order) or the venue stated a refusal (the cpapi
    /// REST answer); the exec loop answers it with a non-terminal `OrderCancelRejected`, the order still live. An
    /// outcome the venue did not state is `Ok`, left to the reconcile.
    fn cancel_order(&mut self, order_id: i32) -> Result<(), String>;
    /// Reprice/resize a resting order in place, under its own numeric `order_id`, and say what became of
    /// the request ([`AmendOutcome`]). **Default: a refusal**, never a silent no-op: a backend that cannot
    /// amend must say so, and the exec loop turns that into a non-terminal `OrderModifyRejected`.
    /// The socket backend re-places the order under the SAME id (TWS reads that as a modification); the cpapi
    /// backend calls its amend endpoint.
    fn modify_order(
        &mut self,
        order_id: i32,
        spec: &IbkrOrderSpec,
        contract: &IbkrContract,
    ) -> AmendOutcome {
        let _ = (order_id, spec, contract);
        AmendOutcome::Refused("this IBKR backend has no way to amend a resting order".to_string())
    }
    /// Re-request all open orders (reconnect resync, Task 10). Executions are the SIBLING request
    /// below — this doc line used to also promise "+ recent executions", which no backend ever did.
    fn request_open_orders(&mut self);
    /// Ask the venue to re-deliver the CURRENT DAY's executions **and their commission reports**,
    /// so a fill whose commissionReport was lost can still be emitted with the REAL commission
    /// (`event_mapper`'s module doc trap 5). Driven from two places in `execution::run_execution`: the
    /// reconnect resync, and the stranded-fill sweep.
    ///
    /// Fire-and-forget like its siblings — the replayed rows arrive through the ordinary
    /// `next_recv` inbound stream as `ExecutionDetails` + `Commission`, not as a return value. Replaying
    /// already-emitted pairs is safe: `EventMapper`'s `emitted` index drops both halves.
    ///
    /// **Default no-op.** A backend that cannot replay executions inherits it, and its stranded
    /// fills escalate to the operator instead of silently recovering. The cpapi backend inherits it
    /// deliberately, for a stronger reason: it decodes ONE `sor` execution row into BOTH halves at
    /// once (`transport/cpapi/decode.rs`), so a cpapi fill structurally cannot strand.
    fn request_executions(&mut self) {}
    /// The orders the host restored after a restart that this connection does not hold (their order client ids), asked when
    /// the host teaches them: those the venue still lists, as `(numeric order id, order client id)` for the exec loop to bind,
    /// so a cancel or amend of one reaches the order. **Default: none**, which is the socket backend's answer: it
    /// learns its orders from the venue's open orders at every connect and takes no venue order id from the host
    /// (IB's numeric id belongs to one API client id). The cpapi backend reads its order list by our order client id.
    fn locate_restored(&mut self, order_client_ids: &[String]) -> Vec<(i32, String)> {
        let _ = order_client_ids;
        Vec::new()
    }
    /// Block up to `timeout` for the next inbound; `None` on timeout (lets the loop poll the cmd
    /// channel and observe `Shutdown`).
    fn next_recv(&mut self, timeout: Duration) -> Option<IbkrInbound>;
    /// Whose orders a venue open-order row has to belong to for this connection to rebind it
    /// ([`OwnOrders`]). `None` = the backend has no such rows.
    fn ownership(&self) -> Option<OwnOrders> {
        None
    }
    /// Whether the gateway says, right now, that it has no link to IBKR's servers (the socket backend's
    /// `link_watch::LinkMonitor`). **Default `false`**: a backend that does not watch has no such word.
    fn server_link_broken(&self) -> bool {
        false
    }
    /// What the account holds of `contract` now, net and signed (long positive): the read a close that waited asks
    /// before it goes out (`crate::execution`'s `size_late_close`). It blocks the exec thread, so a backend bounds it.
    /// **Default: an error**, never a figure: a backend that cannot read says so.
    fn held_position(&mut self, contract: &IbkrContract) -> Result<f64, String> {
        let _ = contract;
        Err("this IBKR backend cannot read a position".to_string())
    }
}

/// The bounded blocking poll both live backends' [`IbkrTransport::next_recv`] make on their pump's
/// channel. A timeout is `None`, which lets the exec loop poll its command channel (Shutdown). A pump
/// that ended (stream closed) is `None` too, so the loop keeps servicing commands, but only after
/// sleeping `timeout`: recv on an already-disconnected channel returns immediately, so without the
/// sleep the exec loop would busy-spin a core once the pump thread has exited.
#[cfg(any(feature = "ibkr-socket", feature = "ibkr-cpapi"))]
fn recv_inbound(
    inbound: &std::sync::mpsc::Receiver<IbkrInbound>,
    timeout: Duration,
) -> Option<IbkrInbound> {
    match inbound.recv_timeout(timeout) {
        Ok(inbound) => Some(inbound),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => None,
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            std::thread::sleep(timeout);
            None
        }
    }
}

#[cfg(all(test, any(feature = "ibkr-socket", feature = "ibkr-cpapi")))]
mod recv_inbound_tests {
    use super::*;

    /// Both pre-merge `next_recv` copies (socket, cpapi): an inbound comes through, a quiet channel is
    /// `None` at the timeout, and an ended pump is `None` only after sleeping that timeout.
    #[test]
    fn an_ended_pump_is_none_after_its_timeout_never_a_spin() {
        let t = Duration::from_millis(40);
        let (tx, rx) = std::sync::mpsc::channel();
        tx.send(IbkrInbound::NextValidId(7)).unwrap();
        assert!(matches!(recv_inbound(&rx, t), Some(IbkrInbound::NextValidId(7))));
        assert!(recv_inbound(&rx, t).is_none(), "a quiet channel times out");
        drop(tx);
        let started = std::time::Instant::now();
        assert!(recv_inbound(&rx, t).is_none(), "an ended pump reads as a timeout");
        assert!(started.elapsed() >= t, "an ended pump costs its timeout, never a busy spin");
    }
}

// ---------------------------------------------------------------------------------------------
// IbkrTransportFake — the no-Gateway test double. Behind `test`/`test-support` so it never ships in a
// default build. It carries a scripted inbound queue (`script`) plus an `on_place_order` hook that
// enqueues follow-up inbounds parameterized by the numeric order_id the exec loop allocated, so a
// full submit→accept→fill lifecycle folds through the real EventMapper with no socket.
// ---------------------------------------------------------------------------------------------

#[cfg(any(test, feature = "test-support"))]
pub use fake::{FAKE_ACCOUNT, FAKE_CLIENT_ID, IbkrTransportFake, ScriptedInbound, SnapshotMode};

#[cfg(any(test, feature = "test-support"))]
mod fake {
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use crate::execution::event_mapper::{
        IbkrCommissionReport, IbkrExecutionDetails, IbkrOpenOrder, IbkrOrderStatus,
    };
    use crate::execution::order::IbkrOrderSpec;
    use crate::instrument::contract::IbkrContract;

    use super::{
        AmendOutcome, IbkrInbound, IbkrTransport, OwnOrders, SnapshotRequest, socket_connect_seed,
    };

    /// The API client id a scripted venue order is placed under unless a test says otherwise, and the
    /// identity [`IbkrTransportFake::with_socket_identity`] declares as "ours".
    pub const FAKE_CLIENT_ID: i32 = 7;
    /// The account a scripted venue order is placed in unless a test says otherwise.
    pub const FAKE_ACCOUNT: &str = "DU1234567";

    /// How the fake answers a snapshot request once its rows are queued.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum SnapshotMode {
        /// The venue says the snapshot ended (`openOrderEnd`).
        Complete,
        /// The request failed (or timed out): the transport says it did NOT complete.
        Failed,
        /// Nothing ends it (a transport whose end marker never comes).
        Silent,
    }

    /// A scripted inbound with the minimal fields a lifecycle test cares about; the rest are filled
    /// with sensible defaults when lowered into a full [`IbkrInbound`]. Keeping this reduced set
    /// separate from `IbkrInbound` keeps test scripts terse.
    pub enum ScriptedInbound {
        NextValidId(i32),
        AccountsReady,
        OrderStatus {
            order_id: i32,
            order_ref: String,
            status: String,
        },
        ExecutionDetails {
            order_id: i32,
            exec_id: String,
            shares: f64,
            price: f64,
        },
        Commission {
            exec_id: String,
            commission: f64,
        },
        Error {
            code: i32,
            order_id: i32,
            msg: String,
        },
        StreamResync,
        /// The terminal-death notice (`IbkrInbound::StreamDead`) — what the socket backend's pump
        /// sends when its stream ends for good, so a test can drive the refusal path.
        StreamDead {
            reason: String,
        },
        /// One row of a scripted open-order snapshot, replayed via `on_request_open_orders`
        /// (Task 10 reconnect resync).
        OpenOrder {
            order_id: i32,
            order_ref: String,
        },
        /// One row of the VENUE's open-order snapshot, as the socket backend decodes it. Use
        /// [`ScriptedInbound::venue_order`] for the usual shape and struct-update for the unusual one.
        VenueOrder {
            order_id: i32,
            order_ref: String,
            client_id: i32,
            account: String,
            total_qty: f64,
            status: String,
            /// What the venue says is already filled (all days).
            filled_qty: f64,
        },
        /// An `openOrder` echo of a working order of this client WITH its prices: what TWS sends after an
        /// order is re-placed under its own id (an amend). `limit_price` is a limit order's, `aux_price` a stop's.
        VenueOrderPriced {
            order_id: i32,
            order_ref: String,
            total_qty: f64,
            limit_price: Option<f64>,
            aux_price: Option<f64>,
        },
        /// A fresh connection was established (what `socket_connect_seed` ends with).
        Connected,
        /// A snapshot request ended.
        SnapshotEnd {
            snapshot: SnapshotRequest,
            complete: bool,
        },
        /// An execution with the order reference and the instrument spelled out (the replay of a day's
        /// executions carries them; the terse [`ScriptedInbound::ExecutionDetails`] does not).
        ExecutionOf {
            order_id: i32,
            order_ref: String,
            exec_id: String,
            symbol: String,
            shares: f64,
            price: f64,
            /// The API client that placed the order (the replay carries it).
            client_id: i32,
        },
    }

    impl ScriptedInbound {
        /// A working order of this client (`FAKE_CLIENT_ID`, `FAKE_ACCOUNT`, status `Submitted`).
        pub fn venue_order(order_id: i32, order_ref: &str, total_qty: f64) -> Self {
            ScriptedInbound::VenueOrder {
                order_id,
                order_ref: order_ref.to_string(),
                client_id: FAKE_CLIENT_ID,
                account: FAKE_ACCOUNT.to_string(),
                total_qty,
                status: "Submitted".to_string(),
                filled_qty: 0.0,
            }
        }

        /// Lower a terse script entry into the full normalized inbound the reader loop folds. Fills
        /// the fields the exec lifecycle doesn't script (symbol/side/ts/currency) with defaults: a
        /// BUY in USD at ts 0, symbol left empty (the EventMapper carries it onto the fill, which
        /// the lifecycle assertion doesn't inspect).
        fn into_inbound(self) -> IbkrInbound {
            match self {
                ScriptedInbound::NextValidId(id) => IbkrInbound::NextValidId(id),
                ScriptedInbound::AccountsReady => IbkrInbound::AccountsReady,
                ScriptedInbound::OrderStatus { order_id, order_ref, status } => {
                    IbkrInbound::OrderStatus(IbkrOrderStatus {
                        order_id,
                        order_ref,
                        status,
                        filled: 0.0,
                        average_fill_price: 0.0,
                        ..Default::default()
                    })
                }
                ScriptedInbound::ExecutionDetails { order_id, exec_id, shares, price } => {
                    IbkrInbound::ExecutionDetails(IbkrExecutionDetails {
                        order_id,
                        order_ref: String::new(),
                        exec_id,
                        symbol: String::new(),
                        side_buy: true,
                        shares,
                        price,
                        ts: 0,
                        ..Default::default()
                    })
                }
                ScriptedInbound::Commission { exec_id, commission } => {
                    IbkrInbound::Commission(IbkrCommissionReport {
                        exec_id,
                        commission,
                        currency: "USD".to_string(),
                    })
                }
                ScriptedInbound::Error { code, order_id, msg } => {
                    IbkrInbound::Error { code, order_id, msg }
                }
                ScriptedInbound::StreamResync => IbkrInbound::StreamResync,
                ScriptedInbound::StreamDead { reason } => IbkrInbound::StreamDead { reason },
                ScriptedInbound::OpenOrder { order_id, order_ref } => {
                    IbkrInbound::OpenOrder { order_id, order_ref }
                }
                ScriptedInbound::VenueOrder {
                    order_id,
                    order_ref,
                    client_id,
                    account,
                    total_qty,
                    status,
                    filled_qty,
                } => IbkrInbound::VenueOrder(IbkrOpenOrder {
                    order_id,
                    order_ref,
                    client_id,
                    account,
                    total_qty,
                    status,
                    filled_qty,
                    ..Default::default()
                }),
                ScriptedInbound::VenueOrderPriced {
                    order_id,
                    order_ref,
                    total_qty,
                    limit_price,
                    aux_price,
                } => IbkrInbound::VenueOrder(IbkrOpenOrder {
                    order_id,
                    order_ref,
                    client_id: FAKE_CLIENT_ID,
                    account: FAKE_ACCOUNT.to_string(),
                    total_qty,
                    status: "Submitted".to_string(),
                    limit_price,
                    aux_price,
                    ..Default::default()
                }),
                ScriptedInbound::Connected => IbkrInbound::Connected,
                ScriptedInbound::SnapshotEnd { snapshot, complete } => {
                    IbkrInbound::SnapshotEnd { snapshot, complete }
                }
                ScriptedInbound::ExecutionOf {
                    order_id,
                    order_ref,
                    exec_id,
                    symbol,
                    shares,
                    price,
                    client_id,
                } => IbkrInbound::ExecutionDetails(IbkrExecutionDetails {
                    order_id,
                    order_ref,
                    exec_id,
                    symbol,
                    side_buy: true,
                    shares,
                    price,
                    ts: 0,
                    client_id,
                    ..Default::default()
                }),
            }
        }
    }

    /// `Some(complete)` = the transport sends a `SnapshotEnd` with that flag; `None` = it sends none.
    fn end_flag(mode: SnapshotMode) -> Option<bool> {
        match mode {
            SnapshotMode::Complete => Some(true),
            SnapshotMode::Failed => Some(false),
            SnapshotMode::Silent => None,
        }
    }

    type PlaceHook = Box<dyn FnMut(i32) -> Vec<ScriptedInbound> + Send>;
    type OpenOrdersHook = Box<dyn FnMut() -> Vec<ScriptedInbound> + Send>;
    type ExecutionsHook = Box<dyn FnMut() -> Vec<ScriptedInbound> + Send>;

    /// The shared, lockable state behind a [`IbkrTransportFake`] handle — see there for why it's Arc'd.
    #[derive(Default)]
    struct Inner {
        inbound: VecDeque<IbkrInbound>,
        on_place_order: Option<PlaceHook>,
        on_request_open_orders: Option<OpenOrdersHook>,
        on_request_executions: Option<ExecutionsHook>,
        /// Records the numeric `order_id` of each `modify_order` call (Task 3 wiring assertion).
        modify_calls: Arc<Mutex<Vec<i32>>>,
        /// The spec each `modify_order` call was handed, in call order: what a real backend writes to the wire.
        modify_specs: Arc<Mutex<Vec<IbkrOrderSpec>>>,
        /// What `modify_order` answers (default: [`AmendOutcome::Sent`]).
        modify_outcome: Option<AmendOutcome>,
        /// Enqueues follow-up inbounds when `modify_order` is called (the venue's echo of the amended order).
        on_modify_order: Option<PlaceHook>,
        /// Records the numeric `order_id` of each `place_order` call, so a test can assert an order
        /// REACHED the transport — or, for the stream-death refusal, that it did NOT. Counting
        /// emitted events alone cannot tell those apart: a refusal and a placement that is never
        /// answered look identical from the event lane.
        place_calls: Arc<Mutex<Vec<i32>>>,
        /// Records the numeric `order_id` of each `cancel_order` call. The stream-death refusal
        /// asserts this stays EMPTY: the fake's `cancel_order` emits nothing either way, so a test
        /// that only checked "no event was produced" would pass with the refusal deleted.
        cancel_calls: Arc<Mutex<Vec<i32>>>,
        /// Counts `request_executions` calls, so a test can assert the commission-recovery
        /// re-request actually reached the transport (`event_mapper` module doc trap 5).
        execution_requests: Arc<Mutex<u32>>,
        /// Counts `request_open_orders` calls.
        open_order_requests: Arc<Mutex<u32>>,
        /// Records the `IbkrContract` each `place_order` call was handed — the thing a REAL
        /// backend turns into the wire contract. `place_calls` above records only the numeric id,
        /// which cannot tell a futures order from an equity one: the silent-equity-default bug was
        /// invisible to every existing assertion here precisely because nothing looked at this.
        place_contracts: Arc<Mutex<Vec<IbkrContract>>>,
        /// The identity of "our" orders, declared by [`IbkrTransportFake::with_socket_identity`].
        own: Option<OwnOrders>,
        /// How `request_open_orders` / `request_executions` end (default: complete).
        open_orders_end: Option<SnapshotMode>,
        executions_end: Option<SnapshotMode>,
        /// How the next open-order requests end, one entry per request, before `open_orders_end` applies.
        open_orders_ends: VecDeque<SnapshotMode>,
        /// What [`IbkrTransport::server_link_broken`] answers.
        server_link_broken: bool,
        /// The spec each `place_order` call was handed, in call order: what a real backend writes to the wire.
        place_specs: Arc<Mutex<Vec<IbkrOrderSpec>>>,
        /// What [`IbkrTransport::held_position`] answers (`None`: the trait's own refusal).
        held_position: Option<Result<f64, String>>,
        /// What the next `held_position` calls answer, one each, before [`Inner::held_position`] applies.
        held_answers: VecDeque<Result<f64, String>>,
        /// Counts `held_position` calls: a close that did not wait reads nothing.
        held_reads: Arc<Mutex<u32>>,
    }

    /// Scriptable [`IbkrTransport`] double — no Gateway, no ibapi. Push initial inbounds with
    /// [`IbkrTransportFake::script`]; register [`IbkrTransportFake::on_place_order`] to enqueue follow-ups
    /// parameterized by the numeric order_id the exec loop allocates on submit, and
    /// [`IbkrTransportFake::on_request_open_orders`] for the reconnect-resync snapshot (Task 10).
    ///
    /// `Clone`-able and backed by `Arc<Mutex<Inner>>`: the exec loop owns one clone on its own
    /// thread (moved into `run_execution_for_test`'s `Box<dyn IbkrTransport>`) while the test keeps a
    /// second clone to [`IbkrTransportFake::inject`] inbounds asynchronously (e.g. a `StreamResync`
    /// mid-lifecycle) — both clones read/write the SAME queue.
    #[derive(Clone, Default)]
    pub struct IbkrTransportFake(Arc<Mutex<Inner>>);

    impl IbkrTransportFake {
        pub fn new() -> Self {
            Self::default()
        }

        /// Queue a scripted inbound to be delivered by [`IbkrTransport::next_recv`] in FIFO order.
        pub fn script(&mut self, inbound: ScriptedInbound) {
            self.0.lock().unwrap().inbound.push_back(inbound.into_inbound());
        }

        /// Register the place-order hook: when the exec loop calls `place_order(order_id, ..)` the
        /// hook's returned inbounds (parameterized by `order_id`) are appended to the queue, so a
        /// Submitted → execDetails → commission sequence folds through the real EventMapper.
        pub fn on_place_order<F>(&mut self, hook: F)
        where
            F: FnMut(i32) -> Vec<ScriptedInbound> + Send + 'static,
        {
            self.0.lock().unwrap().on_place_order = Some(Box::new(hook));
        }

        /// Register the reconnect-resync hook: when the exec loop calls `request_open_orders()`
        /// (in response to a `StreamResync` inbound) the hook's returned open-order rows are
        /// appended to the queue — the scripted twin of IB replaying its open-order snapshot.
        pub fn on_request_open_orders<F>(&mut self, hook: F)
        where
            F: FnMut() -> Vec<ScriptedInbound> + Send + 'static,
        {
            self.0.lock().unwrap().on_request_open_orders = Some(Box::new(hook));
        }

        /// Push a scripted inbound onto the SAME queue the running exec loop drains, from a
        /// separate clone of this handle — how a test injects `StreamResync` (or any other inbound)
        /// asynchronously once `run_execution_for_test` has already moved a clone onto its thread.
        pub fn inject(&self, inbound: ScriptedInbound) {
            self.0.lock().unwrap().inbound.push_back(inbound.into_inbound());
        }

        /// Queue what a SOCKET connection queues when it is established ([`socket_connect_seed`]): IB's
        /// next valid id, the account readiness and `Connected`. The same function the production
        /// `IbkrTransportSocket::start` calls.
        pub fn script_socket_connect(&mut self, next_valid_id: i32) {
            let mut inner = self.0.lock().unwrap();
            for inbound in socket_connect_seed(next_valid_id) {
                inner.inbound.push_back(inbound);
            }
        }

        /// Declare which orders are "ours": a client id of [`FAKE_CLIENT_ID`] in [`FAKE_ACCOUNT`], the
        /// way the socket backend answers [`IbkrTransport::ownership`].
        #[must_use]
        pub fn with_socket_identity(self) -> Self {
            self.0.lock().unwrap().own =
                Some(OwnOrders { client_id: FAKE_CLIENT_ID, account: FAKE_ACCOUNT.to_string() });
            self
        }

        /// Whether the gateway says it has no link to IBKR's servers.
        pub fn set_server_link_broken(&mut self, broken: bool) {
            self.0.lock().unwrap().server_link_broken = broken;
        }

        /// How the open-order request ends (default: the venue says it ended).
        pub fn open_orders_end(&mut self, mode: SnapshotMode) {
            self.0.lock().unwrap().open_orders_end = Some(mode);
        }

        /// How the executions request ends (default: the venue says it ended).
        pub fn executions_end(&mut self, mode: SnapshotMode) {
            self.0.lock().unwrap().executions_end = Some(mode);
        }

        /// How the next open-order requests end, one entry per request (a first attempt that fails and a second
        /// that completes, say); once the list is used up [`IbkrTransportFake::open_orders_end`] applies.
        pub fn open_orders_ends(&mut self, modes: &[SnapshotMode]) {
            self.0.lock().unwrap().open_orders_ends = modes.iter().copied().collect();
        }

        /// How many scripted inbounds are still queued, not yet read by the exec loop.
        pub fn undelivered(&self) -> usize {
            self.0.lock().unwrap().inbound.len()
        }

        /// Register the execution-replay hook: when the exec loop calls `request_executions()` the
        /// hook's returned inbounds are appended to the queue — the scripted twin of IBKR replaying
        /// the day's executions AND their commission reports in response to `reqExecutions`.
        pub fn on_request_executions<F>(&mut self, hook: F)
        where
            F: FnMut() -> Vec<ScriptedInbound> + Send + 'static,
        {
            self.0.lock().unwrap().on_request_executions = Some(Box::new(hook));
        }

        /// Answer every `modify_order` with `outcome` (default [`AmendOutcome::Sent`]).
        pub fn modify_outcome(&mut self, outcome: AmendOutcome) {
            self.0.lock().unwrap().modify_outcome = Some(outcome);
        }

        /// Register the amend hook: when the exec loop calls `modify_order(order_id, ..)` the hook's inbounds
        /// (parameterized by `order_id`) are appended to the queue, the scripted twin of TWS echoing the
        /// amended order as an `openOrder` row.
        pub fn on_modify_order<F>(&mut self, hook: F)
        where
            F: FnMut(i32) -> Vec<ScriptedInbound> + Send + 'static,
        {
            self.0.lock().unwrap().on_modify_order = Some(Box::new(hook));
        }

        /// A handle to the specs `modify_order` was handed, in call order.
        pub fn modify_specs(&self) -> Arc<Mutex<Vec<IbkrOrderSpec>>> {
            Arc::clone(&self.0.lock().unwrap().modify_specs)
        }

        /// A handle to the recorded `modify_order` order ids (Task 3 wiring assertion) — a clone of
        /// the shared vec the transport pushes into.
        pub fn modify_calls(&self) -> Arc<Mutex<Vec<i32>>> {
            Arc::clone(&self.0.lock().unwrap().modify_calls)
        }

        /// A handle to the `request_executions` call counter — a clone of the shared cell the
        /// transport increments.
        pub fn execution_requests(&self) -> Arc<Mutex<u32>> {
            Arc::clone(&self.0.lock().unwrap().execution_requests)
        }

        /// A handle to the `request_open_orders` call counter.
        pub fn open_order_requests(&self) -> Arc<Mutex<u32>> {
            Arc::clone(&self.0.lock().unwrap().open_order_requests)
        }

        /// A handle to the recorded `place_order` order ids — a clone of the shared vec the
        /// transport pushes into. EMPTY is the assertion that matters for the stream-death refusal.
        pub fn place_calls(&self) -> Arc<Mutex<Vec<i32>>> {
            Arc::clone(&self.0.lock().unwrap().place_calls)
        }

        /// A handle to the CONTRACTS `place_order` was handed, in call order — see
        /// [`Inner::place_contracts`]. This is what an order-path test asserts on when it wants to
        /// know WHICH INSTRUMENT was submitted, not merely that something was.
        pub fn place_contracts(&self) -> Arc<Mutex<Vec<IbkrContract>>> {
            Arc::clone(&self.0.lock().unwrap().place_contracts)
        }

        /// A handle to the recorded `cancel_order` order ids — see [`Inner::cancel_calls`] for why
        /// the stream-death test asserts on this rather than on the absence of an event.
        pub fn cancel_calls(&self) -> Arc<Mutex<Vec<i32>>> {
            Arc::clone(&self.0.lock().unwrap().cancel_calls)
        }

        /// A handle to the specs `place_order` was handed, in call order.
        pub fn place_specs(&self) -> Arc<Mutex<Vec<IbkrOrderSpec>>> {
            Arc::clone(&self.0.lock().unwrap().place_specs)
        }

        /// What every later position read answers.
        pub fn set_held_position(&self, held: Result<f64, String>) {
            self.0.lock().unwrap().held_position = Some(held);
        }

        /// What the next position reads answer, one each, before [`IbkrTransportFake::set_held_position`] applies.
        pub fn script_held_reads(&self, answers: &[Result<f64, String>]) {
            self.0.lock().unwrap().held_answers.extend(answers.iter().cloned());
        }

        /// A handle to the count of position reads.
        pub fn held_reads(&self) -> Arc<Mutex<u32>> {
            Arc::clone(&self.0.lock().unwrap().held_reads)
        }
    }

    impl IbkrTransport for IbkrTransportFake {
        fn server_link_broken(&self) -> bool {
            self.0.lock().unwrap().server_link_broken
        }
        fn held_position(&mut self, _contract: &IbkrContract) -> Result<f64, String> {
            let mut inner = self.0.lock().unwrap();
            *inner.held_reads.lock().unwrap() += 1;
            if let Some(answer) = inner.held_answers.pop_front() {
                return answer;
            }
            inner.held_position.clone().unwrap_or_else(|| Err("no position scripted".to_string()))
        }
        fn place_order(&mut self, order_id: i32, spec: &IbkrOrderSpec, contract: &IbkrContract) {
            let mut inner = self.0.lock().unwrap();
            inner.place_calls.lock().unwrap().push(order_id);
            inner.place_specs.lock().unwrap().push(spec.clone());
            inner.place_contracts.lock().unwrap().push(contract.clone());
            // Run the hook then enqueue its follow-ups, all under the one lock acquisition.
            let scripted = match inner.on_place_order.as_mut() {
                Some(hook) => hook(order_id),
                None => Vec::new(),
            };
            for s in scripted {
                inner.inbound.push_back(s.into_inbound());
            }
        }

        fn cancel_order(&mut self, order_id: i32) -> Result<(), String> {
            // Recorded, then a no-op: cancel lifecycle inbounds are scripted explicitly when needed.
            self.0.lock().unwrap().cancel_calls.lock().unwrap().push(order_id);
            Ok(())
        }

        fn modify_order(
            &mut self,
            order_id: i32,
            spec: &IbkrOrderSpec,
            _contract: &IbkrContract,
        ) -> AmendOutcome {
            let mut inner = self.0.lock().unwrap();
            // Record the resolved numeric order_id so a test can assert run_execution forwarded it.
            inner.modify_calls.lock().unwrap().push(order_id);
            inner.modify_specs.lock().unwrap().push(spec.clone());
            let scripted = match inner.on_modify_order.as_mut() {
                Some(hook) => hook(order_id),
                None => Vec::new(),
            };
            for s in scripted {
                inner.inbound.push_back(s.into_inbound());
            }
            inner.modify_outcome.clone().unwrap_or(AmendOutcome::Sent)
        }

        fn request_open_orders(&mut self) {
            let mut inner = self.0.lock().unwrap();
            *inner.open_order_requests.lock().unwrap() += 1;
            let scripted = match inner.on_request_open_orders.as_mut() {
                Some(hook) => hook(),
                None => Vec::new(),
            };
            // Front-insert (in script order), not back: a test may have already injected a
            // follow-up status update for the same order right after `StreamResync` (racing the
            // exec loop's drain of the queue), and the resync snapshot must land BEFORE it so the
            // map is rebuilt in time to resolve that status — matching how `StreamResync` is folded
            // synchronously into this very `request_open_orders` call on the exec loop's thread.
            // The end marker follows every row (rows are front-inserted in script order, the marker
            // after the last of them), the way the real transport sends it.
            let queued = inner.open_orders_ends.pop_front();
            let end = queued.or(inner.open_orders_end).unwrap_or(SnapshotMode::Complete);
            let rows = scripted.len();
            for (n, s) in scripted.into_iter().enumerate() {
                inner.inbound.insert(n, s.into_inbound());
            }
            if let Some(complete) = end_flag(end) {
                inner.inbound.insert(
                    rows,
                    IbkrInbound::SnapshotEnd { snapshot: SnapshotRequest::OpenOrders, complete },
                );
            }
        }

        fn request_executions(&mut self) {
            let mut inner = self.0.lock().unwrap();
            *inner.execution_requests.lock().unwrap() += 1;
            let scripted = match inner.on_request_executions.as_mut() {
                Some(hook) => hook(),
                None => Vec::new(),
            };
            // Front-insert in script order, for the same reason `request_open_orders` does: the
            // replay is IBKR answering a request the exec loop made synchronously on this thread,
            // so it must land ahead of anything a test injected afterwards. The end marker follows
            // the last row.
            let end = inner.executions_end.unwrap_or(SnapshotMode::Complete);
            let rows = scripted.len();
            for (n, s) in scripted.into_iter().enumerate() {
                inner.inbound.insert(n, s.into_inbound());
            }
            if let Some(complete) = end_flag(end) {
                inner.inbound.insert(
                    rows,
                    IbkrInbound::SnapshotEnd { snapshot: SnapshotRequest::Executions, complete },
                );
            }
        }

        fn ownership(&self) -> Option<OwnOrders> {
            self.0.lock().unwrap().own.clone()
        }

        fn next_recv(&mut self, timeout: Duration) -> Option<IbkrInbound> {
            if let Some(inbound) = self.0.lock().unwrap().inbound.pop_front() {
                return Some(inbound);
            }
            // Empty: sleep the (bounded) poll interval and report a timeout so the exec loop keeps
            // polling its command channel and can observe Shutdown.
            std::thread::sleep(timeout.min(Duration::from_millis(20)));
            None
        }
    }
}
