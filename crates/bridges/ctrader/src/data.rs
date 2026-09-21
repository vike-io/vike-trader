//! `DataClient` for cTrader: `CtraderData` sends `Command`s over the actor's channel
//! (`conn::ActorHandle`) to (un)subscribe spot/trendbar streams. It does NOT hold a
//! `LiveDataSink` itself — the sink lives on the actor thread (given to `conn::connect_and_auth`
//! at construction, Task 4), because that is the thread that decodes inbound `SPOT_EVENT` frames
//! and is the only place that can push into it without a second handoff. `CtraderData` is purely
//! the command-side half of the seam: mint a `SubscriptionId`, translate the venue-neutral
//! `DataClient` verb into cTrader `Command`s, and (for `unsubscribe`) remember enough per
//! subscription to send the matching Unsubscribe command back. Ports nothing — cTrader Open API
//! (https://help.ctrader.com/open-api/).
//!
//! ## Ownership: single-client vs shared (F4)
//! Two construction paths, distinguished by who owns the actor thread:
//! - [`CtraderData::new`] takes (and OWNS) the [`ActorHandle`] returned by
//!   `conn::connect_and_auth`. This view is the sole owner of the actor thread — its drop joins the
//!   thread, and its `DataClient::shutdown` tears the socket down. This is the data-only path.
//! - [`CtraderData::from_shared`] takes a cloned [`ConnShared`] (command channel + handshake state)
//!   and does NOT own the actor thread. It is a VIEW over an actor owned elsewhere — by a
//!   [`crate::client::CtraderClient`] sharing the socket with a `CtraderExec`. Dropping this view
//!   never joins/closes the socket, and its `DataClient::shutdown` unsubscribes its own streams but
//!   leaves the socket up for the co-mounted exec client (only the owning `CtraderClient` closes it).

use std::collections::HashMap;
use std::sync::mpsc::Sender;

use vike_data::{DataClient, LiveDataError, SubscriptionId, require_live_verb};
use vike_model::{LiveVerb, now_ms};

use crate::conn::{ActorHandle, Command, ConnShared};
use crate::event_mapper;
use crate::proto::ProtoOaTrendbarPeriod;
use crate::symbols::SymbolMap;

/// How many historical bars `subscribe_bars` seeds via `GetTrendbars` before starting the live
/// trendbar stream (F2). Bounded, not paged — 300 bars is enough context for a chart/consumer to
/// render immediately without a full backfill; `Command::GetTrendbars`'s own `count` field caps
/// the venue response at this number too, so the `from_ts` window computed below can be generous
/// without risking an oversized reply.
const SEED_BAR_COUNT: u32 = 300;

/// What one issued `SubscriptionId` maps back to, so `unsubscribe` can send the matching
/// Unsubscribe command. `Quotes` need only the symbol id; `Bars` also carries the trendbar period.
enum Sub {
    Quotes { symbol_id: i64 },
    Bars { symbol_id: i64, period: ProtoOaTrendbarPeriod },
}

/// The cTrader `DataClient`: subscribe/unsubscribe live spot quotes and live trend bars. Holds
/// the actor's command channel + the shared, already-populated `SymbolMap` from the handshake
/// (Task 3); actual data delivery happens on the actor thread via the `LiveDataSink` passed to
/// `conn::connect_and_auth`, not through this type.
///
/// `owner` distinguishes the two construction paths (see the module doc): `Some(ActorHandle)` for
/// the single-client [`CtraderData::new`] path (this view owns + joins the actor thread), `None`
/// for the shared-socket [`CtraderData::from_shared`] view (the actor is owned by a
/// [`crate::client::CtraderClient`] elsewhere). The `shared` slice carries the command channel and
/// symbol map for both paths.
pub struct CtraderData {
    shared: ConnShared,
    /// `Some` iff this view owns the actor thread (the single-client path). Its `Drop` joins the
    /// thread. `None` for a shared view — dropping it must not close the co-mounted socket.
    owner: Option<ActorHandle>,
    next_id: u64,
    subs: HashMap<SubscriptionId, Sub>,
}

impl CtraderData {
    /// Build a `CtraderData` that OWNS an already-authenticated actor handle (the return of
    /// `conn::connect_and_auth`). This view is the sole owner of the actor thread: it joins on
    /// drop, and `shutdown` tears the socket down. Use this for the data-only mount.
    pub fn new(handle: ActorHandle) -> Self {
        let shared = handle.shared();
        CtraderData { shared, owner: Some(handle), next_id: 0, subs: HashMap::new() }
    }

    /// Build a `CtraderData` VIEW over an actor owned elsewhere, from a cloned [`ConnShared`]
    /// (F4 single-socket mount). Does NOT own the actor thread — dropping this view leaves the
    /// socket up, and `shutdown` only unsubscribes this view's own streams. The actor is torn down
    /// solely by the owning [`crate::client::CtraderClient`].
    pub fn from_shared(shared: ConnShared) -> Self {
        CtraderData { shared, owner: None, next_id: 0, subs: HashMap::new() }
    }

    fn symbols(&self) -> &SymbolMap {
        &self.shared.symbols
    }

    fn tx(&self) -> &Sender<Command> {
        &self.shared.tx
    }

    fn mint_id(&mut self) -> SubscriptionId {
        let id = SubscriptionId(self.next_id);
        self.next_id += 1;
        id
    }

    fn resolve_symbol(&self, symbol: &str) -> Result<i64, LiveDataError> {
        self.symbols()
            .id_of(symbol)
            .ok_or(LiveDataError::Unsupported("unknown cTrader symbol (not in SymbolsList)"))
    }

