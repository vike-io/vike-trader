//! Deribit live exec half — `ExecutionClient` over the persistent authed JSON-RPC order WS + the
//! private `user.trades` fill stream. The live twin of bybit's exec half (linear perp) and okx's
//! (SWAP), assembled from the EXISTING deribit parts — NOTHING here is reimplemented, this is the
//! wiring seam:
//!
//! * order entry: [`VenueRestDeribit`](crate::execution::rest::VenueRestDeribit) over a persistent authed
//!   [`DeribitTransport`](crate::transport::client::DeribitTransport) — JSON-RPC `private/buy` /
//!   `private/sell` / `private/cancel` (`VenueRestDeribit::submit_order` / `cancel_order`, the pinned
//!   coin-unit scaling site; `post_only` is forced FALSE there so a marketable order isn't repriced).
//! * fills: [`spawn_deribit_user_data_with_resync`](crate::execution::user_data::spawn_deribit_user_data_with_resync)
//!   — the `user.trades.any.any.raw` pump (all kinds, all currencies → futures AND options fills
//!   stream live on one subscription) + the audit-A3 reconnect resync (recent order/trade history
//!   replayed through a SEPARATE authed socket).
//!
//! The client IS the shared [`ExecutionClientActor`] ([`spawn_deribit_execution_client`]), with no wrapper of its
//! own: `submit`/`cancel` enqueue onto ONE dedicated OS thread
//! that drives the authed order socket (blocking, off the single-writer core). `submit_order` emits
//! `[OrderSubmitted, OrderAccepted|OrderRejected]` synchronously; the authoritative async fills/cancels
//! return on the private WS pump into the core ingest. ⚠ Both ride ONE [`SubmitFence`]: a fill the
//! venue pushes before it has answered the order is held and sent after the order's own events
//! (`vike_bridge_core::exec_actor::submit_fence`'s module doc), so the core never sees a fill ahead of
//! `OrderAccepted`.
//!
//! LIVE GATE: absent credentials never reach here — the start only spawns this when
//! `vike_secrets::credentials::api_credentials` yields `Some` for a `demo` account (the codebase's
//! absent-creds-is-the-live-gate rule). An account with no keys does not run. Demo credentials route
//! the TESTNET order/user-data endpoints.
//!
//! DERIBIT SPECIFICS (vs the binance/bybit template):
//! * `currency` is the instrument row's SETTLEMENT currency (`BTC-PERPETUAL` → BTC,
//!   `BTC_USDC-PERPETUAL` → USDC), the scope of the label lookup; `currency_of`, the token before
//!   the first `-`, is only the fallback for a row that could not be read. The mounted client also
//!   signs on other perpetuals of the same money model (`VenueRestDeribit::with_foreign_instruments`,
//!   `docs/ops/deribit-any-symbol-plan.md`). The fill stream, however, subscribes the
//!   `any`-kind/`any`-currency `user.trades` channel, so it is NOT scoped to the start's symbol —
//!   futures AND options fills across every currency stream live. The perp is the simplest live
//!   target; options work the same way.
//! * amount/price are COIN units on the venue's per-instrument tick/step grid — the scaling lives once
//!   in [`crate::execution::rest`] (`format_to_step_f`); this seam invents no new scaling.
//! * scope: `submit`, `cancel` and `modify`. `modify` is Deribit's `private/edit`
//!   (`ExecutionClientActorCommand::Modify` → `VenueRestDeribit::modify_order` → `VenueRestDeribit::edit_order`): the resting
//!   limit order is edited IN PLACE (same venue order id, same label), the declared caps say
//!   `supports_modify == true` and `vike_venues::venue_amend`'s row says `Unknown` (the partial-fill
//!   convention is unmeasured); every outcome is an event, `OrderModified` or a non-terminal `OrderModifyRejected`.
//!   Native batch is absent (fans out to per-order submits via the `ExecutionClient` default).
//! * trigger orders: `stop`, `stop_limit`, `take_profit` and `trailing` go out as the venue's own
//!   `stop_market`, `stop_limit`, `take_market`/`take_limit` and `trailing_stop` (the trail as
//!   `trigger_offset`; no activation, no `trailing_limit`) ([`crate::execution::rest`]'s `order_params_on`), so the VENUE
//!   holds the trigger and a stop survives this process. An untriggered stop is `OrderAccepted` at the
//!   submit, and the next thing the `user.trades` pump delivers for it is the fill of the market order the
//!   trigger creates (a NEW venue order id carrying the stop's `label`, so it joins by order client id). A stop the
//!   venue ends WITHOUT a fill (cancelled elsewhere, or triggered into an order the venue refuses) leaves
//!   no fill and no history row, so the pump ALSO subscribes `user.orders` and
//!   [`crate::execution::event_mapper::map_deribit_order_row`] turns the stop's rows into `OrderCanceled` /
//!   `OrderTriggered`; `crates/bridges/deribit/CLAUDE.md` has the measurements and what is still
//!   unmeasured.
//! * funding: a perpetual mount's funding is polled from the account's transaction log on a
//!   background thread of its own ([`crate::execution::funding`], `spawn_funding_poll`): no stream carries it.

