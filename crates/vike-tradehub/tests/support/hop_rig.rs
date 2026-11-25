//! **The hop rig: a REAL gatewayhub server, a link proxy a test can cut, a scripted venue that outlives the
//! daemon, and a real core on the other end** (wave 2, W19).
//!
//! Everything here is test support for the twin and the kill proofs (`gateway_twin.rs`,
//! `hop_kill_proofs.rs`, `no_order_vanishes.rs`, `any_symbol_twin.rs`, `venue_backend_parity.rs`), the
//! members of the ONE `hop` binary: `tests/hop.rs` `#[path]`-includes this file once, so it compiles once,
//! and each member reaches it as `crate::hop_rig`. A member uses a different part, so that `mod`
//! declaration carries the one `#[allow(dead_code, clippy::allow_attributes)]`.
//!
//! * [`World`]: the VENUE. Its state (what rests, what was submitted, how each order ended) belongs to the
//!   test, not to the daemon, so it survives a gatewayhub restart the way an exchange does. One command
//!   loop, [`World::handle`], is the venue's whole behaviour; the in-process twin drives it under an
//!   `ExecutionClientActor` straight from the core, the hop drives the very same loop under the very same
//!   `ExecutionClientActor` inside the gatewayhub's lane ([`ActorLane`]).
//! * [`GatewayServer`]: `vike_gatewayhub::server::accept::serve_with_trade` over any
//!   `vike_gatewayhub::trade::lane::TradeLane`, on an ephemeral loopback port, stopped the way the daemon
//!   stops (the stop flag, a drain, a join). Starting a second one is a gatewayhub RESTART: a new hub, new
//!   sessions, new `gateway_epoch`s.
//! * [`LinkProxy`]: a TCP relay between the client and the server a test can CUT (every connection dies and
//!   new ones are refused), re-aim at a restarted server, or make swallow one direction (the frames the
//!   daemon never read; the acks the client never got).
//! * [`Hop`]: the three together, and the link a client dials.
//! * [`CoreRig`]: a real `vike_core` core over one engine, fed by the `live-event-forward` relay the
//!   tradehub's composition runs (`vike_tradehub_core`'s `forward_ingest`, spelled the same here), with a
//!   journal a test can read back.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io::{Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use vike_bridge_core::exec_actor::{ExecutionClientActor, ExecutionClientActorCommand};
use vike_core::{CoreConfig, CoreHandle, spawn_core};
use vike_execution::recon::ReconcileClient;
use vike_execution::{
    Account, BalanceMode, Command, Engine, EventSender, ExecutionClient, Ingest, OrderIntent,
    RiskGate,
};
use vike_gatewayhub::budget::ledger::Budget;
use vike_gatewayhub::server::accept::serve_with_trade;
use vike_gatewayhub::server::bind::bind_loopback;
use vike_gatewayhub::server::lanes::ServerLanes;
use vike_gatewayhub::server::limits::ServerConfig;
use vike_gatewayhub::server::trade::TradeSetup;
use vike_gatewayhub::testkit::data::DataLaneScript;
use vike_gatewayhub::testkit::gateway::{ServedThread, TradeKit, fast_timings, trade_keys};
use vike_gatewayhub::testkit::rest::RestLaneScript;
use vike_gatewayhub::testkit::trade::TradeLaneScript;
use vike_gatewayhub::testkit::trade_rig::{fast_trade_timings, quick_config};
use vike_gatewayhub::trade::adapters::symbol_facts::{InstrumentList, SymbolPropertiesCache};
use vike_gatewayhub::trade::lane::{AdapterRefusal, StartedAdapter, TradeLane, TradeLaneTarget};
use vike_gatewayhub_client::client::GatewayLink;
use vike_gatewayhub_client::proto;
use vike_gatewayhub_client::proto::{SymbolPropertiesRefusal, SymbolPropertiesWire};
use vike_log::poll::wait_until;
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::events::{
    Event, FillEvent, FundingEvent, OrderAccepted, OrderCancelRejected, OrderCanceled, OrderFilled,
    OrderModified, OrderModifyRejected, OrderPartiallyFilled, OrderRejected, OrderSubmitted,
    PositionSide, TradeId,
};
use vike_model::{AccountTier, OrderRequest, OrderStatus, OrderType, RiskLimits};
use vike_model::{ArmingBlock, OrderCancelIntent};
use vike_utility::mutex::lock;

/// The one venue every rig trades on.
pub const VENUE: &str = "binance";
/// The one symbol.
pub const SYMBOL: &str = "BTCUSDT";

/// **Hold the kept instrument lists this binary's doors read, once per process** (decision 0290: the door reads a
/// typed symbol's user name, and an ibkr mount's money, off the venue's KEPT row). The lib's own fixture
/// (`crate::testutil` of the lib) is compiled into its unit tests only, so this binary holds its own with
/// `vike_catalog::hold_kept_list`, each row keyed as the venue's list keys it:
///
/// * binance: the spot pairs `BTCUSDT` (the rig's [`SYMBOL`]), `ETHUSDT`, `SOLUSDT`, `NOTICKUSDT` (the pair a test
///   venue lists with no tick) and `XRPUSDT` (kept here, listed by no test venue: a symbol the venue does not
///   list), and the perpetual `ETHUSDT.P`;
/// * ibkr (the operator's rows): `AAPL` in USD and `SAP` in EUR.
///
/// [`Hop::start_with_catalog`] calls it, and so does every helper that crosses the door or starts an adapter
/// without a hop, so every test sees the same rows whatever order the tests run in.
pub fn hold_kept_lists() {
    use vike_model::{Contract, ContractEquity, ContractPerp, Instrument, Margining};
    static HELD: std::sync::Once = std::sync::Once::new();
    HELD.call_once(|| {
        let spot = |base: &str| {
            Instrument::new(VENUE, format!("{base}USDT"), base, "USDT", Contract::CryptoSpot)
        };
        let perp = Contract::Perp(ContractPerp {
            margining: Margining::Linear,
            settle: "USDT".into(),
            value_currency: None,
            funding_interval_ms: None,
            settling_ms: None,
        });
        let mut binance: Vec<Instrument> =
            ["BTC", "ETH", "SOL", "NOTICK", "XRP"].into_iter().map(spot).collect();
        binance.push(Instrument::new(VENUE, "ETHUSDT.P", "ETH", "USDT", perp));
        vike_catalog::hold_kept_list(VENUE, binance);
        let stock = |raw: &str, ticker: &str, currency: &str| {
            Instrument::new(
                "ibkr",
                raw,
                ticker,
                currency,
                Contract::Equity(ContractEquity::default()),
            )
        };
        vike_catalog::hold_kept_list(
            "ibkr",
            vec![stock("AAPL.SMART.USD", "AAPL", "USD"), stock("SAP.SMART.EUR", "SAP", "EUR")],
        );
    });
}

/// A limit buy of 1.0 on the rig's venue.
pub fn order(order_client_id: &str, px: f64) -> OrderRequest {
    OrderRequest {
        order_client_id: order_client_id.to_string(),
        venue: VENUE.to_string(),
        symbol: SYMBOL.to_string(),
        side: 1,
        qty: 1.0,
        order_type: OrderType::Limit,
        price: Some(px),
        timestamp: 1_700_000_000_000,
        ..Default::default()
    }
}

// ---- the venue ----------------------------------------------------------------------------------------