    fn send(&self, cmd: Command) -> Result<(), LiveDataError> {
        self.tx().send(cmd).map_err(|_| LiveDataError::Subscribe("actor thread gone".to_string()))
    }
}

impl DataClient for CtraderData {
    /// Subscribes to live trend bars for `(symbol, interval)`. cTrader requires an active spot
    /// subscription for live trendbar pushes (`ProtoOASubscribeLiveTrendbarReq` doc), so this
    /// also sends `SubscribeSpots` for the same symbol — a second `subscribe_bars`/`subscribe_quotes`
    /// call on the same symbol just re-sends that (idempotent on the venue side; no local dedup).
    /// `interval` values with no cTrader trendbar period equivalent (see
    /// `event_mapper::trendbar_period_for_interval`) return `Unsupported` rather than silently ignoring
    /// the call.
    ///
    /// F2: before the live stream starts, this also fires a bounded one-shot `GetTrendbars`
    /// historical seed — `SEED_BAR_COUNT` bars ending now (`from_ts = now - SEED_BAR_COUNT *
    /// period_ms`, `to_ts = now`), so a fresh subscription arrives with recent closed-bar context
    /// instead of starting blank. Both requests are fire-and-forget over the actor's command
    /// channel (this call never blocks on the network): the actor thread decodes the eventual
    /// `GET_TRENDBARS_RES` and pushes the seeded bars into the `LiveDataSink` via `seed_bars`
    /// (`conn::on_get_trendbars_res`) — best-effort, so an errored/absent seed response never
    /// prevents `SubscribeTrendbar`'s live stream from starting (it was already sent regardless).
    fn subscribe_bars(
        &mut self,
        symbol: &str,
        interval: &str,
    ) -> Result<SubscriptionId, LiveDataError> {
        let symbol_id = self.resolve_symbol(symbol)?;
        let period = event_mapper::trendbar_period_for_interval(interval)
            .ok_or(LiveDataError::Unsupported("interval has no cTrader trendbar period"))?;
        self.send(Command::SubscribeSpots { symbol_id })?;
        let to_ts = now_ms();
        let from_ts = to_ts - event_mapper::trendbar_period_ms(period) * SEED_BAR_COUNT as i64;
        self.send(Command::GetTrendbars {
            symbol_id,
            period,
            from_ts,
            to_ts,
            count: SEED_BAR_COUNT,
        })?;
        self.send(Command::SubscribeTrendbar { symbol_id, period })?;
        let id = self.mint_id();
        self.subs.insert(id, Sub::Bars { symbol_id, period });
        Ok(id)
    }

    /// Subscribes to live L1 quotes (bid/ask) for `symbol`. `Err(Unsupported)` if `symbol` is not
    /// in the handshake's `SymbolsList`.
    fn subscribe_quotes(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        let symbol_id = self.resolve_symbol(symbol)?;
        self.send(Command::SubscribeSpots { symbol_id })?;
        let id = self.mint_id();
        self.subs.insert(id, Sub::Quotes { symbol_id });
        Ok(id)
    }

    /// cTrader's Open API market-data feed serves quotes and trend bars only — no last-trade
    /// print stream (capabilities-not-obligations, spec D7).
    fn subscribe_trades(&mut self, _symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        // Caps-driven refusal (ctrader live_data.trades = false): stays in lockstep with the table.
        require_live_verb("ctrader", LiveVerb::Trades)?;
        unreachable!("ctrader declares no trade-print feed")
    }

    /// No live L2 book feed on the Open API market-data seam used here (depth quotes are a
    /// separate, unimplemented venue verb — `PROTO_OA_SUBSCRIBE_DEPTH_QUOTES_REQ`).
    fn subscribe_book(&mut self, _symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        // Caps-driven refusal (ctrader live_data.book = false): stays in lockstep with the table.
        require_live_verb("ctrader", LiveVerb::Book)?;
        unreachable!("ctrader declares no lossless book lane")
    }

    /// Stop exactly the stream `id` names — unknown/already-stopped ids are a no-op. Sends the
    /// matching Unsubscribe command(s) over the actor channel; a dead actor thread is swallowed
    /// (unsubscribe never panics/propagates an error per the `DataClient` contract).
    fn unsubscribe(&mut self, id: SubscriptionId) {
        if let Some(sub) = self.subs.remove(&id) {
            match sub {
                Sub::Quotes { symbol_id } => {
                    let _ = self.send(Command::UnsubscribeSpots { symbol_id });
                }
                Sub::Bars { symbol_id, period } => {
                    let _ = self.send(Command::UnsubscribeTrendbar { symbol_id, period });
                }
            }
        }
    }

    /// Stop every stream this client owns. In the single-client (`new`) path this ALSO tears the
    /// actor thread down (a `Command::Shutdown`, then the join on drop of the owned `ActorHandle`).
    /// In the shared (`from_shared`) path it unsubscribes this view's streams but does NOT send
    /// `Shutdown` — the socket is co-owned with a `CtraderExec` and its lifecycle belongs to the
    /// owning [`crate::client::CtraderClient`]; killing it here would strand the exec client.
    fn shutdown(&mut self) {
        for (_, sub) in self.subs.drain() {
            match sub {
                Sub::Quotes { symbol_id } => {
                    let _ = self.shared.tx.send(Command::UnsubscribeSpots { symbol_id });
                }
                Sub::Bars { symbol_id, period } => {
                    let _ = self.shared.tx.send(Command::UnsubscribeTrendbar { symbol_id, period });
                }
            }
        }
        if self.owner.is_some() {
            let _ = self.shared.tx.send(Command::Shutdown);
        }
    }
}