// The combo-book ORDER-ENTRY wire model (`private/create_combo` and the `private/buy`/`sell` params
// it resolves to, quantized through the order-wire formatter): pure, but its one consumer is
// `client`, so it rides `exec` with it.
pub mod combo;
pub mod event_mapper;
// The perpetual funding poll's decoder and poller (the account's transaction log): an authed read, so
// it rides `exec` with the exec thread that spawns it.
pub mod funding;
pub mod rest;
pub mod resync;
pub mod user_data;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::thread;
use std::time::Duration;

use serde_json::{Value, json};

use vike_bridge_core::exec_actor::submit_fence::{SubmitFence, VenueRestOrderSubmitFence};
use vike_bridge_core::exec_actor::{
    DEFAULT_FILL_SENTINEL, ExecutionClientActor, ExecutionClientActorCommand, forward_poll,
    run_loop,
};
use vike_bridge_core::per_symbol::NEGATIVE_TTL;
use vike_bridge_core::transport::{RestTransport, UreqTransport, VenueApiError};
use vike_bridge_core::venue_adapter::await_properties;
use vike_bridge_core::websocket_session::user::{RESYNC_HISTORY_LIMIT, sleep_unless_stopped};
use vike_execution::EventSender;
use vike_model::events::Event;
use vike_model::{SymbolProperties, now_ms, now_ns};
use vike_secrets::credentials::Credentials;
use vike_venues::venue_urls::VenueUrls;

use crate::execution::funding::{DeribitFundingPoller, PATH_TRANSACTION_LOG, funding_currency};
use crate::execution::rest::{MoneyModel, VenueRestDeribit};
use crate::execution::resync::map_deribit_history;
// The re-dial POLICY, borrowed rather than re-derived: [`A3Resync`] and
// `crates/bridges/deribit/src/reconcile/mod.rs`'s `ReconcileClientDeribit` are the crate's two
// idempotent-READ socket owners, and a second copy of "which failures a fresh socket might fix"
// would be free to drift from the one the recon lane's incident calibrated. Its doc carries the
// precondition this module has to meet.
use crate::execution::user_data::spawn_deribit_user_data_with_resync;
use crate::reconcile::{ReconcileClientDeribit, earns_a_redial};
use crate::transport::client::{DeribitTransport, DeribitUrls, testnet_rest};

/// The funding poll's cadence, the bybit poll's (`crates/bridges/bybit/src/execution/mod.rs`'s
/// `FUNDING_POLL_INTERVAL`): the venue realizes funding at a trade and at the daily settlement, so a
/// minute keeps own PnL current without leaning on the account's read budget.
const FUNDING_POLL_INTERVAL: Duration = Duration::from_secs(60);

/// Deribit's `currency` (BTC/ETH/SOL) is the token before the first `-` in every instrument name
/// (`BTC-PERPETUAL`, `BTC-1JAN27-100000-C`). Uppercased; empty-safe.
fn currency_of(symbol: &str) -> String {
    symbol.split('-').next().unwrap_or(symbol).to_uppercase()
}

/// The `result` row of the keyless TESTNET `public/get_instrument` for `symbol`, at the REST base `rest`
/// ([`DeribitUrls::rest`]): the ONE read the mounted instrument's grid, contract value, settlement currency
/// and money model all come from, and the read a foreign instrument is resolved by (the book the
/// adapter signs on is the testnet). `Err` names what failed: the read (the venue's code and words), or
/// an answer with no row (an unlisted symbol).
pub fn fetch_instrument_row(rest: &str, symbol: &str) -> Result<Value, String> {
    instrument_row_over(&UreqTransport::new("deribit"), rest, symbol)
}

/// How long the read of a FOREIGN instrument's row may take. It runs on the exec thread inside a
/// submit or an amend, so every command queued behind it (a cancel included) waits for it: the shared
/// transport's 30 s would stall them for that long on a slow testnet, and a failure is remembered
/// only for a short while. A row is a few hundred bytes from a keyless host.
const FOREIGN_ROW_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(4);