/// How the scripted venue answers a submit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fate {
    /// Accepted and left resting (the default).
    Rest,
    /// Refused with a venue reject.
    Reject,
    /// Accepted and filled at once, in full.
    Fill,
    /// Accepted, filled 40 percent, then the rest.
    PartialThenFill,
}

#[derive(Default)]
struct WorldState {
    resting: BTreeMap<String, OrderRequest>,
    /// Every order client id a Submit reached the venue for, in order.
    seen: Vec<String>,
    /// Every order a Submit reached the venue with, whole (the symbol and the snapped price and size
    /// the venue was actually handed), in order.
    submitted: Vec<OrderRequest>,
    /// How each finished order ended (the authoritative event `confirm` re-states).
    ended: HashMap<String, Event>,
    /// How many times a lookup (`confirm`) re-stated the terminal of each order client id.
    restated: HashMap<String, usize>,
    next_trade: u64,
    fates: HashMap<String, Fate>,
    /// A cancel of an order that is not resting is answered as a bridge whose REST cancel swallows
    /// "too late / already filled / not found" does (bybit 110001 and 170213, okx 51400-51402): the
    /// SYNTHETIC `OrderCanceled { ts: 0 }` of `vike_bridge_core::exec_actor::cancel_event`, not a refusal.
    swallow_cancels_of_gone_orders: bool,
    /// A lookup of a RESTING order answers an optimistic accept, which names no venue order (what IG's
    /// `unresolved_confirm` states when the venue could not be asked), instead of the venue's own.
    bare_lookups: bool,
    /// The cancel of a RESTING order is answered the way a REST venue with a private stream answers it: the
    /// loop's SYNTHETIC `OrderCanceled { ts: 0 }` the moment the REST call returns, then the venue's own
    /// stamped row.
    synthetic_then_stamped_cancels: bool,
    /// The commission every fill the venue states carries (so a balance moves when a fill is booked).
    commission: f64,
    /// How long the venue's thread takes to get to a submit (see [`World::stall_submits`]).
    submit_stall: Duration,
}

/// The scripted venue. See the module doc.
#[derive(Default)]
pub struct World {
    st: Mutex<WorldState>,
}

impl World {
    /// A venue that rests everything.
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Script how `order_client_id` is answered (default [`Fate::Rest`]).
    pub fn fate(&self, order_client_id: &str, fate: Fate) {
        lock(&self.st).fates.insert(order_client_id.to_string(), fate);
    }

    /// Answer a cancel of an order that is gone (filled, never seen) with the bridge's synthetic
    /// `OrderCanceled { ts: 0 }` instead of a refusal (see `WorldState::swallow_cancels_of_gone_orders`).
    pub fn swallow_cancels_of_gone_orders(&self, on: bool) {
        lock(&self.st).swallow_cancels_of_gone_orders = on;
    }

    /// A lookup of a resting order answers a bare accept (see `WorldState::bare_lookups`).
    pub fn bare_lookups(&self, on: bool) {
        lock(&self.st).bare_lookups = on;
    }

    /// A cancel of a resting order is answered synthetic-then-stamped (see
    /// `WorldState::synthetic_then_stamped_cancels`).
    pub fn synthetic_then_stamped_cancels(&self, on: bool) {
        lock(&self.st).synthetic_then_stamped_cancels = on;
    }

    /// Every fill the venue states from now on carries this commission.
    pub fn set_commission(&self, commission: f64) {
        lock(&self.st).commission = commission;
    }

    /// Every submit reaches the venue `stall` after the daemon's threads hand it over: a thread starved by a
    /// loaded box, anywhere between the daemon's acknowledgement and the venue, made repeatable.
    pub fn stall_submits(&self, stall: Duration) {
        lock(&self.st).submit_stall = stall;
    }

    /// Every order client id a Submit reached the venue for, in arrival order.
    pub fn seen_submits(&self) -> Vec<String> {
        lock(&self.st).seen.clone()
    }

    /// Every order a Submit reached the venue with, whole, in arrival order.
    pub fn submitted_orders(&self) -> Vec<OrderRequest> {
        lock(&self.st).submitted.clone()
    }

    /// The order client ids resting now.
    pub fn resting(&self) -> BTreeSet<String> {
        lock(&self.st).resting.keys().cloned().collect()
    }

    /// How many times a lookup re-stated the terminal of `order_client_id` (see [`World::confirm`]).
    pub fn restatements(&self, order_client_id: &str) -> usize {
        lock(&self.st).restated.get(order_client_id).copied().unwrap_or(0)
    }

    /// The venue's authoritative answer for `order_client_id` (what a lookup by order client id returns): the
    /// terminal event it ended with, or an `OrderAccepted` while it rests, or nothing when the venue never
    /// saw it (never a false terminal).
    ///
    /// **A lookup RE-STATES a terminal the venue may already have sent live** (the daemon asks about an
    /// UNKNOWN submit whose cancel or fill is in flight, or whose live event died with the old daemon's
    /// ring): the copy it returns carries [`RESTATED_TIMESTAMP`] where the live event carried the order's own
    /// timestamp, as a real venue's status lookup carries its own update time, and is what
    /// [`is_restated`] recognises. The core folds a re-stated terminal as a no-op (the order is already
    /// ended); a test that counts "terminals per order" on a lane must not count it as a second ending.
    pub fn confirm(&self, order_client_id: &str) -> Vec<Event> {
        let mut st = lock(&self.st);
        if let Some(ended) = st.ended.get(order_client_id) {
            let mut again = ended.clone();
            match &mut again {
                Event::OrderCanceled(e) => e.timestamp = RESTATED_TIMESTAMP,
                Event::OrderFilled(e) => e.timestamp = RESTATED_TIMESTAMP,
                Event::OrderRejected(e) => e.timestamp = RESTATED_TIMESTAMP,
                _ => {}
            }
            *st.restated.entry(order_client_id.to_string()).or_default() += 1;
            return vec![again];
        }
        if st.resting.contains_key(order_client_id) {
            if st.bare_lookups {
                return vec![Event::OrderAccepted(OrderAccepted {
                    order_client_id: order_client_id.to_string(),
                    venue_order_id: None,
                    timestamp: 1_700_000_000_000,
                })];
            }
            return vec![accepted(order_client_id)];
        }
        Vec::new()
    }

    fn fill_event(st: &mut WorldState, o: &OrderRequest, qty: f64) -> FillEvent {
        st.next_trade += 1;
        FillEvent {
            trade_id: TradeId::prefixed("wt", st.next_trade),
            order_client_id: o.order_client_id.clone(),
            venue: VENUE.into(),
            // The symbol the ORDER named, as the per-order bridges state it (every rig order but the
            // any-symbol twin's names `SYMBOL`, so nothing else changes).
            symbol: o.symbol.as_str().into(),
            side: o.side,
            last_qty: qty,
            last_price: o.price.unwrap_or(100.0),
            commission: st.commission,
            commission_asset: String::new().into(),
            liquidity_side: "taker".to_string().into(),
            timestamp: o.timestamp,
            mark_price: o.price,
            position_side: PositionSide::Both,
        }
    }

