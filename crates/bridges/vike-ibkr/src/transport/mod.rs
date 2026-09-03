//! The transport seam: the ONE trait the (future) three backends implement. Phase 1 ships the
//! socket backend (Task 9) + a `FakeTransport` double so the whole exec lifecycle is testable
//! without a Gateway. `IbInbound` is the normalized event set the reader loop folds through the
//! `EventMapper`.
//!
//! The trait is deliberately tiny and blocking: `place_order`/`cancel_order`/`request_open_orders`
//! are fire-and-forget, and `next_recv` is a bounded blocking poll so the exec loop can interleave
//! transport inbound with the command channel (see `exec::run_exec`).

use std::time::Duration;

#[cfg(feature = "ibkr-socket")]
mod socket;
#[cfg(feature = "ibkr-socket")]
pub use socket::SocketTransport;

#[cfg(feature = "ibkr-cpapi")]
mod cpapi;
/// Re-exported pure decoder surface for the no-network fixture tests (`tests/ibkr_cpapi_decode.rs`).
#[cfg(feature = "ibkr-cpapi")]
pub use cpapi::decode as cpapi_decode;
/// Re-exported so the live smoke can do a read-only session-account guard before placing an order
/// (`tests/ibkr_cpapi_smoke.rs`): cpapi routes orders by the gateway's authenticated SESSION, not
/// `cfg.account`, so the `.env`-string `DU` guard alone cannot prove the session is paper.
#[cfg(feature = "ibkr-cpapi")]
pub use cpapi::CpapiRest;
#[cfg(feature = "ibkr-cpapi")]
pub use cpapi::CpapiTransport;

use crate::contract::IbkrContract;
use crate::event_mapper::{IbCommissionReport, IbExecDetails, IbOrderStatus};
use crate::order::IbOrderSpec;

/// Normalized inbound from any backend — the reader loop folds each through the `EventMapper`.
pub enum IbInbound {
    NextValidId(i32),
    AccountsReady,
    OrderStatus(IbOrderStatus),
    ExecDetails(IbExecDetails),
    Commission(IbCommissionReport),
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
    /// one, which has no reconnect at all — see [`IbInbound::StreamDead`] for what that cost.
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
    /// `request_open_orders` — the reconnect-resync payload the mapper rebuilds its coid⇄orderId
    /// map from (Task 10; `order_ref` IS the coid).
    OpenOrder {
        order_id: i32,
        order_ref: String,
    },
}

/// The ONE seam every IBKR backend (socket now; cpapi/oauth later) implements. `Send` so the exec
/// loop can own it on the `ExecActor` thread.
pub trait IbkrTransport: Send {
    /// Place `spec`/`contract` under the numeric IB `order_id` (already allocated by the caller).
    fn place_order(&mut self, order_id: i32, spec: &IbOrderSpec, contract: &IbkrContract);
    /// Cancel by numeric IB `order_id`.
    fn cancel_order(&mut self, order_id: i32);
    /// Reprice/resize a resting order in place. Default no-op: the socket backend has no native
    /// amend (declared `supports_modify=false`), so it inherits this and the order keeps its terms.
    /// The cpapi backend overrides it with a real amend endpoint.
    fn modify_order(&mut self, order_id: i32, spec: &IbOrderSpec, contract: &IbkrContract) {
        let _ = (order_id, spec, contract);
    }
    /// Re-request all open orders (reconnect resync, Task 10). Executions are the SIBLING request
    /// below — this doc line used to also promise "+ recent executions", which no backend ever did.
    fn request_open_orders(&mut self);
    /// Ask the venue to re-deliver the CURRENT DAY's executions **and their commission reports**,
    /// so a fill whose commissionReport was lost can still be emitted with the REAL commission
    /// (`event_mapper`'s module doc trap 5). Driven from two places in `exec::run_exec`: the
    /// reconnect resync, and the stranded-fill sweep.
    ///
    /// Fire-and-forget like its siblings — the replayed rows arrive through the ordinary
    /// `next_recv` inbound stream as `ExecDetails` + `Commission`, not as a return value. Replaying
    /// already-emitted pairs is safe: `EventMapper`'s `emitted` index drops both halves.
    ///
    /// **Default no-op.** A backend that cannot replay executions inherits it, and its stranded
    /// fills escalate to the operator instead of silently recovering. The cpapi backend inherits it
    /// deliberately, for a stronger reason: it decodes ONE `sor` execution row into BOTH halves at
    /// once (`transport/cpapi/decode.rs`), so a cpapi fill structurally cannot strand.
    fn request_executions(&mut self) {}
    /// Block up to `timeout` for the next inbound; `None` on timeout (lets the loop poll the cmd
    /// channel and observe `Shutdown`).
    fn next_recv(&mut self, timeout: Duration) -> Option<IbInbound>;
}