/// [`fetch_instrument_row`] bounded by [`FOREIGN_ROW_TIMEOUT`], for the read the exec thread makes on
/// the first order per foreign instrument.
fn fetch_foreign_instrument_row(rest: &str, symbol: &str) -> Option<Value> {
    let agent = vike_bridge_core::http::blocking_agent_with_timeout(FOREIGN_ROW_TIMEOUT);
    instrument_row_over(&UreqTransport::with_agent("deribit", agent), rest, symbol).ok()
}

fn instrument_row_over(
    transport: &UreqTransport,
    rest: &str,
    symbol: &str,
) -> Result<Value, String> {
    const PATH: &str = "/api/v2/public/get_instrument";
    let response = transport
        .public(rest, PATH, &[("instrument_name", symbol.to_string())])
        .map_err(|e| format!("{PATH} could not be read: code {} {}", e.code, e.msg))?;
    response.get("result").cloned().ok_or_else(|| format!("{PATH} answered no row for {symbol}"))
}

/// Every FUTURE-kind row (perpetuals and dated futures, every currency) of the TESTNET book, mapped
/// through the catalog's own parser: the instrument list behind the gatewayhub's symbol facts for a
/// deribit account, which signs on the testnet whatever the public catalog (mainnet) lists, at deribit's
/// address rows `urls` ([`testnet_rest`]). One keyless request; the consumer keeps the answer.
pub fn fetch_testnet_futures(urls: &VenueUrls) -> Result<Vec<vike_model::Instrument>, String> {
    let rest = testnet_rest(urls).map_err(|e| e.to_string())?;
    let response = UreqTransport::new("deribit")
        .public(
            rest,
            "/api/v2/public/get_instruments",
            &[("currency", "any".to_string()), ("kind", "future".to_string())],
        )
        .map_err(|e| e.msg)?;
    Ok(crate::catalog::parse_instruments(&response))
}

/// The venue's code for an instrument it does not list (`instrument_not_found`).
const INSTRUMENT_NOT_FOUND: i64 = 13020;

/// ONE instrument's row of the TESTNET book, mapped through the catalog's own parser (decision 0233): the
/// symbol facts of an OPTION, which the venue lists by the thousand and so are asked one at a time. An
/// instrument the venue does not list answers no rows (the consumer says "no such instrument"); any other
/// failure is an `Err`, never an empty answer. `urls` are deribit's address rows ([`testnet_rest`]).
pub fn fetch_testnet_instrument(
    urls: &VenueUrls,
    symbol: &str,
) -> Result<Vec<vike_model::Instrument>, String> {
    let rest = testnet_rest(urls).map_err(|e| e.to_string())?;
    match UreqTransport::new("deribit").public(
        rest,
        "/api/v2/public/get_instrument",
        &[("instrument_name", symbol.to_string())],
    ) {
        Ok(response) => {
            let row = response.get("result").cloned().unwrap_or(Value::Null);
            Ok(crate::catalog::parse_instruments(&json!({ "result": [row] })))
        }
        Err(e) if e.code == INSTRUMENT_NOT_FOUND => Ok(Vec::new()),
        Err(e) => Err(e.msg),
    }
}

/// The currency a row's contract SETTLES in, upper case: what `private/get_order_state_by_label`,
/// `private/get_account_summary` and the positions read are scoped by. `None` for a row that mints no
/// derivative contract (a combo, a spot pair, a row without its settlement currency): the caller then
/// falls back to `currency_of`.
pub fn settlement_currency_of(row: &Value) -> Option<String> {
    match crate::catalog::contract_of(row)? {
        vike_model::Contract::Perp(p) => Some(p.settle),
        vike_model::Contract::Future(f) => Some(f.settle),
        vike_model::Contract::Option(o) => Some(o.settle),
        _ => None,
    }
}

/// Fetch `symbol`'s REAL tick/step grid from the KEYLESS public `public/get_instrument`
/// (`tick_size` + `min_trade_amount` → step = min_qty; no per-instrument max/min-notional, matching
/// the option-chain parser's convention in [`crate::execution::rest`]). `None` on any network/parse failure or
/// unknown symbol: there is no fallback grid (owner, 2026-11-13). Works for BOTH options and futures/perps (same
/// fields), so no new scaling is invented. `rest` is the testnet REST base ([`DeribitUrls::rest`]).
pub fn fetch_deribit_properties(rest: &str, symbol: &str) -> Option<SymbolProperties> {
    let response = UreqTransport::new("deribit")
        .public(rest, "/api/v2/public/get_instrument", &[("instrument_name", symbol.to_string())])
        .ok()?;
    parse_get_instrument(&response)
}