    /// The venue's whole behaviour for one command. `false` = the loop ends (a `Shutdown`).
    pub fn handle(&self, cmd: ExecutionClientActorCommand, events: &EventSender) -> bool {
        match cmd {
            ExecutionClientActorCommand::Submit(o, _) => {
                let stall = lock(&self.st).submit_stall;
                std::thread::sleep(stall);
                let mut out = vec![Event::OrderSubmitted(OrderSubmitted {
                    order_client_id: o.order_client_id.clone(),
                    timestamp: o.timestamp,
                })];
                {
                    let mut st = lock(&self.st);
                    st.seen.push(o.order_client_id.clone());
                    st.submitted.push((*o).clone());
                    let fate = st.fates.get(&o.order_client_id).copied().unwrap_or(Fate::Rest);
                    if fate == Fate::Reject {
                        let reject = Event::OrderRejected(OrderRejected {
                            cause: None,
                            order_client_id: o.order_client_id.clone(),
                            reason: "scripted venue: insufficient margin".to_string().into(),
                            timestamp: o.timestamp,
                            refusal_id: None,
                        });
                        st.ended.insert(o.order_client_id.clone(), reject.clone());
                        out.push(reject);
                    } else {
                        out.push(Event::OrderAccepted(OrderAccepted {
                            order_client_id: o.order_client_id.clone(),
                            venue_order_id: Some(format!("v-{}", o.order_client_id).into()),
                            timestamp: o.timestamp,
                        }));
                        match fate {
                            Fate::Fill => {
                                let fill = Self::fill_event(&mut st, &o, o.qty);
                                let done = Event::OrderFilled(OrderFilled {
                                    order_client_id: o.order_client_id.clone(),
                                    fill: fill.clone(),
                                    timestamp: o.timestamp,
                                });
                                st.ended.insert(o.order_client_id.clone(), done.clone());
                                out.push(Event::Fill(fill));
                                out.push(done);
                            }
                            Fate::PartialThenFill => {
                                let part = Self::fill_event(&mut st, &o, o.qty * 0.4);
                                out.push(Event::Fill(part.clone()));
                                out.push(Event::OrderPartiallyFilled(OrderPartiallyFilled {
                                    order_client_id: o.order_client_id.clone(),
                                    fill: part,
                                    timestamp: o.timestamp,
                                }));
                                let rest = Self::fill_event(&mut st, &o, o.qty * 0.6);
                                let done = Event::OrderFilled(OrderFilled {
                                    order_client_id: o.order_client_id.clone(),
                                    fill: rest.clone(),
                                    timestamp: o.timestamp,
                                });
                                st.ended.insert(o.order_client_id.clone(), done.clone());
                                out.push(Event::Fill(rest));
                                out.push(done);
                                // The venue pays funding on the position this order opened. A funding
                                // payment has no id and no dedup (`Account::apply_funding` is a bare
                                // `+=`), so it is the event whose double delivery would show in a balance.
                                out.push(Event::Funding(FundingEvent {
                                    venue: VENUE.into(),
                                    symbol: SYMBOL.into(),
                                    position_side: PositionSide::Both,
                                    funding_rate: 0.0001,
                                    amount: 0.5,
                                    mark_price: None,
                                    timestamp: o.timestamp,
                                    route_key: None,
                                    currency: None,
                                }));
                            }
                            Fate::Rest | Fate::Reject => {
                                st.resting.insert(o.order_client_id.clone(), (*o).clone());
                            }
                        }
                    }
                }
                for e in out {
                    let _ = events.blocking_send(e);
                }
                true
            }
            ExecutionClientActorCommand::Cancel { order_client_id, .. } => {
                self.cancel_one(&order_client_id, events);
                true
            }
            ExecutionClientActorCommand::CancelBatch { order_client_ids, .. } => {
                for c in order_client_ids {
                    self.cancel_one(&c, events);
                }
                true
            }
            // This rig declares neither lane, as no venue it stands for does.
            ExecutionClientActorCommand::SubmitBatch(requests, _) => {
                vike_bridge_core::exec_actor::submit_batch_undeclared(events, &requests);
                true
            }
            ExecutionClientActorCommand::Adopt(_) => true,
            ExecutionClientActorCommand::Modify { order, new_qty, new_price, .. } => {
                let event = {
                    let st = lock(&self.st);
                    if st.resting.contains_key(&order.order_client_id) {
                        Event::OrderModified(OrderModified {
                            order_client_id: order.order_client_id.clone(),
                            venue_order_id: Some(format!("v-{}", order.order_client_id).into()),
                            new_qty,
                            new_price,
                            new_trigger_price: None,
                            timestamp: order.timestamp,
                        })
                    } else {
                        Event::OrderModifyRejected(OrderModifyRejected {
                            order_client_id: order.order_client_id.clone(),
                            reason: "scripted venue: unknown order".to_string().into(),
                            timestamp: order.timestamp,
                            refusal_id: None,
                        })
                    }
                };
                let _ = events.blocking_send(event);
                true
            }
            ExecutionClientActorCommand::Shutdown => false,
        }
    }

    /// The venue executes a cancel of `order_client_id` and NOBODY hears the answer: the daemon that sent it died first
    /// (the ending event goes to a channel nobody reads).
    pub fn cancel_unheard(&self, order_client_id: &str) {
        let (tx, rx) = vike_execution::event_channel(4);
        drop(rx);
        self.cancel_one(order_client_id, &tx);
    }

    fn cancel_one(&self, order_client_id: &str, events: &EventSender) {
        let mut lead = None;
        let event = {
            let mut st = lock(&self.st);
            if st.resting.remove(order_client_id).is_some() {
                if st.synthetic_then_stamped_cancels {
                    lead = Some(Event::OrderCanceled(OrderCanceled {
                        order_client_id: order_client_id.to_string(),
                        reason: String::new().into(),
                        timestamp: 0,
                        post_only_would_cross: false,
                        refusal_id: None,
                    }));
                }
                let done = Event::OrderCanceled(OrderCanceled {
                    order_client_id: order_client_id.to_string(),
                    reason: "user".to_string().into(),
                    timestamp: 1_700_000_000_001,
                    post_only_would_cross: false,
                    refusal_id: None,
                });
                st.ended.insert(order_client_id.to_string(), done.clone());
                done
            } else if st.swallow_cancels_of_gone_orders {
                Event::OrderCanceled(OrderCanceled {
                    order_client_id: order_client_id.to_string(),
                    reason: String::new().into(),
                    timestamp: 0,
                    post_only_would_cross: false,
                    refusal_id: None,
                })
            } else {
                Event::OrderCancelRejected(OrderCancelRejected {
                    order_client_id: order_client_id.to_string(),
                    reason: "scripted venue: unknown order".to_string().into(),
                    timestamp: 0,
                    refusal_id: None,
                })
            }
        };
        if let Some(lead) = lead {
            let _ = events.blocking_send(lead);
        }
        let _ = events.blocking_send(event);
    }

    /// The venue's exec client for `events`: the REAL shared `ExecutionClientActor` running [`World::handle`] on its
    /// command thread, with the venue's lookup by order client id wired as its `confirm`.
    pub fn actor(self: &Arc<Self>, events: EventSender) -> ExecutionClientActor {
        let (w, event) = (Arc::clone(self), events.clone());
        let confirm_world = Arc::clone(self);
        ExecutionClientActor::spawn(
            "scripted-venue",
            events,
            move |rx: &Receiver<ExecutionClientActorCommand>| {
                while let Ok(cmd) = rx.recv() {
                    if !w.handle(cmd, &event) {
                        break;
                    }
                }
            },
        )
        .with_confirm(Arc::new(move |order_client_id: &str| confirm_world.confirm(order_client_id)))
    }
}