// ---------------------------------------------------------------------------------------------
// FakeTransport — the no-Gateway test double. Behind `test`/`test-support` so it never ships in a
// default build. It carries a scripted inbound queue (`script`) plus an `on_place_order` hook that
// enqueues follow-up inbounds parameterized by the numeric order_id the exec loop allocated, so a
// full submit→accept→fill lifecycle folds through the real EventMapper with no socket.
// ---------------------------------------------------------------------------------------------

#[cfg(any(test, feature = "test-support"))]
pub use fake::{FakeTransport, ScriptedInbound};

#[cfg(any(test, feature = "test-support"))]
mod fake {
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use crate::contract::IbkrContract;
    use crate::event_mapper::{IbCommissionReport, IbExecDetails, IbOrderStatus};
    use crate::order::IbOrderSpec;

    use super::{IbInbound, IbkrTransport};

    /// A scripted inbound with the minimal fields a lifecycle test cares about; the rest are filled
    /// with sensible defaults when lowered into a full [`IbInbound`]. Keeping this reduced set
    /// separate from `IbInbound` keeps test scripts terse.
    pub enum ScriptedInbound {
        NextValidId(i32),
        AccountsReady,
        OrderStatus {
            order_id: i32,
            order_ref: String,
            status: String,
        },
        ExecDetails {
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
        /// The terminal-death notice (`IbInbound::StreamDead`) — what the socket backend's pump
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
    }

    impl ScriptedInbound {
        /// Lower a terse script entry into the full normalized inbound the reader loop folds. Fills
        /// the fields the exec lifecycle doesn't script (symbol/side/ts/currency) with defaults: a
        /// BUY in USD at ts 0, symbol left empty (the EventMapper carries it onto the fill, which
        /// the lifecycle assertion doesn't inspect).
        fn into_inbound(self) -> IbInbound {
            match self {
                ScriptedInbound::NextValidId(id) => IbInbound::NextValidId(id),
                ScriptedInbound::AccountsReady => IbInbound::AccountsReady,
                ScriptedInbound::OrderStatus { order_id, order_ref, status } => {
                    IbInbound::OrderStatus(IbOrderStatus {
                        order_id,
                        order_ref,
                        status,
                        filled: 0.0,
                        avg_fill_price: 0.0,
                    })
                }
                ScriptedInbound::ExecDetails { order_id, exec_id, shares, price } => {
                    IbInbound::ExecDetails(IbExecDetails {
                        order_id,
                        order_ref: String::new(),
                        exec_id,
                        symbol: String::new(),
                        side_buy: true,
                        shares,
                        price,
                        ts: 0,
                    })
                }
                ScriptedInbound::Commission { exec_id, commission } => {
                    IbInbound::Commission(IbCommissionReport {
                        exec_id,
                        commission,
                        currency: "USD".to_string(),
                    })
                }
                ScriptedInbound::Error { code, order_id, msg } => {
                    IbInbound::Error { code, order_id, msg }
                }
                ScriptedInbound::StreamResync => IbInbound::StreamResync,
                ScriptedInbound::StreamDead { reason } => IbInbound::StreamDead { reason },
                ScriptedInbound::OpenOrder { order_id, order_ref } => {
                    IbInbound::OpenOrder { order_id, order_ref }
                }
            }
        }
    }

    type PlaceHook = Box<dyn FnMut(i32) -> Vec<ScriptedInbound> + Send>;
    type OpenOrdersHook = Box<dyn FnMut() -> Vec<ScriptedInbound> + Send>;
    type ExecutionsHook = Box<dyn FnMut() -> Vec<ScriptedInbound> + Send>;

