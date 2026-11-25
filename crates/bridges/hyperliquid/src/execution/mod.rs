//! Hyperliquid execution — the [`ExecutionClient`] is the shared [`vike_bridge_core::exec_actor::ExecutionClientActor`] over
//! this crate's own command loop ([`client::spawn`]; `actor.rs`'s `run`).
//!
//! HL's `/exchange` order action is **batch-first**, so `submit_batch`/`cancel_batch` are NATIVE (one signed action
//! with N items, [`vike_venues::venue_caps::HYPERLIQUID`]'s `supports_native_batch`) rather than the trait-default
//! fan-out: the actor is built with `with_bulk_submit` and `with_bulk_cancel`, and its HALT boundary judges a batch
//! as a unit (`client.rs`'s `with_venue_hooks`).
//!
//! Order flow (research §6):
//! - Rust emits [`OrderSubmitted`] synchronously at submit; the venue terminal events come from the
//!   `/exchange` response ([`map_order_response`]) AND the WS pump ([`crate::execution::user_data`]).
//! - Limit orders map `time_in_force` → the case-sensitive wire TIF (`Gtc`/`Ioc`); **market orders
//!   are emulated** as an `Ioc` limit at ±the mount-resolved slippage band ([`market_slippage_for`],
//!   default [`DEFAULT_MARKET_SLIPPAGE`]) off the current mid (fetched via `allMids`); no quote ⇒ a
//!   locally-synthesized [`OrderRejected`].
//! - **Stop/trigger orders** (`OrderType::Stop`, or any request carrying a `trigger_price`) map
//!   to `OrderKind::Trigger { is_market, trigger_px, tpsl }` (research §6): a stop-MARKET (no
//!   `price`) fires aggressively when it trips (protective ± that SAME band off the trigger,
//!   NOT the far-away current mid); a stop-LIMIT (`price` present) rests at the requested limit.
//!   `tpsl` is `"sl"` for a stop-loss (`OrderType::Stop` / a bare `trigger_price`) or `"tp"` for
//!   a take-profit (`OrderType::TakeProfit`) — see [`build_trigger_kind`].
//! - `cancel`/`cancel_batch` use `cancelByCloid` (the cloid is derived from the order client id); `modify` is a
//!   native cancel-replace (`Action::Modify`) keyed by the resting venue `oid`. One exception, for
//!   the node's `recon cancel` of an order this process never placed: a `cancel` whose id is
//!   already a venue `cloid` (`0x` and 32 hex digits) that exec holds no entry for is sent as printed,
//!   on the asset of the coin the account's `frontendOpenOrders` list shows for it
//!   (`state::ExecutionState::cancel_wire_cloid`).
//! - **Any coin of the account's own network is placeable with no hand step.** Before an opening order
//!   on a perp coin the margin guard (`margin.rs`; the TESTNET only: a `live` account is never written
//!   to) makes sure the account holds it in cross margin (one
//!   `activeAssetData` read; an isolated coin with no open position is switched with one signed
//!   `updateLeverage`, keeping the figure the account holds), and a coin the start's universe lacks is
//!   looked up again once, rate-limited (`universe.rs`), before it is refused in words naming the network.
//! - Every failure path is explicit: a build failure (unknown symbol / sub-step size / no mid) and a
//!   DEFINITE transport error synthesize a terminal [`OrderRejected`] — no order silently vanishes.
//!   The ambiguous post-send timeout ([`vike_bridge_core::transport::ErrorKind::must_requery`]) is the
//!   ONE exception: it is never rejected (that would strand a phantom position); the WS pump, the
//!   watchdog's confirm (`confirm.rs`, `orderStatus` by cloid) and recon resolve the truth.
//!
//! Cloid carry-through: HL's `cloid = keccak(order client id)` is one-way, so exec REGISTERS every mapping into a
//! shared [`OrderClientIdVenueRegistry`] at submit and the WS pump RESOLVES it when an `orderUpdates`/`userFills`
//! frame carries the cloid back (the pump is spawned here, sharing the registry + the symbology); the
//! reconcile client reads the same registry for its `/info` rows ([`crate::reconcile`]).
//!
//! Restart (decision 0121): the registry and the exec thread's order client id → asset map are in memory, so a
//! restart loses both. `wire_id_for` (pure: `keccak(order client id)`) lets the core read a report's cloid back
//! into an order client id, and `adopt_restored` rebuilds the state for an adopted order — the registry
//! synchronously from the fold thread (the actor's `with_adopt_restored` hook holds a handle created
//! before the thread spawns), the asset map by an `ExecutionClientActorCommand::Adopt` on the same lane as `Cancel`
//! (`with_adopt_queued`). This crate never reads the ownership file: the core hands it
//! `(order client id, unified symbol)` pairs.
//!
//! LIVE GATE: absent credentials never reach here — the composition root only builds a [`Signer`] and
//! spawns this when the workspace `.env` yields a private key (absent-creds-is-the-live-gate).
//!
//! NOW WIRED (see the impls below): the reconnect→recon trigger (the pump fires the `reconcile_trigger`
//! threaded through [`client::spawn`] once each WS reconnect's `userFills` snapshot is decoded, so a reconcile pass
//! re-syncs state missed while the socket was down), and **stale-`orderUpdates`-cancel suppression
//! during a native `modify`** — a native `Action::Modify` keeps the SAME cloid on a NEW oid, so a
//! `canceled` streamed for the retired old oid would (same cloid → same order client id) prematurely terminate
//! the FSM entry the new oid now owns. `modify` records the old oid on [`OrderClientIdVenueRegistry`] with a TTL
//! *after* the replace succeeds (so a REJECTED modify never mis-fires) and the pump drops a cancel
//! for a retired oid. Residual: a `canceled` the pump processes before exec finishes the retire
//! (WS-faster-than-REST) still slips through — the periodic reconcile is the backstop.
//!
//! NOT YET (deliberately out of this fan-out): pruning the per-order/cloid maps on terminal events
//! (they grow with a session's submitted orders).