fn accepted(order_client_id: &str) -> Event {
    Event::OrderAccepted(OrderAccepted {
        order_client_id: order_client_id.to_string(),
        venue_order_id: Some(format!("v-{order_client_id}").into()),
        timestamp: 1_700_000_000_000,
    })
}

// ---- the daemon's lane ---------------------------------------------------------------------------------

/// One call the daemon made of the exec client it started.
#[derive(Debug, Clone, PartialEq)]
pub enum Call {
    /// `submit`.
    Submit(String),
    /// `cancel_with_intent`.
    Cancel(String, OrderCancelIntent),
    /// `modify`.
    Modify(String),
    /// `confirm`.
    Confirm(String),
    /// `adopt_restored`.
    Adopt(Vec<vike_execution::RestoredOrder>),
    /// `detach`.
    Detach,
}

/// The exec client the lane hands the daemon: the venue's `ExecutionClientActor` behind a recorder, and a poison list
/// (a bridge that PANICS inside `submit` for an order client id on it).
struct GuardedActor {
    inner: ExecutionClientActor,
    log: Arc<Mutex<Vec<Call>>>,
    panic_on: Arc<Mutex<HashSet<String>>>,
}

impl ExecutionClient for GuardedActor {
    fn submit(&mut self, request: &OrderRequest) {
        lock(&self.log).push(Call::Submit(request.order_client_id.clone()));
        assert!(
            !lock(&self.panic_on).contains(&request.order_client_id),
            "scripted bridge panic while submitting {}",
            request.order_client_id
        );
        self.inner.submit(request);
    }
    fn cancel(&mut self, order_client_id: &str) {
        self.cancel_with_intent(order_client_id, OrderCancelIntent::Unspecified);
    }
    fn cancel_with_intent(&mut self, order_client_id: &str, intent: OrderCancelIntent) {
        lock(&self.log).push(Call::Cancel(order_client_id.to_string(), intent));
        self.inner.cancel_with_intent(order_client_id, intent);
    }
    fn cancel_batch_with_intent(&mut self, order_client_ids: &[String], intent: OrderCancelIntent) {
        for c in order_client_ids {
            self.cancel_with_intent(c, intent);
        }
    }
    fn modify(&mut self, order: &OrderRequest, new_qty: Option<f64>, new_price: Option<f64>) {
        lock(&self.log).push(Call::Modify(order.order_client_id.clone()));
        self.inner.modify(order, new_qty, new_price);
    }
    fn confirm(&mut self, order_client_id: &str) {
        lock(&self.log).push(Call::Confirm(order_client_id.to_string()));
        self.inner.confirm(order_client_id);
    }
    fn begin_detach(&mut self) {
        self.inner.begin_detach();
    }
    fn detach(&mut self) {
        lock(&self.log).push(Call::Detach);
        self.inner.detach();
    }
    fn wire_id_for(&self, order_client_id: &str) -> Option<String> {
        self.inner.wire_id_for(order_client_id)
    }
    fn adopt_restored(&mut self, orders: &[vike_execution::RestoredOrder]) {
        lock(&self.log).push(Call::Adopt(orders.to_vec()));
        self.inner.adopt_restored(orders);
    }
}

/// A `TradeLane` whose exec client is the venue's real `ExecutionClientActor` ([`World::actor`]) and whose everything
/// else (the arming table, the tier, the preflight, the reconcile client) is `TradeLaneScript`'s. What a
/// bridge is to the daemon, minus the network.
pub struct ActorLane {
    /// The scripted lane underneath: its [`TradeLaneScript::calls`] are the daemon's `TradeCall` record, its
    /// [`TradeLaneScript::adapter`] fires the reconcile trigger.
    pub inner: TradeLaneScript,
    /// The venue.
    pub world: Arc<World>,
    /// Every call the daemon made of an exec client this lane started, in order, over every adapter.
    pub log: Arc<Mutex<Vec<Call>>>,
    /// Order client ids a `submit` PANICS for.
    pub panic_on: Arc<Mutex<HashSet<String>>>,
    /// The gatewayhub's REAL `SymbolPropertiesCache` over a fake instrument list (`None`: the scripted lane's default
    /// answer, a refusal). It keeps no listing (a zero keep time), so a changed catalog is seen at once.
    facts: Option<SymbolPropertiesCache>,
    /// Every `(venue, symbol)` the lease asked `symbol_facts` about, in order.
    facts_asked: Mutex<Vec<(String, String)>>,
}

impl ActorLane {
    /// A lane serving [`VENUE`] with `arming` as its table.
    pub fn new(world: Arc<World>, arming: proto::ArmingTableWire) -> Arc<Self> {
        Self::with_catalog(world, arming, None)
    }

    /// [`ActorLane::new`] answering `symbol_facts` through the gatewayhub's real `SymbolPropertiesCache` over `list`
    /// (the venue's instrument catalog, faked).
    pub fn with_catalog(
        world: Arc<World>,
        arming: proto::ArmingTableWire,
        list: Option<InstrumentList>,
    ) -> Arc<Self> {
        Arc::new(Self {
            inner: TradeLaneScript::new(&[VENUE]).with_arming(arming).with_resolver(VENUE),
            world,
            log: Arc::default(),
            panic_on: Arc::default(),
            facts: list.map(|list| SymbolPropertiesCache::with_ttl(list, Duration::ZERO)),
            facts_asked: Mutex::new(Vec::new()),
        })
    }

    /// Every `(venue, symbol)` a lease asked this lane's `symbol_facts` about, in order.
    pub fn facts_asked(&self) -> Vec<(String, String)> {
        lock(&self.facts_asked).clone()
    }

    /// The recorded calls.
    pub fn calls(&self) -> Vec<Call> {
        lock(&self.log).clone()
    }

    /// Make a `submit` of `order_client_id` panic inside the bridge.
    pub fn poison(&self, order_client_id: &str) {
        lock(&self.panic_on).insert(order_client_id.to_string());
    }
}

impl TradeLane for ActorLane {
    fn venues(&self) -> Vec<String> {
        self.inner.venues()
    }
    fn account_tier(&self, venue: &str, account: Option<&str>) -> Result<AccountTier, ArmingBlock> {
        self.inner.account_tier(venue, account)
    }
    fn arming_table(&self) -> proto::ArmingTableWire {
        self.inner.arming_table()
    }
    fn preflight(&self, venues: &[String], step: proto::PreflightStep) -> proto::PreflightAnswer {
        self.inner.preflight(venues, step)
    }
    fn start_adapter(
        &self,
        target: &TradeLaneTarget,
        start: &proto::AdapterStart,
        events: EventSender,
        trigger: Option<Sender<()>>,
    ) -> Result<StartedAdapter, AdapterRefusal> {
        // The scripted lane keeps its own record (`TradeCall::Start`) and a trigger handle, and answers the
        // start; it is given a lane nobody reads, so it holds no clone of the daemon's.
        let (unused, _rx) = vike_execution::event_channel(1);
        let mut started = self.inner.start_adapter(target, start, unused, trigger)?;
        started.client = Box::new(GuardedActor {
            inner: self.world.actor(events),
            log: Arc::clone(&self.log),
            panic_on: Arc::clone(&self.panic_on),
        });
        Ok(started)
    }
    fn record_identity(
        &self,
        target: &TradeLaneTarget,
        bound_tier: AccountTier,
        recon: Option<&dyn ReconcileClient>,
    ) -> Result<(), String> {
        self.inner.record_identity(target, bound_tier, recon)
    }
    fn resolves_by_order_client_id(&self, venue: &str) -> bool {
        self.inner.resolves_by_order_client_id(venue)
    }
    fn symbol_facts(
        &self,
        target: &TradeLaneTarget,
        symbol: &str,
    ) -> Result<SymbolPropertiesWire, SymbolPropertiesRefusal> {
        lock(&self.facts_asked).push((target.venue.clone(), symbol.to_string()));
        match &self.facts {
            Some(facts) => facts.facts(&target.venue, symbol),
            None => self.inner.symbol_facts(target, symbol),
        }
    }
}