    /// The shared, lockable state behind a [`FakeTransport`] handle — see there for why it's Arc'd.
    #[derive(Default)]
    struct Inner {
        inbound: VecDeque<IbInbound>,
        on_place_order: Option<PlaceHook>,
        on_request_open_orders: Option<OpenOrdersHook>,
        on_request_executions: Option<ExecutionsHook>,
        /// Records the numeric `order_id` of each `modify_order` call (Task 3 wiring assertion).
        modify_calls: Arc<Mutex<Vec<i32>>>,
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
    }

    /// Scriptable [`IbkrTransport`] double — no Gateway, no ibapi. Push initial inbounds with
    /// [`FakeTransport::script`]; register [`FakeTransport::on_place_order`] to enqueue follow-ups
    /// parameterized by the numeric order_id the exec loop allocates on submit, and
    /// [`FakeTransport::on_request_open_orders`] for the reconnect-resync snapshot (Task 10).
    ///
    /// `Clone`-able and backed by `Arc<Mutex<Inner>>`: the exec loop owns one clone on its own
    /// thread (moved into `run_exec_for_test`'s `Box<dyn IbkrTransport>`) while the test keeps a
    /// second clone to [`FakeTransport::inject`] inbounds asynchronously (e.g. a `StreamResync`
    /// mid-lifecycle) — both clones read/write the SAME queue.
    #[derive(Clone, Default)]
    pub struct FakeTransport(Arc<Mutex<Inner>>);

    impl FakeTransport {
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
        /// asynchronously once `run_exec_for_test` has already moved a clone onto its thread.
        pub fn inject(&self, inbound: ScriptedInbound) {
            self.0.lock().unwrap().inbound.push_back(inbound.into_inbound());
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

        /// A handle to the recorded `place_order` order ids — a clone of the shared vec the
        /// transport pushes into. EMPTY is the assertion that matters for the stream-death refusal.
        pub fn place_calls(&self) -> Arc<Mutex<Vec<i32>>> {
            Arc::clone(&self.0.lock().unwrap().place_calls)
        }

        /// A handle to the recorded `cancel_order` order ids — see [`Inner::cancel_calls`] for why
        /// the stream-death test asserts on this rather than on the absence of an event.
        pub fn cancel_calls(&self) -> Arc<Mutex<Vec<i32>>> {
            Arc::clone(&self.0.lock().unwrap().cancel_calls)
        }
    }

    impl IbkrTransport for FakeTransport {
        fn place_order(&mut self, order_id: i32, _spec: &IbOrderSpec, _contract: &IbkrContract) {
            let mut inner = self.0.lock().unwrap();
            inner.place_calls.lock().unwrap().push(order_id);
            // Run the hook then enqueue its follow-ups, all under the one lock acquisition.
            let scripted = match inner.on_place_order.as_mut() {
                Some(hook) => hook(order_id),
                None => Vec::new(),
            };
            for s in scripted {
                inner.inbound.push_back(s.into_inbound());
            }
        }

        fn cancel_order(&mut self, order_id: i32) {
            // Recorded, then a no-op: cancel lifecycle inbounds are scripted explicitly when needed.
            self.0.lock().unwrap().cancel_calls.lock().unwrap().push(order_id);
        }

        fn modify_order(&mut self, order_id: i32, _spec: &IbOrderSpec, _contract: &IbkrContract) {
            // Record the resolved numeric order_id so a test can assert run_exec forwarded it.
            self.0.lock().unwrap().modify_calls.lock().unwrap().push(order_id);
        }

        fn request_open_orders(&mut self) {
            let mut inner = self.0.lock().unwrap();
            let scripted = match inner.on_request_open_orders.as_mut() {
                Some(hook) => hook(),
                None => Vec::new(),
            };
            // Front-insert (in script order), not back: a test may have already injected a
            // follow-up status update for the same order right after `StreamResync` (racing the
            // exec loop's drain of the queue), and the resync snapshot must land BEFORE it so the
            // map is rebuilt in time to resolve that status — matching how `StreamResync` is folded
            // synchronously into this very `request_open_orders` call on the exec loop's thread.
            for s in scripted.into_iter().rev() {
                inner.inbound.push_front(s.into_inbound());
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
            // so it must land ahead of anything a test injected afterwards.
            for s in scripted.into_iter().rev() {
                inner.inbound.push_front(s.into_inbound());
            }
        }

        fn next_recv(&mut self, timeout: Duration) -> Option<IbInbound> {
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