/// The PURE half of [`fetch_deribit_properties`]: one `public/get_instrument` response envelope →
/// the symbol's grid. Split out from the fetch so the parse (notably `contract_size`, which the
/// order-entry notional cap depends on) is fixture-testable without network.
///
/// This is the venue's PRODUCTION properties writer for EVERY kind — options, futures and perps
/// alike (`run` records exactly this grid into the PIT properties store through
/// `vike_data::PropertiesRecorder::record_opt`), so the class it carries is the class those rows
/// are stored with.
///
/// The row is read by `crates/bridges/deribit/src/catalog.rs`'s `properties_of` — the SAME
/// function the plural `public/get_instruments` catalog runs on each of its rows — so the grid
/// (tick, tiered `tick_size_steps` scheme, step/min from `min_trade_amount`, `contract_size`) and
/// the class (the venue's `kind` + `settlement_period`, never the instrument NAME) are decided ONCE
/// for both payloads. A row the venue calls `future_combo`/`option_combo`, or one publishing no
/// `kind`, still yields a usable grid with the class ABSENT.
pub fn parse_get_instrument(response: &serde_json::Value) -> Option<SymbolProperties> {
    crate::catalog::properties_of(response.get("result")?)
}

/// What ONE `public/get_instrument` row says about the engine symbol, for the mount: its order grid
/// and what a unit of its quantity is worth.
///
/// `value` is `Err(reason)` when the row cannot be valued without a guess
/// ([`crate::catalog::contract_value_of`]: a future or perpetual that states no margining, or a
/// quanto one); the mount then does not run and says why (decision 0170, item 6).
#[derive(Debug, Clone, PartialEq)]
pub struct DeribitProperties {
    /// The order grid ([`parse_get_instrument`]).
    pub grid: SymbolProperties,
    /// The contract value, or the reason there is none.
    pub value: Result<vike_model::ContractValue, String>,
}

/// [`fetch_deribit_properties`] with the contract value read from the SAME row: one keyless
/// `public/get_instrument` round trip answers both, at the testnet REST base `rest`
/// ([`DeribitUrls::rest`]). `None` on any network or parse failure.
pub fn fetch_deribit_instrument(rest: &str, symbol: &str) -> Option<DeribitProperties> {
    let response = UreqTransport::new("deribit")
        .public(rest, "/api/v2/public/get_instrument", &[("instrument_name", symbol.to_string())])
        .ok()?;
    parse_instrument_facts(&response)
}

/// The PURE half of [`fetch_deribit_instrument`]: the grid [`parse_get_instrument`] reads and the
/// value [`crate::catalog::contract_value_of`] reads, off one response envelope. `None` exactly when
/// [`parse_get_instrument`] is.
pub fn parse_instrument_facts(response: &serde_json::Value) -> Option<DeribitProperties> {
    let row = response.get("result")?;
    let grid = crate::catalog::properties_of(row)?;
    Some(DeribitProperties { grid, value: crate::catalog::contract_value_of(row) })
}

/// Fetch + map recent Deribit order/trade history to events — the `run_loop` FILL-SENTINEL's
/// replay body, on the EXEC order `rest`.
///
/// ⚠ **It deliberately does NOT heal a dead socket, and that is the difference between it and
/// [`A3Resync::history_events`].** This runs on the transport that also carries `private/buy` /
/// `private/sell` / `private/cancel`, whose healing policy is already decided — and decided
/// DIFFERENTLY — by `crates/bridges/deribit/src/execution/rest.rs`'s `redial_order_socket`, at the order
/// boundary where an ambiguous submit can be re-QUERIED instead of re-sent. Adding a re-dial here
/// would hand that socket a second, unreviewed re-dial site reachable from a timer. A failed
/// sentinel fetch stays an empty replay: the sentinel is a dedup'd safety net over the live pump,
/// never the primary fill path.
fn deribit_history_events(rest: &VenueRestDeribit, symbol: &str) -> Vec<Event> {
    let orders = rest.get_order_history(RESYNC_HISTORY_LIMIT).unwrap_or_else(|_| json!([]));
    let trades = rest.get_user_trades(RESYNC_HISTORY_LIMIT).unwrap_or_else(|_| json!([]));
    map_deribit_history(&orders, &trades, "deribit", symbol)
}