/// The arming table of a gatewayhub serving binance's default account at demo.
pub fn arming(takes_recon_trigger: bool) -> proto::ArmingTableWire {
    proto::ArmingTableWire {
        properties: BTreeMap::from([(
            VENUE.to_string(),
            vike_model::VenueAdapterDeclarationView {
                adapter_present: true,
                addresses_accounts: true,
                takes_reconcile_trigger: takes_recon_trigger,
                grid_source: vike_model::GridSource::NoGrid,
                process_exclusive: None,
                clock_declaration_word: vike_model::ClockDeclarationKind::Wired,
            },
        )]),
        rows: vec![proto::ArmingRowWire {
            venue: VENUE.to_string(),
            label: None,
            tier: Some(AccountTier::Demo),
            effective: Some(AccountTier::Demo),
            block_word: ArmingBlock::None.word().to_string(),
            account_ids: vec![3],
            book: None,
        }],
        key_changed: Vec::new(),
        start_inputs: Default::default(),
    }
}

/// The `account` rows of the rig: binance's default account at demo.
pub fn accounts() -> vike_secrets::account_directory::AccountDirectory {
    vike_tradehub_core::AdapterPolicy::default()
        .with_account(VENUE, &AccountLabel::Default, AccountTier::Demo)
        .accounts
}

/// The rows of the rig with a SECOND, labelled `demo` account of the venue beside the default one (decision
/// 0169: the account an operator activates while the node runs).
pub fn accounts_with(label: &AccountLabel) -> vike_secrets::account_directory::AccountDirectory {
    vike_tradehub_core::AdapterPolicy::default()
        .with_account(VENUE, &AccountLabel::Default, AccountTier::Demo)
        .with_account(VENUE, label, AccountTier::Demo)
        .accounts
}

/// The `AdapterStartQuery` for `label`'s account, whose route key is `route_key` (`venue#LABEL`).
pub fn start_query_for<'a>(
    label: &'a AccountLabel,
    route_key: &'a str,
) -> vike_tradehub_core::AdapterStartQuery<'a> {
    vike_tradehub_core::AdapterStartQuery { route_key, ..start_query(label) }
}

// ---- the server -----------------------------------------------------------------------------------------

/// A gatewayhub server on an ephemeral loopback port over `lane`. Dropping (or [`GatewayServer::stop`]) raises
/// the stop flag and joins it.
pub struct GatewayServer {
    served: ServedThread,
    /// Where it listens.
    pub addr: SocketAddr,
}

impl GatewayServer {
    /// Start serving `lane` (no state directory, no HALT path: the rig's sessions claim no lock).
    pub fn start(lane: Arc<dyn TradeLane>, config: ServerConfig) -> Self {
        Self::start_with(lane, config, TradeKit::default())
    }

    /// [`GatewayServer::start`] with a state directory and/or a HALT path.
    pub fn start_with(lane: Arc<dyn TradeLane>, config: ServerConfig, kit: TradeKit) -> Self {
        let listener = bind_loopback("127.0.0.1:0").expect("bind an ephemeral loopback port");
        let addr = listener.local_addr().expect("the assigned port");
        let lanes = ServerLanes {
            data: Arc::new(DataLaneScript::new(&[])),
            rest: Arc::new(RestLaneScript::new()),
        };
        let setup = TradeSetup {
            lane,
            state_dir: kit.state_dir,
            halt_path: kit.halt_path,
            budget: Arc::new(Budget::new()),
        };
        let served = ServedThread::spawn("hop-rig-gatewayhub", move |stop| {
            serve_with_trade(listener, trade_keys(), lanes, config, setup, stop)
        });
        Self { served, addr }
    }

    /// Raise the stop flag and wait for the server to drain and return. Idempotent.
    pub fn stop(&mut self) {
        self.served.halt();
    }
}

// ---- the proxy --------------------------------------------------------------------------------------------

struct ProxySt {
    target: Mutex<SocketAddr>,
    refuse: AtomicBool,
    swallow_up: AtomicBool,
    swallow_down: AtomicBool,
    connections: Mutex<Vec<TcpStream>>,
    stop: AtomicBool,
}

/// A TCP relay between a client and a server that a test can cut. See the module doc.
pub struct LinkProxy {
    addr: SocketAddr,
    st: Arc<ProxySt>,
    accept: Option<JoinHandle<()>>,
}

fn pump(mut from: TcpStream, mut to: TcpStream, st: Arc<ProxySt>, up: bool) {
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        match from.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let swallow = if up { &st.swallow_up } else { &st.swallow_down };
                if swallow.load(Ordering::SeqCst) {
                    continue;
                }
                if to.write_all(&buffer[..n]).is_err() {
                    break;
                }
            }
        }
    }
    let _ = to.shutdown(Shutdown::Both);
    let _ = from.shutdown(Shutdown::Both);
}

impl LinkProxy {
    /// A relay to `target`, listening on an ephemeral loopback port.
    pub fn start(target: SocketAddr) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind the proxy");
        listener.set_nonblocking(true).expect("a non-blocking listener");
        let addr = listener.local_addr().expect("the proxy's port");
        let st = Arc::new(ProxySt {
            target: Mutex::new(target),
            refuse: AtomicBool::new(false),
            swallow_up: AtomicBool::new(false),
            swallow_down: AtomicBool::new(false),
            connections: Mutex::new(Vec::new()),
            stop: AtomicBool::new(false),
        });
        let accept = {
            let st = Arc::clone(&st);
            std::thread::Builder::new()
                .name("hop-rig-proxy-accept".into())
                .spawn(move || accept_loop(&listener, &st))
                .expect("spawn the proxy accept thread")
        };
        Self { addr, st, accept: Some(accept) }
    }

    /// The `host:port` a client dials.
    pub fn address_string(&self) -> String {
        self.addr.to_string()
    }

    /// Aim new connections at `target` (a restarted server).
    pub fn retarget(&self, target: SocketAddr) {
        *lock(&self.st.target) = target;
    }

    /// Kill every connection and refuse new ones until [`LinkProxy::heal`].
    pub fn cut(&self) {
        self.st.refuse.store(true, Ordering::SeqCst);
        for c in lock(&self.st.connections).drain(..) {
            let _ = c.shutdown(Shutdown::Both);
        }
    }

    /// Accept connections again and stop swallowing either direction.
    pub fn heal(&self) {
        self.st.swallow_up.store(false, Ordering::SeqCst);
        self.st.swallow_down.store(false, Ordering::SeqCst);
        self.st.refuse.store(false, Ordering::SeqCst);
    }

    /// Read and discard everything the client sends (the daemon never reads it).
    pub fn swallow_up(&self, on: bool) {
        self.st.swallow_up.store(on, Ordering::SeqCst);
    }

    /// Read and discard everything the daemon sends (the client never hears it).
    pub fn swallow_down(&self, on: bool) {
        self.st.swallow_down.store(on, Ordering::SeqCst);
    }
}