mod actor;
pub mod client;
mod confirm;
pub mod event_mapper;
pub(crate) mod order_wire;
pub(crate) mod registry;
pub(crate) mod spot;
pub(crate) mod state;
pub mod user_data;

pub mod settlement;

pub use registry::OrderClientIdVenueRegistry;

#[cfg(test)]
use std::collections::HashMap;
#[cfg(test)]
use std::sync::atomic::AtomicBool;
#[cfg(test)]
use std::sync::mpsc::{Receiver, channel};
#[cfg(test)]
use std::sync::{Arc, Mutex};

#[cfg(test)]
use serde_json::Value;

#[cfg(test)]
use vike_bridge_core::transport::VenueApiError;
#[cfg(test)]
use vike_execution::EventSender;
#[cfg(any(test, doc))]
use vike_execution::ExecutionClient;
#[cfg(test)]
use vike_model::events::Event;
#[cfg(doc)]
use vike_model::events::{OrderRejected, OrderSubmitted};
#[cfg(test)]
use vike_model::{OrderRequest, TimeInForce};

#[cfg(test)]
use crate::execution::event_mapper::cloid_from_order_client_id;
#[cfg(doc)]
use crate::execution::event_mapper::map_order_response;
#[cfg(test)]
use crate::instrument::symbology::Symbology;
#[cfg(doc)]
use crate::signing::Signer;
#[cfg(test)]
use crate::signing::action::{Action, HyperliquidBuilderFee, OrderKind};
#[cfg(test)]
use crate::transport::consts::VENUE;

#[cfg(doc)]
use order_wire::build_trigger_kind;
#[cfg(test)]
use order_wire::{HyperliquidExchange, hl_time_in_force};
#[cfg(test)]
use state::ExecutionState;

/// FALLBACK emulated-market slippage band off the reference price — the literal this adapter priced
/// every market order with unconditionally before the operator's band was threaded in (research §6:
/// all HL adapters emulate a market order as an `Ioc` limit at ±5%). Used ONLY when the operator
/// expressed no band, so a start that sets nothing is byte-identical to every start before
/// [`market_slippage_for`] existed.
///
/// ⚠ Read what this number actually authorizes. It is not a slippage ESTIMATE and not a fee — it is
/// the worst price the order is ALLOWED to reach. On a deep BTC book it never binds (the `Ioc` fills
/// at the touch); on a thin alt book it binds completely, and the order fills 5% away **silently** —
/// no rejection, no alert, nothing in the fill distinguishing "the market moved" from "we authorized
/// this". A liquid major wants 0.1–0.5%; the safe direction is DOWN. Hence
/// [`vike_bridge_core::market_slippage`], whose ceiling is exactly this value, making the knob a
/// one-way ratchet.
const DEFAULT_MARKET_SLIPPAGE: f64 = 0.05;

/// The emulated-market slippage band this adapter prices with, resolved from the operator's policy.
///
/// Pure, and `pub` on purpose — the SAME shape [`crate::execution`]'s perp siblings use for leverage
/// (`vike_bridge_core::leverage::leverage_for`): the composition root resolves this ONCE per
/// start and threads the resolved `f64` into [`client::spawn_with_market_slippage`].
/// Nothing inside the adapter reaches for a config, and no spawned thread re-reads one, so the band
/// on the wire and the band the operator configured can never disagree.
///
/// Unset (`None`) ⇒ [`DEFAULT_MARKET_SLIPPAGE`], byte-identical to before. An out-of-range value is
/// clamped and warned, never honored — see `vike_bridge_core::market_slippage` for the full rule.
pub fn market_slippage_for(requested: Option<f64>) -> f64 {
    vike_bridge_core::market_slippage::resolve_market_slippage(requested, DEFAULT_MARKET_SLIPPAGE)
}

#[path = "execution_tests.rs"]
#[cfg(test)]
mod execution_tests;
