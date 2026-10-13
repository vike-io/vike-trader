//! Hyperliquid execution — the [`ExecutionClient`] over a dedicated `ExecActor`-style command thread.
//!
//! Mirrors the Bybit/OKX exec anatomy (a newtype forwarding to an owned command thread with
//! deterministic Shutdown-and-join teardown), but with a HL-specific command enum: HL's `/exchange`
//! order action is **batch-first**, so `submit_batch`/`cancel_batch` are NATIVE (one signed action
//! with N items, [`vike_model::venues::venue_caps::HYPERLIQUID`]'s `supports_native_batch`) rather than the
//! trait-default fan-out. The shared [`vike_bridge_core::exec_actor::ExecActor`] carries no batch
//! command, which is why this crate rolls its own thin actor.
//!
//! Order flow (research §6):
//! - Rust emits [`OrderSubmitted`] synchronously at submit; the venue terminal events come from the
//!   `/exchange` response ([`map_order_response`]) AND the WS pump ([`crate::exec::user_data`]).
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
//! - `cancel`/`cancel_batch` use `cancelByCloid` (the cloid is derived from the coid); `modify` is a
//!   native cancel-replace (`Action::Modify`) keyed by the resting venue `oid`.
//! - Every failure path is explicit: a build failure (unknown symbol / sub-step size / no mid) and a
//!   DEFINITE transport error synthesize a terminal [`OrderRejected`] — no order silently vanishes.
//!   The ambiguous post-send timeout ([`vike_bridge_core::transport::ErrorKind::must_requery`]) is the
//!   ONE exception: it is never rejected (that would strand a phantom position); the WS pump / recon
//!   resolves the truth.
//!
//! Cloid carry-through: HL's `cloid = keccak(coid)` is one-way, so exec REGISTERS every mapping into a
//! shared [`CloidRegistry`] at submit and the WS pump RESOLVES it when an `orderUpdates`/`userFills`
//! frame carries the cloid back (the pump is spawned here, sharing the registry + the symbology).
//!
//! Restart (decision 0121): the registry and the exec thread's coid → asset map are in memory, so a
//! restart loses both. `wire_id_for` (pure: `keccak(coid)`) lets the core read a report's cloid back
//! into a coid, and `adopt_restored` rebuilds the state for an adopted order — the registry
//! synchronously from the fold thread (the client keeps a handle created before the thread
//! spawns), the asset map by an `HlCommand::Adopt` on the same lane as `Cancel`. This crate
//! never reads the ownership file: the core hands it `(coid, unified symbol)` pairs.
//!
//! LIVE GATE: absent credentials never reach here — the composition root only builds a [`Signer`] and
//! spawns this when the workspace `.env` yields a private key (absent-creds-is-the-live-gate).
//!
//! NOW WIRED (see the impls below): the reconnect→reconcile poke (the pump pokes the `recon_trigger`
//! threaded through [`HyperliquidExecutionClient::spawn`] on every WS reconnect, so a reconcile pass
//! re-syncs state missed while the socket was down), and **stale-`orderUpdates`-cancel suppression
//! during a native `modify`** — a native `Action::Modify` keeps the SAME cloid on a NEW oid, so a
//! `canceled` streamed for the retired old oid would (same cloid → same coid) prematurely terminate
//! the FSM entry the new oid now owns. `modify` records the old oid on [`CloidRegistry`] with a TTL
//! *after* the replace succeeds (so a REJECTED modify never mis-fires) and the pump drops a cancel
//! for a retired oid. Residual: a `canceled` the pump processes before exec finishes the retire
//! (WS-faster-than-REST) still slips through — the periodic reconcile is the backstop.
//!
//! NOT YET (deliberately out of this fan-out): pruning the per-order/cloid maps on terminal events
//! (they grow with a session's submitted orders).

mod actor;
mod client;
pub mod event_mapper;
mod order_wire;
pub mod outcome_settlement;
pub mod recon_client;
mod registry;
mod state;
pub mod user_data;

pub use client::HyperliquidExecutionClient;
pub use registry::CloidRegistry;

use std::time::Duration;

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
use vike_exec::EventSender;
#[cfg(any(test, doc))]
use vike_exec::ExecutionClient;
#[cfg(test)]
use vike_model::events::Event;
#[cfg(doc)]
use vike_model::events::{OrderRejected, OrderSubmitted};
#[cfg(test)]
use vike_model::{OrderRequest, TimeInForce};

#[cfg(test)]
use crate::base::consts::VENUE;
#[cfg(test)]
use crate::base::symbology::Symbology;
#[cfg(test)]
use crate::exec::event_mapper::cloid_from_client_order_id;
#[cfg(doc)]
use crate::exec::event_mapper::map_order_response;
#[cfg(doc)]
use crate::signing::Signer;
#[cfg(test)]
use crate::signing::action::{Action, HlBuilderFee, OrderKind};

#[cfg(doc)]
use order_wire::build_trigger_kind;
#[cfg(test)]
use order_wire::{HlExchange, hl_tif};
#[cfg(test)]
use state::{ExecState, HlCommand};

/// FALLBACK emulated-market slippage band off the reference price — the literal this adapter priced
/// every market order with unconditionally before the operator's band was threaded in (research §6:
/// all HL adapters emulate a market order as an `Ioc` limit at ±5%). Used ONLY when the operator
/// expressed no band, so a mount that sets nothing is byte-identical to every mount before
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
/// Pure, and `pub` on purpose — the SAME shape [`crate::exec`]'s perp siblings use for leverage
/// (`vike_binance::exec::leverage_for` and friends): the composition root resolves this ONCE per
/// mount and threads the resolved `f64` into [`HyperliquidExecutionClient::spawn_with_market_slippage`].
/// Nothing inside the adapter reaches for a config, and no spawned thread re-reads one, so the band
/// on the wire and the band the operator configured can never disagree.
///
/// Unset (`None`) ⇒ [`DEFAULT_MARKET_SLIPPAGE`], byte-identical to before. An out-of-range value is
/// clamped and warned, never honored — see `vike_bridge_core::market_slippage` for the full rule.
pub fn market_slippage_for(requested: Option<f64>) -> f64 {
    vike_bridge_core::market_slippage::resolve_market_slippage(requested, DEFAULT_MARKET_SLIPPAGE)
}
/// Funding-payment poll cadence (`userFunding`; not on either WS channel this crate subscribes to
/// — see `crate::exec::user_data`'s module doc). 60s keeps live equity current without over-querying.
const FUNDING_POLL_INTERVAL: Duration = Duration::from_secs(60);

#[path = "exec_tests.rs"]
#[cfg(test)]
mod exec_tests;