fn accept_loop(listener: &TcpListener, st: &Arc<ProxySt>) {
    while !st.stop.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((client, _)) => {
                let _ = client.set_nonblocking(false);
                if st.refuse.load(Ordering::SeqCst) {
                    continue; // dropped: the client sees a closed connection
                }
                let target = *lock(&st.target);
                let Ok(server) = TcpStream::connect_timeout(&target, Duration::from_secs(1)) else {
                    continue;
                };
                let _ = client.set_nodelay(true);
                let _ = server.set_nodelay(true);
                let (Ok(c2), Ok(s2), Ok(c3), Ok(s3)) = (
                    client.try_clone(),
                    server.try_clone(),
                    client.try_clone(),
                    server.try_clone(),
                ) else {
                    continue;
                };
                {
                    let mut connections = lock(&st.connections);
                    connections.push(c3);
                    connections.push(s3);
                }
                let (st_up, st_down) = (Arc::clone(st), Arc::clone(st));
                let _ = std::thread::Builder::new()
                    .name("hop-rig-proxy-up".into())
                    .spawn(move || pump(c2, s2, st_up, true));
                let _ = std::thread::Builder::new()
                    .name("hop-rig-proxy-down".into())
                    .spawn(move || pump(server, client, st_down, false));
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(2));
            }
            Err(_) => std::thread::sleep(Duration::from_millis(10)),
        }
    }
}

impl Drop for LinkProxy {
    fn drop(&mut self) {
        self.st.stop.store(true, Ordering::SeqCst);
        self.cut();
        if let Some(t) = self.accept.take() {
            let _ = t.join();
        }
    }
}

// ---- the three together ----------------------------------------------------------------------------------

/// A gatewayhub, a proxy in front of it and the venue behind it. Clients dial [`Hop::link`].
pub struct Hop {
    /// The relay clients dial.
    pub proxy: LinkProxy,
    /// The venue (outlives every gatewayhub).
    pub world: Arc<World>,
    /// The CURRENT daemon's lane.
    pub lane: Arc<ActorLane>,
    gw: Option<GatewayServer>,
    arming: proto::ArmingTableWire,
    /// The fake instrument catalog every daemon of this hop answers `symbol_facts` from.
    catalog: Option<InstrumentList>,
}

impl Hop {
    /// Start a gatewayhub serving [`VENUE`] over `world`, with `arming` as its table.
    pub fn start(world: Arc<World>, arming: proto::ArmingTableWire) -> Self {
        Self::start_with_catalog(world, arming, None)
    }

    /// [`Hop::start`] whose daemon answers `symbol_facts` from the fake instrument catalog `list`
    /// (a restarted daemon answers from the same one).
    pub fn start_with_catalog(
        world: Arc<World>,
        arming: proto::ArmingTableWire,
        catalog: Option<InstrumentList>,
    ) -> Self {
        hold_kept_lists();
        let lane = ActorLane::with_catalog(Arc::clone(&world), arming.clone(), catalog.clone());
        let gw = GatewayServer::start(Arc::clone(&lane) as Arc<dyn TradeLane>, quick_config());
        let proxy = LinkProxy::start(gw.addr);
        Self { proxy, world, lane, gw: Some(gw), arming, catalog }
    }

    /// A trade link to the gatewayhub THROUGH the proxy, with the test timings.
    pub fn link(&self) -> GatewayLink {
        GatewayLink::new(self.proxy.address_string(), trade_keys()).with_timings(fast_timings())
    }

    /// **A gatewayhub restart**: every connection is cut, the daemon stops (its sessions, rings and dedup
    /// sets die with it), a NEW daemon starts over a fresh lane on the same venue, and the proxy is aimed at
    /// it. The new daemon mints new epochs; the venue ([`Hop::world`]) is the same one.
    pub fn restart_gateway(&mut self) {
        self.stop_gateway();
        self.start_gateway();
    }

    /// Stop the daemon (connections cut, new ones refused) and leave it down.
    pub fn stop_gateway(&mut self) {
        self.proxy.cut();
        if let Some(mut gw) = self.gw.take() {
            gw.stop();
        }
    }

    /// Start a fresh daemon after [`Hop::stop_gateway`] and let clients back in.
    pub fn start_gateway(&mut self) {
        let lane = ActorLane::with_catalog(
            Arc::clone(&self.world),
            self.arming.clone(),
            self.catalog.clone(),
        );
        let gw = GatewayServer::start(Arc::clone(&lane) as Arc<dyn TradeLane>, quick_config());
        self.proxy.retarget(gw.addr);
        self.lane = lane;
        self.gw = Some(gw);
        self.proxy.heal();
    }
}

// ---- the core ----------------------------------------------------------------------------------------------

/// What the core's lane receives, readable from a relay thread.
pub struct LaneSource {
    next: Box<dyn FnMut() -> Option<Ingest> + Send>,
}

/// A fresh core lane: the sender every exec client delivers to (stamped `routed(VENUE)`, as a mount makes
/// it) and the receiver the relay reads.
pub fn live_lane() -> (EventSender, LaneSource) {
    let (tx, mut rx) = vike_execution::event_channel(4096);
    (tx.routed(VENUE), LaneSource { next: Box::new(move || rx.blocking_recv()) })
}

impl LaneSource {
    /// Read the lane on a thread of its own and drop everything: a lane nobody relays into a core, whose
    /// producers must still never block on a full queue.
    pub fn discard(mut self) {
        std::thread::Builder::new()
            .name("hop-rig-discard".into())
            .spawn(move || while (self.next)().is_some() {})
            .expect("spawn the discard reader");
    }
}

/// A real core over one engine on [`VENUE`]/[`SYMBOL`] driving `client`, fed by the `live-event-forward`
/// relay.
pub struct CoreRig {
    /// The core. `None` once [`CoreRig::shutdown`] ran.
    pub core: Option<CoreHandle>,
    stop: Arc<AtomicBool>,
    /// Every venue event the relay forwarded, in arrival order (the `Event` inside a `GatewayEvent` too).
    pub relayed: Arc<Mutex<Vec<Event>>>,
    /// Every `Ingest::GatewayEvent` cursor the relay saw, in arrival order.
    pub cursors: Arc<Mutex<Vec<(u64, u64)>>>,
    /// What the lane disclosed about the link, in arrival order: `exec-link up` / `exec-link down` and
    /// `status <stream> <status>` for every `StreamStatus` (the trade link's own words included).
    pub links: Arc<Mutex<Vec<String>>>,
    /// The journal directory, when [`CoreRig::start`] was given one.
    pub journal: Option<PathBuf>,
}

impl CoreRig {
    /// Spawn the core over `client` with `config`, and the relay over `source`.
    pub fn start(
        client: Box<dyn ExecutionClient + Send>,
        source: LaneSource,
        config: CoreConfig,
    ) -> Self {
        Self::start_tuned(client, source, config, |_| {})
    }