/// The audit-A3 resync's history source: the SECOND authed order-WS (never the exec side's), plus
/// the re-dial that keeps it usable for the life of the start.
///
/// ## Which cure this is, and why it is the RECON one
///
/// This crate now heals a dead order-WS in three places and the three are NOT interchangeable —
/// `crates/bridges/deribit/CLAUDE.md` carries the split. This is the RECON shape (re-dial **and**
/// re-send), not the exec shape (re-dial, never re-send, re-QUERY instead), and the licence is
/// that every method this type can put on the wire is an idempotent READ. Its socket carries
/// exactly three frames, ever:
///
/// * `public/auth`, from [`DeribitTransport::connect`];
/// * `private/get_order_history_by_instrument` ([`VenueRestDeribit::get_order_history`]);
/// * `private/get_user_trades_by_instrument` ([`VenueRestDeribit::get_user_trades`]).
///
/// None of the three can place, cancel, move or double anything, so re-sending one over a fresh
/// socket is safe in the way a `private/buy` never is. Same fact from the rate limiter's side:
/// `crates/bridges/deribit/src/transport/client.rs`'s `is_matching_engine` classifies both reads as
/// non-matching-engine, so they draw the read budget rather than the order budget.
///
/// ⚠ **The list above is the whole argument, so it is a list of what this type CAN send, not of
/// what it happens to send today.** Anything that widens it past a read invalidates the cure and
/// makes this the exec path's problem instead.
///
/// ## What it fixes
///
/// [`DeribitTransport`] never re-dials itself and leaves a failed socket in place, and this
/// socket's `connect()` ran exactly once, at start. A venue-side close therefore disabled the A3
/// replay for the life of the process. Its failure mode is quieter than the recon lane's and than
/// the order lane's: not a flood of errors and not a phantom order, but a **silently stale
/// replay** — the A3 resync exists to recover order/trade activity that landed inside a WS
/// reconnect gap, so a dead resync socket means those fills are simply never replayed. Nothing
/// looks wrong; data is just missing. That is why the one latched log line below is load-bearing
/// rather than decoration: before it, this path's failure was invisible end to end (the fetch
/// errors were swallowed by `unwrap_or_else` and never reached an operator at all).
pub struct A3Resync {
    /// OWNED, not shared behind an `Arc` — owning the transport is what licenses re-dialing it,
    /// the same constraint `ReconcileClientDeribit::new`'s doc states for the recon socket.
    rest: VenueRestDeribit,
    symbol: String,
    /// Log latch: ONE line per OUTAGE, not one per attempt. A plain `bool` behind `&mut self`
    /// rather than the recon client's `AtomicBool` — that one is forced by `ReconcileClient`'s
    /// `&self` methods; this type is driven by the single resync supervisor thread through an
    /// `FnMut` closure, so pretending at shared mutability would only hide who mutates it.
    unhealthy: bool,
}

impl A3Resync {
    /// Wrap an already-connected resync `rest`. `symbol` is the engine instrument name, used for
    /// the mapper's fallback symbol and for the log lines.
    pub fn new(rest: VenueRestDeribit, symbol: &str) -> Self {
        A3Resync { rest, symbol: symbol.to_string(), unhealthy: false }
    }

    /// The post-reconnect replay body: recent order history + recent user trades, folded by
    /// [`map_deribit_history`] — the same mapper [`deribit_history_events`] uses, so a replayed
    /// event is byte-identical whichever path fetched it. The two differ ONLY in socket policy.
    ///
    /// Each half is fetched independently: one half failing still replays the other, which is the
    /// pre-existing degradation this keeps.
    pub fn history_events(&mut self) -> Vec<Event> {
        let orders = self.read("order_history", |r| r.get_order_history(RESYNC_HISTORY_LIMIT));
        let trades = self.read("user_trades", |r| r.get_user_trades(RESYNC_HISTORY_LIMIT));
        map_deribit_history(&orders, &trades, "deribit", &self.symbol)
    }

    /// One history read, healed. ONE re-dial and ONE retry — no loop: the pump re-opens (and so
    /// re-fires the resync) on its own reconnect cadence, which IS the retry cadence, so a
    /// genuinely dead venue costs one bounded failure per pass rather than a spin. An empty array
    /// is the failure value, exactly as `unwrap_or_else(|_| json!([]))` gave before.
    fn read(
        &mut self,
        what: &str,
        fetch: impl Fn(&VenueRestDeribit) -> Result<Value, VenueApiError>,
    ) -> Value {
        // ⚠ Bound to a `let` before the match, per this crate's standing rule about the order-WS
        // `Mutex`: a scrutinee's temporaries outlive every arm, and the re-dial below re-locks
        // `self.rest.transport`. `private_result` drops its guard before returning, so this is
        // belt-and-braces — but the arm that would deadlock is the one that only runs on a dying
        // socket, which is precisely where the crate has been bitten before.
        let first = fetch(&self.rest);
        let err = match first {
            Ok(v) => {
                self.mark_healthy();
                return v;
            }
            Err(e) => e,
        };
        self.report_outage(what, &err);
        if !earns_a_redial(&err) {
            // The venue ANSWERED (a JSON-RPC error object) — the socket is alive, and a reconnect
            // would churn the zero-margin credit pool while fixing nothing.
            return json!([]);
        }
        // `connect()` closes the prior socket first, which is what finally drops the dead one.
        // A refused dial leaves the transport socket-LESS, and that state earns a re-dial of its
        // own next pass (`WEBSOCKET_NOT_CONNECTED` is one of `is_dead_socket_error`'s four) — so one
        // unlucky reconnect cannot strand the replay either.
        if self.rest.transport.lock().unwrap().connect().is_err() {
            return json!([]); // still latched: the next pass retries, silently
        }
        match fetch(&self.rest) {
            Ok(v) => {
                self.mark_healthy();
                v
            }
            Err(_) => json!([]),
        }
    }

    /// Announce the transition INTO an unusable replay, exactly once per outage.
    ///
    /// Latched rather than per-attempt for the reason the recon lane learned in production: ONE
    /// dead socket logged 795 identical lines there, and the cure must not reproduce that at a
    /// different layer. Latched rather than SILENT because the alternative is what this path had
    /// — a replay that stops working with no error an operator could ever see.
    fn report_outage(&mut self, what: &str, err: &VenueApiError) {
        if std::mem::replace(&mut self.unhealthy, true) {
            return;
        }
        tracing::warn!(
            target: "vike_deribit::execution",
            symbol = %self.symbol,
            fetch = what,
            code = err.code,
            error = %err.msg,
            re_dialing = earns_a_redial(err),
            "A3 resync history fetch failed — the post-reconnect replay is STALE (fills landing in a reconnect gap go unreplayed); further failures stay silent until it recovers"
        );
    }

    /// Clear the latch, announcing the recovery exactly once.
    fn mark_healthy(&mut self) {
        if std::mem::replace(&mut self.unhealthy, false) {
            tracing::info!(
                target: "vike_deribit::execution",
                symbol = %self.symbol,
                "A3 resync order-WS re-dialed — the post-reconnect replay is live again"
            );
        }
    }
}

/// Spawn the live Deribit exec client (options AND futures/perps, scoped by the symbol's
/// `currency`/`kind`): the shared [`ExecutionClientActor`] itself, with no wrapper of its own. Its exec thread
/// fetches the instrument grid, authenticates the order socket, starts the user-data pump, then drains
/// commands; `submit`/`cancel`/`modify` enqueue onto it (non-blocking) and every venue event returns
/// through the core ingest. Dropping it (or `detach`) stops the order thread, the user-data pump and the
/// funding poll. Deribit adds no venue step to any door, so it takes no `with_*` hook: no `confirm`
/// (the trait's no-op), no adopt, no wire id.
///
/// `symbol` is the Deribit instrument name (e.g. `"BTC-PERPETUAL"` or `"BTC-1JAN27-100000-C"`).
/// The real grid is fetched at startup so any symbol formats correctly; until it loads every order is
/// refused in words and the fetch asked again (`vike_bridge_core::venue_adapter::await_properties`): there is
/// no fallback grid (owner, 2026-11-13). `urls` are the testnet addresses every socket and read of this client
/// dials. An opt-in `properties_rec` records the REAL fetched grid into the PIT properties store, keyed by
/// venue `"deribit"`.
///
/// ⚠ The start hands the client the HALT sentinel it watches with `ExecutionClientActor::with_halt_path`
/// (`AdapterInputs::process.halt_path`); a client built without it watches NOTHING, and
/// `crates/vike-ops/tests/settings_secrets/bridge_inputs_gate.rs` holds that every live start calls it.
pub fn spawn_deribit_execution_client(
    urls: DeribitUrls,
    creds: Credentials,
    symbol: String,
    events: EventSender,
    properties_rec: Option<Arc<vike_data::PropertiesRecorder>>,
) -> ExecutionClientActor {
    let (funding_stop, funding_join) =
        spawn_funding_poll(urls.clone(), creds.clone(), symbol.clone(), events.clone());
    ExecutionClientActor::spawn("deribit-exec", events.clone(), move |rx| {
        run(urls, creds, symbol, events, rx, properties_rec)
    })
    .with_background(funding_stop, funding_join)
}