    /// [`CoreRig::start`] with the engine handed to `tune` before the core takes it (its tier, its
    /// risk limits): the engine a mount builds is not the bare one.
    pub fn start_tuned(
        client: Box<dyn ExecutionClient + Send>,
        source: LaneSource,
        config: CoreConfig,
        tune: impl FnOnce(&mut Engine<Box<dyn ExecutionClient + Send>>),
    ) -> Self {
        let journal = config.journal.as_ref().map(|j| j.dir.clone());
        let mut engine = Engine::new(
            Account::new(1.0, VENUE, None, BalanceMode::Delta),
            RiskGate::new(RiskLimits::new()),
            client,
            VENUE,
            SYMBOL,
        );
        tune(&mut engine);
        let core = spawn_core(engine, config);
        let stop = Arc::new(AtomicBool::new(false));
        let relayed: Arc<Mutex<Vec<Event>>> = Arc::default();
        let cursors: Arc<Mutex<Vec<(u64, u64)>>> = Arc::default();
        let links: Arc<Mutex<Vec<String>>> = Arc::default();
        {
            let (events, ticks) = (core.event_sender(), core.tick_sender());
            let (stop, relayed, cursors, links) =
                (Arc::clone(&stop), Arc::clone(&relayed), Arc::clone(&cursors), Arc::clone(&links));
            let mut source = source;
            std::thread::Builder::new()
                .name("live-event-forward".into())
                .spawn(move || {
                    while let Some(ing) = (source.next)() {
                        if let Some(event) = ing.venue_event() {
                            lock(&relayed).push(event.clone());
                        }
                        match &ing {
                            Ingest::GatewayEvent(g) => {
                                lock(&cursors).push((g.cursor.gateway_epoch, g.cursor.sequence));
                            }
                            Ingest::TradeLink(u) => lock(&links)
                                .push(format!("exec-link {}", if u.up { "up" } else { "down" })),
                            Ingest::StreamStatus(s) => {
                                lock(&links).push(format!("status {} {:?}", s.stream, s.status));
                            }
                            _ => {}
                        }
                        if stop.load(Ordering::SeqCst) {
                            continue; // teardown: keep draining, stop forwarding
                        }
                        let sent = match ing {
                            Ingest::Event(e) => events.blocking_send(e),
                            Ingest::GatewayEvent(g) => events.blocking_send_gateway(*g),
                            Ingest::StreamStatus(s) => ticks.stream_status(*s),
                            Ingest::TradeLink(u) => events.execution_link(*u),
                            _ => Ok(()),
                        };
                        if sent.is_err() {
                            break;
                        }
                    }
                })
                .expect("spawn the relay");
        }
        Self { core: Some(core), stop, relayed, cursors, links, journal }
    }

    /// The core.
    pub fn core(&self) -> &CoreHandle {
        self.core.as_ref().expect("the core is running")
    }

    /// **Append a second account's engine to the RUNNING core** (decision 0169, slice S1): the engine
    /// [`CoreRig::start`] builds, with `route_key` for its account, handed over the core's own ingest lane.
    pub fn add_account(&self, client: Box<dyn ExecutionClient + Send>, route_key: &str) {
        let mut engine = Engine::new(
            Account::new(1.0, VENUE, None, BalanceMode::Delta),
            RiskGate::new(RiskLimits::new()),
            client,
            VENUE,
            SYMBOL,
        );
        engine.route_key = route_key.to_string();
        self.core()
            .event_sender()
            .add_account_engine(500.0, engine)
            .expect("the core takes the engine");
    }

    /// Relay a started account's own event lane into the RUNNING core, as the production forwarder
    /// (`vike_tradehub_core`'s `run_live_forwarder`) does once the engine has joined: its events
    /// (a cancel's acknowledgement, a reject) then fold into that account's engine. Call it after
    /// [`Self::add_account`] so the first event folds after the engine exists.
    pub fn forward(&self, mut source: LaneSource) {
        let (events, ticks) = (self.core().event_sender(), self.core().tick_sender());
        std::thread::Builder::new()
            .name("hop-rig-forward-account".into())
            .spawn(move || {
                while let Some(ing) = (source.next)() {
                    let sent = match ing {
                        Ingest::Event(e) => events.blocking_send(e),
                        Ingest::GatewayEvent(g) => events.blocking_send_gateway(*g),
                        Ingest::StreamStatus(s) => ticks.stream_status(*s),
                        Ingest::TradeLink(u) => events.execution_link(*u),
                        _ => Ok(()),
                    };
                    if sent.is_err() {
                        break;
                    }
                }
            })
            .expect("spawn the account relay");
    }

    /// Wait until the core publishes `n` engine blocks.
    pub fn wait_engines(&self, n: usize, within: Duration) -> bool {
        wait_until(within, || self.core().snapshot().portfolio.venues.len() == n)
    }

    /// Submit `req` through the core's one order-write path.
    pub fn submit(&self, req: OrderRequest) {
        self.core().send_command(Command::Order(OrderIntent::Submit(Box::new(req))));
    }

    /// Cancel `order_client_id`.
    pub fn cancel(&self, order_client_id: &str) {
        self.core().send_command(Command::Order(OrderIntent::Cancel(order_client_id.to_string())));
    }

    /// Reprice `order_client_id`.
    pub fn modify(&self, order_client_id: &str, new_qty: Option<f64>, new_price: Option<f64>) {
        self.core().send_command(Command::Order(OrderIntent::Modify {
            order_client_id: order_client_id.to_string(),
            new_qty,
            new_price,
            new_trigger_price: None,
        }));
    }

    /// The status the core holds for `order_client_id`.
    pub fn status(&self, order_client_id: &str) -> Option<OrderStatus> {
        self.core()
            .snapshot()
            .orders
            .iter()
            .find(|o| o.order_client_id == order_client_id)
            .map(|o| o.status)
    }

    /// Wait until the core holds `order_client_id` at `want`.
    pub fn wait_status(&self, order_client_id: &str, want: OrderStatus, within: Duration) -> bool {
        wait_until(within, || self.status(order_client_id) == Some(want))
    }

    /// Every order the core holds, as `(order client id, status)`, sorted.
    pub fn orders(&self) -> Vec<(String, OrderStatus)> {
        let mut v: Vec<_> = self
            .core()
            .snapshot()
            .orders
            .iter()
            .map(|o| (o.order_client_id.clone(), o.status))
            .collect();
        v.sort_by(|a, b| a.0.cmp(&b.0));
        v
    }

    /// Stop forwarding, shut the core down losslessly and join it. The journal (when there is one) is
    /// complete after this returns.
    pub fn shutdown(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(core) = self.core.take() {
            core.shutdown_and_join();
        }
    }
}

impl Drop for CoreRig {
    fn drop(&mut self) {
        self.shutdown();
    }
}

// ---- a core-less lane, for the tests that read the client's own deliveries -------------------------------

/// A fresh lane with no core behind it: the sender a client delivers to (`routed(VENUE)`) and the reader a
/// test drains (the gatewayhub client's own fake-peer lane, `fake_trade_peer::Lane`, 4096 deep).
pub fn poll_lane() -> (EventSender, vike_gatewayhub_client::test_support::fake_trade_peer::Lane) {
    vike_gatewayhub_client::test_support::fake_trade_peer::lane_with_capacity(4096)
}