/// Spawn the funding poll thread ([`crate::execution::funding`]), attached through
/// [`ExecutionClientActor::with_background`]: floored at THIS spawn instant (the restart law), it reads the
/// mounted instrument's row and, for a PERPETUAL (inverse or linear), polls the transaction log of its settle
/// coin every [`FUNDING_POLL_INTERVAL`] until `stop` fires or the core goes away, on an authed socket of its
/// own (the order socket's `Mutex` is never taken). Any other mount returns at once. A failed read warns
/// once per streak (`forward_poll`) and is read again next cadence; a refused dial is re-dialed by the
/// next read (the socket's owner is a [`ReconcileClientDeribit`], whose `call` heals an idempotent read).
///
/// ⚠ One poller per START: two mounts of one deribit account on the same perpetual would each emit its
/// funding. A mount is one instrument of one account today; whoever lifts that dedupes this lane per account.
fn spawn_funding_poll(
    urls: DeribitUrls,
    creds: Credentials,
    symbol: String,
    events: EventSender,
) -> (Arc<AtomicBool>, thread::JoinHandle<()>) {
    let stop = Arc::new(AtomicBool::new(false));
    let stop_thread = stop.clone();
    let join = thread::Builder::new()
        .name("deribit-funding-poll".to_string())
        .spawn(move || {
            let floor = now_ms();
            let currency = loop {
                if stop_thread.load(Ordering::Relaxed) {
                    return;
                }
                match fetch_instrument_row(&urls.rest, &symbol) {
                    Ok(row) => match funding_currency(&row) {
                        Some(currency) => break currency,
                        None => return, // not a perpetual: no funding to read
                    },
                    Err(_) => sleep_unless_stopped(&stop_thread, FUNDING_POLL_INTERVAL),
                }
            };
            let mut transport = DeribitTransport::new(
                &urls.ws,
                creds.api_key.expose(),
                creds.api_secret.expose(),
                None,
            );
            if let Err(e) = transport.connect() {
                tracing::warn!(target: "vike_deribit::execution", %symbol, error = %e, "funding order-WS could not be opened (dial or auth, the error names which); the next poll dials again");
            }
            let reader = ReconcileClientDeribit::new(Arc::new(VenueRestDeribit::new(
                transport,
                &symbol,
                SymbolProperties::default(),
                &currency,
            )));
            let mut poller = DeribitFundingPoller::new(
                |params: &Value| reader.call(PATH_TRANSACTION_LOG, params),
                &symbol,
                &currency,
                floor,
            );
            let mut fail_streak = 0u32;
            while !stop_thread.load(Ordering::Relaxed) {
                match forward_poll(poller.poll(now_ms()), &events, &mut fail_streak) {
                    Err(_core_gone) => return,
                    Ok(Some(e)) => tracing::warn!(
                        target: "vike_deribit::execution",
                        %symbol,
                        error = %e,
                        "funding poll read failed; retrying each cadence (warns once per streak)"
                    ),
                    Ok(None) => {}
                }
                sleep_unless_stopped(&stop_thread, FUNDING_POLL_INTERVAL);
            }
        })
        .expect("spawn deribit-funding-poll thread");
    (stop, join)
}

/// The exec thread: resolve the grid, authenticate the order socket, start the private-WS pump, then
/// drain commands until `Shutdown`. ALL network I/O (fetch, auth, submit/cancel, WS) lives HERE — off
/// the single-writer core thread. On `Shutdown` the pump is stopped + joined before returning.
fn run(
    urls: DeribitUrls,
    creds: Credentials,
    symbol: String,
    events: EventSender,
    rx: Receiver<ExecutionClientActorCommand>,
    properties_rec: Option<Arc<vike_data::PropertiesRecorder>>,
) {
    // ONE read of the mounted instrument's row decides its grid, its settlement currency (the label
    // lookup's scope: `BTC_USDC-PERPETUAL` settles in `USDC`, which `currency_of` cannot say) and its
    // money model (what other instruments this client may also sign). Off the core thread, and until a
    // row with a grid loads every command is refused in words and the read asked again: orders format on
    // the right tick/step or not at all (no fallback grid, owner 2026-11-13).
    // Meanwhile a cancel is SENT (it needs no grid) on an order socket of its own, opened at the first failed
    // read, with no grid and the currency the NAME states (the start's answer wherever the row could not be
    // read, `currency_of`); the client re-dials a dead socket as the trading one does.
    let grid_less = || {
        let mut order_tx = DeribitTransport::new(
            &urls.ws,
            creds.api_key.expose(),
            creds.api_secret.expose(),
            None,
        );
        if let Err(e) = order_tx.connect() {
            tracing::warn!(target: "vike_deribit::execution", %symbol, error = %e, "the order-WS for cancels while the instrument row loads could not be opened; a cancel dials it again");
        }
        VenueRestDeribit::new(order_tx, &symbol, SymbolProperties::default(), &currency_of(&symbol))
    };
    let Some((row, properties)) =
        await_properties("deribit", &symbol, &events, &rx, NEGATIVE_TTL, grid_less, || {
            let row = fetch_instrument_row(&urls.rest, &symbol)?;
            let grid = crate::catalog::properties_of(&row)
                .ok_or_else(|| format!("the row of {symbol} states no tick_size"))?;
            Ok((row, grid))
        })
    else {
        return;
    };
    let currency = settlement_currency_of(&row).unwrap_or_else(|| currency_of(&symbol));
    // The model exists only for a perpetual or dated-future mount: another instrument is signed only
    // beside one.
    let model = MoneyModel::of_mount_row(&row).ok();
    vike_data::PropertiesRecorder::record_opt(
        &properties_rec,
        "deribit",
        &symbol,
        properties,
        now_ns(),
    );

    // Order socket (submit/cancel + the run-thread fill-sentinel history). A failed auth means the
    // thread cannot submit — return so the channel closes and the ExecutionClientActor synthesizes a terminal
    // OrderRejected for every command (the venue-adapter contract: an order never silently vanishes).
    let mut order_tx =
        DeribitTransport::new(&urls.ws, creds.api_key.expose(), creds.api_secret.expose(), None);
    if let Err(e) = order_tx.connect() {
        tracing::error!(target: "vike_deribit::execution", %symbol, error = %e, "order-WS could not be opened (dial or auth, the error names which); exec thread exiting (commands will reject)");
        return;
    }
    let mut rest = VenueRestDeribit::new(order_tx, &symbol, properties, &currency);
    if let Some(model) = model {
        // Any-symbol: an order on another perpetual of the SAME money model is signed on its own
        // instrument and grid (`VenueRestDeribit::with_foreign_instruments`); every other is refused.
        let base = urls.rest.clone();
        rest = rest.with_foreign_instruments(
            model,
            Box::new(move |name| {
                fetch_foreign_instrument_row(&base, name).ok_or_else(|| {
                    format!(
                        "the instrument row of `{name}` could not be read from the testnet (it is \
                         not listed, or the venue could not be reached)"
                    )
                })
            }),
        );
    }

    // A SEPARATE authed socket for the pump's resync supervisor thread — the submit `rest` above lives
    // on THIS thread and can't be shared with the pump. On auth failure the resync closure is inert
    // (returns nothing); the live pump still delivers fills, only the post-reconnect replay is skipped.
    // Once connected it is owned by an `A3Resync`, which re-dials it for the life of the start — see
    // that type's doc for why a plain retry is sound HERE and is not on the exec order socket.
    let mut resync_tx =
        DeribitTransport::new(&urls.ws, creds.api_key.expose(), creds.api_secret.expose(), None);
    let mut resync = match resync_tx.connect() {
        Ok(()) => Some(A3Resync::new(
            VenueRestDeribit::new(resync_tx, &symbol, properties, &currency),
            &symbol,
        )),
        Err(e) => {
            tracing::warn!(target: "vike_deribit::execution", %symbol, error = %e, "resync order-WS could not be opened (dial or auth, the error names which); A3 replay disabled (live fills unaffected)");
            None
        }
    };

    // The fence both threads below share: the pump's fills for an order wait behind that order's own
    // `OrderSubmitted`/`OrderAccepted` (`vike_bridge_core::exec_actor::submit_fence`'s module doc has
    // the race).
    let fence = Arc::new(SubmitFence::new(events.clone()));

    // Private user.trades pump (+ audit-A3 resync): authoritative fills/cancels push into the core
    // ingest through the fence. The pump subscribes the `any`-kind/`any`-currency channel, so it is NOT
    // scoped to this start's symbol/currency — futures AND options fills across every currency stream.
    let feed = spawn_deribit_user_data_with_resync(
        urls.ws.clone(),
        creds.api_key.expose().clone(),
        creds.api_secret.expose().clone(),
        symbol.clone(),
        events.clone(),
        Arc::clone(&fence),
        move || match resync.as_mut() {
            Some(r) => r.history_events(),
            None => Vec::new(),
        },
    );

    // Submit + the gap-sentinel both run on THIS thread, so they share the one order `rest` (two shared
    // borrows, sequential calls — no cross-thread contention). The sentinel replays recent history
    // `DEFAULT_FILL_SENTINEL` after the last command, to recover a fill/cancel the WS lost (the
    // first-connect subscribe race); the core dedups the overlap, so a normal WS delivery makes it a
    // no-op. A venue-free mechanism default.
    let sentinel_symbol = symbol.clone();
    run_loop(
        &VenueRestOrderSubmitFence::new(&rest, &fence),
        &events,
        rx,
        || deribit_history_events(&rest, &sentinel_symbol),
        DEFAULT_FILL_SENTINEL,
    );
    // Shutdown reached: stop + join the pump + resync supervisor, then close the order socket.
    let _ = feed.shutdown();
    rest.detach();
}

#[path = "execution_tests.rs"]
#[cfg(test)]
mod execution_tests;