/// The order client id a venue event names, when it names one.
pub fn order_client_id_of(event: &Event) -> Option<&str> {
    Some(match event {
        Event::OrderSubmitted(e) => e.order_client_id.as_str(),
        Event::OrderAccepted(e) => e.order_client_id.as_str(),
        Event::OrderRejected(e) => e.order_client_id.as_str(),
        Event::OrderPartiallyFilled(e) => e.order_client_id.as_str(),
        Event::OrderFilled(e) => e.order_client_id.as_str(),
        Event::OrderCanceled(e) => e.order_client_id.as_str(),
        Event::OrderModified(e) => e.order_client_id.as_str(),
        Event::OrderCancelRejected(e) => e.order_client_id.as_str(),
        Event::OrderModifyRejected(e) => e.order_client_id.as_str(),
        _ => return None,
    })
}

/// Whether `event` ENDS an order (the FSM's terminal states a venue event can reach here).
pub fn is_terminal(event: &Event) -> bool {
    matches!(event, Event::OrderRejected(_) | Event::OrderFilled(_) | Event::OrderCanceled(_))
}

/// The timestamp a lookup's re-stated terminal carries (see [`World::confirm`]).
pub const RESTATED_TIMESTAMP: i64 = 1_700_000_999_000;

/// Whether `event` is a terminal the venue's lookup RE-STATED (see [`World::confirm`]), not one it sent live.
pub fn is_restated(event: &Event) -> bool {
    match event {
        Event::OrderCanceled(e) => e.timestamp == RESTATED_TIMESTAMP,
        Event::OrderFilled(e) => e.timestamp == RESTATED_TIMESTAMP,
        Event::OrderRejected(e) => e.timestamp == RESTATED_TIMESTAMP,
        _ => false,
    }
}

/// The adapter start every direct-session test sends (the session names the account's route key itself).
pub fn start_args() -> proto::AdapterStart {
    proto::AdapterStart {
        rid: 0,
        symbol: SYMBOL.into(),
        declared_legs: Vec::new(),
        reconcile_enabled: true,
        takes_trigger: false,
        risk: None,
        market_slippage: None,
        halt_admit: vike_model::HaltAdmit::Admit,
        withhold: None,
        account_leverage: None,
        read_account_currency: false,
        route_key: "ignored: the session names the account's own".into(),
    }
}

// ---- the whole stack ---------------------------------------------------------------------------------------

/// The `AdapterStartQuery` the tradehub's fold would hand its backend for binance's default account at demo.
pub fn start_query(label: &AccountLabel) -> vike_tradehub_core::AdapterStartQuery<'_> {
    vike_tradehub_core::AdapterStartQuery {
        venue: VENUE,
        account: label,
        tier: AccountTier::Demo,
        symbol: SYMBOL,
        declared_legs: &[],
        reconcile_enabled: true,
        risk_profile: None,
        market_slippage: None,
        halt_admit: vike_model::HaltAdmit::default(),
        withhold: None,
        account_leverage: None,
        route_key: VENUE,
        properties_rec: None,
    }
}

/// Everything the tradehub is on the hop, in one value: a real core, over the `ExecutionClientGateway` the
/// tradehub's own `VenueBackendGateway` returned, dialled through the proxy to a real gatewayhub over the
/// scripted venue. Fields drop in declaration order: the core first, the gatewayhub last.
pub struct Stack {
    /// The core and its relay.
    pub rig: CoreRig,
    /// The tradehub's backend (it keeps the lease open).
    pub backend: vike_tradehub::venue_backend::gateway::VenueBackendGateway,
    /// The reconcile triggers the gatewayhub forwarded (`ReconcileNow`) and the tradehub's own.
    pub triggers: Receiver<String>,
    /// The gatewayhub, its proxy and the venue.
    pub hop: Hop,
}

impl Stack {
    /// Start the whole stack with `config` for the core. The gatewayhub's table says the venue takes the
    /// reconcile trigger.
    pub fn start(config: CoreConfig) -> Self {
        use vike_tradehub::venue_backend::gateway::reteach::SnapshotCell;
        use vike_tradehub_core::VenueBackend as _;
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let hop = Hop::start(World::new(), arming(true));
        let cell: SnapshotCell = Arc::default();
        let backend = vike_tradehub::venue_backend::gateway::VenueBackendGateway::connect(
            hop.link(),
            accounts(),
            format!("vike-tradehub-hop-{}", NEXT.fetch_add(1, Ordering::SeqCst)),
            Duration::from_secs(5),
        )
        .with_timings(fast_trade_timings())
        .with_snapshot_cell(Arc::clone(&cell));
        let (events, source) = live_lane();
        let (trigger, triggers) = std::sync::mpsc::channel::<String>();
        let label = AccountLabel::Default;
        let bind = backend
            .start_adapter(start_query(&label), &events, Some(trigger))
            .expect("the adapter starts over the hop");
        let rig = CoreRig::start(bind.client, source, config);
        assert!(cell.set(rig.core().snapshot_cell()).is_ok(), "the cell is set once");
        Self { rig, backend, triggers, hop }
    }

    /// The venue.
    pub fn world(&self) -> &Arc<World> {
        &self.hop.world
    }
}

/// The fake gatewayhub peer config the WIRE members (`gateway_restart_wire`, `late_currency_wire`,
/// `venue_link_wire`) start: it serves `venue`'s trade lane under `route_key`, answers the arming query with
/// ONE wired `venue` row armed at demo (account id 1) and refuses any other RPC by name, and plays `script`
/// (the fake peer's default when `None`).
pub fn config_for(
    venue: &str,
    route_key: &str,
    script: Option<vike_gatewayhub_client::test_support::fake_trade_peer::Script>,
) -> vike_gatewayhub_client::test_support::fake_trade_peer::TradePeerConfig {
    use vike_gatewayhub_client::proto::{ArmingRowWire, ArmingTableWire, Request, Response};
    use vike_model::{GridSource, VenueAdapterDeclarationView};
    let venue = venue.to_string();
    let mut config = vike_gatewayhub_client::test_support::fake_trade_peer::TradePeerConfig::new();
    config.route_key = route_key.to_string();
    config.features.push(vike_gatewayhub_client::proto::trade_venue_feature(&venue));
    let arming_venue = venue.clone();
    let mut config = config.with_rpc(move |req| match req {
        Request::ArmingQuery => vec![Response::ArmingTable(Box::new(ArmingTableWire {
            properties: std::collections::BTreeMap::from([(
                arming_venue.clone(),
                VenueAdapterDeclarationView {
                    adapter_present: true,
                    addresses_accounts: true,
                    takes_reconcile_trigger: false,
                    grid_source: GridSource::NoGrid,
                    process_exclusive: None,
                    clock_declaration_word: vike_model::ClockDeclarationKind::Wired,
                },
            )]),
            rows: vec![ArmingRowWire {
                venue: arming_venue.clone(),
                label: None,
                tier: Some(AccountTier::Demo),
                effective: Some(AccountTier::Demo),
                block_word: ArmingBlock::None.word().to_string(),
                account_ids: vec![1],
                book: None,
            }],
            key_changed: Vec::new(),
            start_inputs: Default::default(),
        }))],
        other => vec![Response::Error(format!("unscripted: {other:?}"))],
    });
    if let Some(script) = script {
        config.script = script;
    }
    config
}
