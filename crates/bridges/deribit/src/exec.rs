//! Deribit live exec half — `ExecutionClient` over the persistent authed JSON-RPC order WS + the
//! private `user.trades` fill stream. The live twin of `vike_bybit::exec::BybitExecutionClient` (linear
//! perp) and `okx::exec` (SWAP), assembled from the EXISTING deribit parts — NOTHING here is
//! reimplemented, this is the wiring seam:
//!
//! * order entry: [`DeribitRest`](crate::client::DeribitRest) over a persistent authed
//!   [`DeribitOrderTransport`](crate::transport::DeribitOrderTransport) — JSON-RPC `private/buy` /
//!   `private/sell` / `private/cancel` (`DeribitRest::submit_order` / `cancel_order`, the pinned
//!   coin-unit scaling site; `post_only` is forced FALSE there so a marketable order isn't repriced).
//! * fills: [`spawn_deribit_user_data_with_resync`](crate::user_data::spawn_deribit_user_data_with_resync)
//!   — the `user.trades.any.any.raw` pump (all kinds, all currencies → futures AND options fills
//!   stream live on one subscription) + the audit-A3 reconnect resync (recent order/trade history
//!   replayed through a SEPARATE authed socket).
//!
//! Built on the shared [`ExecActor`] scaffold: `submit`/`cancel` enqueue onto ONE dedicated OS thread
//! that drives the authed order socket (blocking, off the single-writer core). `submit_order` emits
//! `[OrderSubmitted, OrderAccepted|OrderRejected]` synchronously; the authoritative async fills/cancels
//! return on the private WS pump straight into the core ingest.
//!
//! LIVE GATE: absent credentials never reach here — the composition root only spawns this when
//! `load_credentials_from` yields `Some` (the codebase's absent-creds-is-the-live-gate rule). With no
//! `.env` keys the venue stays paper. Demo credentials route the TESTNET order/user-data endpoints.
//!
//! DERIBIT SPECIFICS (vs the binance/bybit template):
//! * `currency` (BTC/ETH/SOL) is DERIVED from the symbol for the order-side REST scaling
//!   (`BTC-PERPETUAL` → BTC, `BTC-1JAN27-100000-C` → BTC). The fill stream, however, subscribes the
//!   `any`-kind/`any`-currency `user.trades` channel, so it is NOT scoped to the mount's symbol —
//!   futures AND options fills across every currency stream live. The perp is the simplest live
//!   target; options work the same way.
//! * amount/price are COIN units on the venue's per-instrument tick/step grid — the scaling lives once
//!   in [`crate::client`] (`format_to_step_f`); this seam invents no new scaling.
//! * v1 scope: `submit`/`cancel`. `modify` uses the [`ExecutionClient`] DEFAULT (leaves the resting
//!   order in place): Deribit HAS `private/edit`, but `DeribitRest` does not override `modify_order`
//!   (so it is the `VenueRest` no-op) and the declared caps say `supports_modify == false` — a native
//!   amend is a follow-up (`ExecCommand::Modify` would then route to a new `DeribitRest::edit`). Native
//!   batch is likewise absent (fans out to per-order submits via the `ExecutionClient` default).

use std::sync::Arc;
use std::sync::mpsc::Receiver;
use std::time::Duration;

use serde_json::{Value, json};

use vike_bridge_core::credentials::Credentials;
use vike_bridge_core::exec_actor::{ExecActor, ExecCommand, run_loop};
use vike_bridge_core::json::json_num;
use vike_bridge_core::transport::{RestTransport, UreqTransport, VenueApiError};
use vike_exec::{EventSender, ExecutionClient};
use vike_model::events::Event;
use vike_model::{OrderRequest, SymbolProperties, TickScheme, TickTier, now_ns};

use crate::client::DeribitRest;
use crate::history::map_deribit_history;
// The re-dial POLICY, borrowed rather than re-derived: [`A3Resync`] and
// `crates/bridges/deribit/src/recon_client.rs`'s `DeribitReconClient` are the crate's two
// idempotent-READ socket owners, and a second copy of "which failures a fresh socket might fix"
// would be free to drift from the one the recon lane's incident calibrated. Its doc carries the
// precondition this module has to meet.
use crate::recon_client::earns_a_redial;
use crate::transport::{DeribitOrderTransport, TESTNET_REST, TESTNET_WS};
use crate::user_data::spawn_deribit_user_data_with_resync;

/// Rows of recent order/trade history the audit-A3 resync replays after each WS reconnect.
const RESYNC_HISTORY_LIMIT: u32 = 50;
/// Gap-sentinel: after a submit/cancel, replay recent history this long later to recover a fill/cancel
/// the WS lost (esp. the first-connect subscribe race — a market order fills before the pump is
/// subscribed, and the private WS does not replay on subscribe). Dedup'd by the core, so a normal WS
/// delivery makes it a no-op. Reconnect-driven resync covers resting-order gaps with no recent submit.
const FILL_SENTINEL: Duration = Duration::from_secs(5);

/// Deribit's `currency` (BTC/ETH/SOL) is the token before the first `-` in every instrument name
/// (`BTC-PERPETUAL`, `BTC-1JAN27-100000-C`). Uppercased; empty-safe.
fn currency_of(symbol: &str) -> String {
    symbol.split('-').next().unwrap_or(symbol).to_uppercase()
}

/// Fetch `symbol`'s REAL tick/step grid from the KEYLESS public `public/get_instrument`
/// (`tick_size` + `min_trade_amount` → step = min_qty; no per-instrument max/min-notional, matching
/// the option-chain parser's convention in [`crate::client`]). `None` on any network/parse failure or
/// unknown symbol → the caller uses its fallback grid. Works for BOTH options and futures/perps (same
/// fields), so no new scaling is invented. `creds` is unused (this is a public read) — kept for
/// signature parity with the other venues' fetch helpers.
pub fn fetch_deribit_properties(_creds: &Credentials, symbol: &str) -> Option<SymbolProperties> {
    let resp = UreqTransport::new("deribit")
        .public(
            TESTNET_REST,
            "/api/v2/public/get_instrument",
            &[("instrument_name", symbol.to_string())],
        )
        .ok()?;
    parse_get_instrument(&resp)
}

/// The PURE half of [`fetch_deribit_properties`]: one `public/get_instrument` response envelope →
/// the symbol's grid. Split out from the fetch so the parse (notably `contract_size`, which the
/// order-entry notional cap depends on) is fixture-testable without network.
///
/// This is the venue's PRODUCTION properties writer for EVERY kind — options, futures and perps
/// alike (`run` records exactly this grid into the PIT properties store through
/// `vike_data::PropertiesRecorder::record_opt`), so the class it carries is the class those rows
/// are stored with.
pub fn parse_get_instrument(resp: &serde_json::Value) -> Option<SymbolProperties> {
    let r = resp.get("result")?;
    let tick = r.get("tick_size").and_then(json_num)?;
    let step = r.get("min_trade_amount").and_then(json_num).unwrap_or(0.0);
    let properties = SymbolProperties {
        tick_size: tick,
        step_size: step,
        min_qty: step,
        // `contract_size` is a REAL `public/get_instrument` field — the plural
        // `public/get_instruments` twin in `client.rs::parse_deribit_option_instruments` has parsed
        // it since r6, and `tests/offline/r6_deribit_parity.rs` pins it against the exported fixture. It is
        // what makes a Deribit option's notional `qty * price * contract_size` rather than
        // `qty * price` — the value the order-entry notional cap needs. Absent → 0.0, which
        // `SymbolProperties::multiplier` folds to the inert 1.0.
        contract_size: r.get("contract_size").and_then(json_num).unwrap_or(0.0),
        // The class comes from the venue's own `kind` (+ `settlement_period` for a future), read
        // through the crate's ONE classifier — `crates/bridges/deribit/src/catalog.rs`'s
        // `asset_class_of`, which the plural `public/get_instruments` list has classified on since
        // the catalog was written. Reusing it rather than writing a second judgement here is the
        // point: this singular row and that list describe the SAME instrument, and two copies of
        // the mapping would be free to disagree about it.
        // ⚠ NOT derived from the instrument NAME (`docs/decisions/0061-an-instrument-names-its-kind.md`).
        // A spot pair is `CryptoSpot` (the venue's `kind == "spot"`, classified since 2026-10-04); a
        // row the venue calls `future_combo`/`option_combo`, or one publishing no `kind`, leaves this
        // ABSENT — the grid is still returned and still usable; only the class is withheld, which is
        // the honest answer rather than a guess.
        asset_class: crate::catalog::asset_class_of(r),
        // `tick_size_steps` (the tiered price grid — Deribit options carry it; futures/perps do
        // not) is parsed + attached BELOW via `with_tick_scheme`, now that the #661
        // `kind=properties` codec column persists a scheme through a store round-trip. FRU here
        // still covers max_qty/min_notional (no per-instrument caps) and `taker_hold_ms` (Deribit
        // declares no venue hold), so a new `SymbolProperties` field costs this parser nothing.
        ..Default::default()
    };
    // Attach the tiered grid when the venue reports one; absent/empty/malformed steps leave it
    // scheme-less — byte-identical to before. The scheme's base tick IS `tick` above, so the
    // `SymbolProperties::with_tick_scheme` base-tick invariant holds by construction.
    Some(match parse_tick_scheme(r, tick) {
        Some(scheme) => properties.with_tick_scheme(scheme),
        None => properties,
    })
}

/// Parse Deribit's optional `tick_size_steps` array —
/// `[{"above_price": 0.005, "tick_size": 0.0005}, …]` (options carry it; futures/perps do not) —
/// into a tiered [`TickScheme`] whose base tick IS the instrument's scalar `tick_size`. The equal
/// base tick is what satisfies the [`SymbolProperties::with_tick_scheme`] caller invariant.
///
/// Returns `None` when the array is ABSENT or EMPTY (a flat grid — the scalar `tick_size` IS the
/// whole grid, byte-identical to before), OR when a tier is MALFORMED (a missing/non-finite field,
/// an unsorted or over-long array — all rejected by [`TickScheme::new`]): a bad venue array degrades
/// to the scalar grid (logged) rather than failing the whole grid parse or shipping a zero-tick
/// grid. A present, valid, non-empty array builds the scheme with the tiers in venue order.
fn parse_tick_scheme(result: &serde_json::Value, tick_size: f64) -> Option<TickScheme> {
    let steps = result.get("tick_size_steps")?.as_array()?;
    if steps.is_empty() {
        return None; // an explicit empty array is a flat grid → no scheme (see above)
    }
    // A missing/unparseable tier field becomes NaN, which `TickScheme::new` rejects (BadBoundary /
    // BadTierTick) — so a malformed tier lands in the `Err` arm below, never a silent zero-tick
    // tier. `json_num` decodes both number- and string-encoded values, like the rest of this parse.
    let tiers: Vec<TickTier> = steps
        .iter()
        .map(|s| TickTier {
            above_price: s.get("above_price").and_then(json_num).unwrap_or(f64::NAN),
            tick_size: s.get("tick_size").and_then(json_num).unwrap_or(f64::NAN),
        })
        .collect();
    match TickScheme::new(tick_size, &tiers) {
        Ok(scheme) => Some(scheme),
        Err(e) => {
            tracing::warn!(
                target: "vike_deribit::exec",
                error = %e,
                "malformed deribit tick_size_steps; falling back to the scalar tick grid"
            );
            None
        }
    }
}

/// Fetch + map recent Deribit order/trade history to events — the `run_loop` FILL-SENTINEL's
/// replay body, on the EXEC order `rest`.
///
/// ⚠ **It deliberately does NOT heal a dead socket, and that is the difference between it and
/// [`A3Resync::history_events`].** This runs on the transport that also carries `private/buy` /
/// `private/sell` / `private/cancel`, whose healing policy is already decided — and decided
/// DIFFERENTLY — by `crates/bridges/deribit/src/client.rs`'s `redial_order_socket`, at the order
/// boundary where an ambiguous submit can be re-QUERIED instead of re-sent. Adding a re-dial here
/// would hand that socket a second, unreviewed re-dial site reachable from a timer. A failed
/// sentinel fetch stays an empty replay: the sentinel is a dedup'd safety net over the live pump,
/// never the primary fill path.
fn deribit_history_events(rest: &DeribitRest, symbol: &str) -> Vec<Event> {
    let orders = rest.get_order_history(RESYNC_HISTORY_LIMIT).unwrap_or_else(|_| json!([]));
    let trades = rest.get_user_trades(RESYNC_HISTORY_LIMIT).unwrap_or_else(|_| json!([]));
    map_deribit_history(&orders, &trades, "deribit", symbol)
}

/// The audit-A3 resync's history source: the SECOND authed order-WS (never the exec side's), plus
/// the re-dial that keeps it usable for the life of the mount.
///
/// ## Which cure this is, and why it is the RECON one
///
/// This crate now heals a dead order-WS in three places and the three are NOT interchangeable —
/// `crates/bridges/deribit/CLAUDE.md` carries the split. This is the RECON shape (re-dial **and**
/// re-send), not the exec shape (re-dial, never re-send, re-QUERY instead), and the licence is
/// that every method this type can put on the wire is an idempotent READ. Its socket carries
/// exactly three frames, ever:
///
/// * `public/auth`, from [`DeribitOrderTransport::connect`];
/// * `private/get_order_history_by_instrument` ([`DeribitRest::get_order_history`]);
/// * `private/get_user_trades_by_instrument` ([`DeribitRest::get_user_trades`]).
///
/// None of the three can place, cancel, move or double anything, so re-sending one over a fresh
/// socket is safe in the way a `private/buy` never is. Same fact from the rate limiter's side:
/// `crates/bridges/deribit/src/transport.rs`'s `is_matching_engine` classifies both reads as
/// non-matching-engine, so they draw the read budget rather than the order budget.
///
/// ⚠ **The list above is the whole argument, so it is a list of what this type CAN send, not of
/// what it happens to send today.** Anything that widens it past a read invalidates the cure and
/// makes this the exec path's problem instead.
///
/// ## What it fixes
///
/// [`DeribitOrderTransport`] never re-dials itself and leaves a failed socket in place, and this
/// socket's `connect()` ran exactly once, at mount. A venue-side close therefore disabled the A3
/// replay for the life of the process. Its failure mode is quieter than the recon lane's and than
/// the order lane's: not a flood of errors and not a phantom order, but a **silently stale
/// replay** — the A3 resync exists to recover order/trade activity that landed inside a WS
/// reconnect gap, so a dead resync socket means those fills are simply never replayed. Nothing
/// looks wrong; data is just missing. That is why the one latched log line below is load-bearing
/// rather than decoration: before it, this path's failure was invisible end to end (the fetch
/// errors were swallowed by `unwrap_or_else` and never reached an operator at all).
pub struct A3Resync {
    /// OWNED, not shared behind an `Arc` — owning the transport is what licenses re-dialing it,
    /// the same constraint `DeribitReconClient::new`'s doc states for the recon socket.
    rest: DeribitRest,
    symbol: String,
    /// Log latch: ONE line per OUTAGE, not one per attempt. A plain `bool` behind `&mut self`
    /// rather than the recon client's `AtomicBool` — that one is forced by `ReconClient`'s
    /// `&self` methods; this type is driven by the single resync supervisor thread through an
    /// `FnMut` closure, so pretending at shared mutability would only hide who mutates it.
    unhealthy: bool,
}

impl A3Resync {
    /// Wrap an already-connected resync `rest`. `symbol` is the mounted instrument name, used for
    /// the mapper's fallback symbol and for the log lines.
    pub fn new(rest: DeribitRest, symbol: &str) -> Self {
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
        fetch: impl Fn(&DeribitRest) -> Result<Value, VenueApiError>,
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
        // own next pass (`WS_NOT_CONNECTED` is one of `is_dead_socket_error`'s four) — so one
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
            target: "vike_deribit::exec",
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
                target: "vike_deribit::exec",
                symbol = %self.symbol,
                "A3 resync order-WS re-dialed — the post-reconnect replay is live again"
            );
        }
    }
}

/// Live Deribit exec client (options AND futures/perps, scoped by the symbol's `currency`/`kind`).
/// `submit`/`cancel` enqueue onto the order-WS thread (non-blocking); every venue event returns through
/// the core ingest. Dropping it (or `detach`) stops the order thread AND the user-data pump —
/// deterministic teardown via the owned [`ExecActor`].
pub struct DeribitExecutionClient(ExecActor);

impl DeribitExecutionClient {
    /// Hand this client the HALT sentinel it watches: the venue's mount passes
    /// `MountInputs::process.halt_path`, the one file `vike-mount` resolved for the whole process
    /// (`vike_bridge_core::exec_actor::ExecActor::with_halt_path` carries the contract). A client
    /// built without it watches NOTHING — `crates/vike-ops/tests/settings_secrets/bridge_inputs_gate.rs` holds that
    /// every live mount calls this.
    #[must_use]
    pub fn with_halt_path(self, path: std::path::PathBuf) -> Self {
        Self(self.0.with_halt_path(path))
    }

    /// Spawn the exec thread (fetches the instrument grid, authenticates the order socket, starts the
    /// user-data pump, then drains commands). `symbol` is the Deribit instrument name (e.g.
    /// `"BTC-PERPETUAL"` or `"BTC-1JAN27-100000-C"`). `fallback_properties` is used ONLY if the startup
    /// `public/get_instrument` fetch fails — the real grid is fetched at startup so any symbol formats
    /// correctly.
    pub fn spawn(
        creds: Credentials,
        symbol: String,
        fallback_properties: SymbolProperties,
        events: EventSender,
    ) -> Self {
        Self::spawn_with_recorder(creds, symbol, fallback_properties, events, None)
    }

    /// Same as [`Self::spawn`], plus an opt-in `properties_rec`: when present, the REAL fetched
    /// instrument grid (never the fallback) is recorded into the PIT properties store right after the
    /// startup `get_instrument` fetch resolves, keyed by venue `"deribit"`. `properties_rec` is moved
    /// into the exec thread (`PropertiesRecorder` is `Send + Sync`).
    pub fn spawn_with_recorder(
        creds: Credentials,
        symbol: String,
        fallback_properties: SymbolProperties,
        events: EventSender,
        properties_rec: Option<Arc<vike_data::PropertiesRecorder>>,
    ) -> Self {
        let actor = ExecActor::spawn("deribit-exec", events.clone(), move |rx| {
            run(creds, symbol, fallback_properties, events, rx, properties_rec)
        });
        Self(actor)
    }
}

impl ExecutionClient for DeribitExecutionClient {
    fn submit(&mut self, request: &OrderRequest) {
        self.0.submit(request)
    }
    fn cancel(&mut self, client_order_id: &str) {
        self.0.cancel(client_order_id)
    }
    fn modify(&mut self, order: &OrderRequest, new_qty: Option<f64>, new_price: Option<f64>) {
        self.0.modify(order, new_qty, new_price)
    }
    fn confirm(&mut self, client_order_id: &str) {
        self.0.confirm(client_order_id)
    }
    /// Phase-one teardown seam — raise the stop flags, join nothing. See
    /// `crates/vike-exec/src/execution_engine/client.rs`'s `ExecutionClient::begin_detach`.
    /// ⚠ It MUST be delegated like every other method on this wrapper: a newtype that omits it
    /// inherits the trait's no-op, and the core's raise-all phase then skips this venue entirely
    /// while `detach` below still pays its full wind-down.
    fn begin_detach(&mut self) {
        self.0.begin_detach()
    }
    fn detach(&mut self) {
        self.0.detach()
    }
}

/// The exec thread: resolve the grid, authenticate the order socket, start the private-WS pump, then
/// drain commands until `Shutdown`. ALL network I/O (fetch, auth, submit/cancel, WS) lives HERE — off
/// the single-writer core thread. On `Shutdown` the pump is stopped + joined before returning.
fn run(
    creds: Credentials,
    symbol: String,
    fallback_properties: SymbolProperties,
    events: EventSender,
    rx: Receiver<ExecCommand>,
    properties_rec: Option<Arc<vike_data::PropertiesRecorder>>,
) {
    let currency = currency_of(&symbol);
    // Fetch the venue's REAL instrument grid for `symbol` (off the core thread); fall back to the
    // caller's default only if the fetch fails, so orders format on the right tick/step. Only the REAL
    // fetched grid is ever recorded into the PIT properties store (never the fallback).
    let properties = match fetch_deribit_properties(&creds, &symbol) {
        Some(real) => {
            vike_data::PropertiesRecorder::record_opt(
                &properties_rec,
                "deribit",
                &symbol,
                real,
                now_ns(),
            );
            real
        }
        None => {
            tracing::warn!(target: "vike_deribit::exec", %symbol, "get_instrument fetch failed; using fallback properties");
            fallback_properties
        }
    };

    // Order socket (submit/cancel + the run-thread fill-sentinel history). A failed auth means the
    // thread cannot submit — return so the channel closes and the ExecActor synthesizes a terminal
    // OrderRejected for every command (the venue-adapter contract: an order never silently vanishes).
    let mut order_tx =
        DeribitOrderTransport::new(TESTNET_WS, &creds.api_key, &creds.api_secret, None);
    if let Err(e) = order_tx.connect() {
        tracing::error!(target: "vike_deribit::exec", %symbol, error = %e, "order-WS auth failed; exec thread exiting (commands will reject)");
        return;
    }
    let rest = DeribitRest::new(order_tx, &symbol, properties, &currency);

    // A SEPARATE authed socket for the pump's resync supervisor thread — the submit `rest` above lives
    // on THIS thread and can't be shared with the pump. On auth failure the resync closure is inert
    // (returns nothing); the live pump still delivers fills, only the post-reconnect replay is skipped.
    // Once connected it is owned by an `A3Resync`, which re-dials it for the life of the mount — see
    // that type's doc for why a plain retry is sound HERE and is not on the exec order socket.
    let mut resync_tx =
        DeribitOrderTransport::new(TESTNET_WS, &creds.api_key, &creds.api_secret, None);
    let mut resync = match resync_tx.connect() {
        Ok(()) => Some(A3Resync::new(
            DeribitRest::new(resync_tx, &symbol, properties, &currency),
            &symbol,
        )),
        Err(e) => {
            tracing::warn!(target: "vike_deribit::exec", %symbol, error = %e, "resync order-WS auth failed; A3 replay disabled (live fills unaffected)");
            None
        }
    };

    // Private user.trades pump (+ audit-A3 resync): authoritative fills/cancels push straight into
    // the core ingest. The pump subscribes the `any`-kind/`any`-currency channel, so it is NOT
    // scoped to this mount's symbol/currency — futures AND options fills across every currency stream.
    let feed = spawn_deribit_user_data_with_resync(
        TESTNET_WS.to_string(),
        creds.api_key.clone(),
        creds.api_secret.clone(),
        symbol.clone(),
        events.clone(),
        move || match resync.as_mut() {
            Some(r) => r.history_events(),
            None => Vec::new(),
        },
    );

    // Submit + the gap-sentinel both run on THIS thread, so they share the one order `rest` (two shared
    // borrows, sequential calls — no cross-thread contention).
    let sentinel_symbol = symbol.clone();
    run_loop(&rest, &events, rx, || deribit_history_events(&rest, &sentinel_symbol), FILL_SENTINEL);
    // Shutdown reached: stop + join the pump + resync supervisor, then close the order socket.
    let _ = feed.shutdown();
    rest.detach();
}

#[path = "exec_tests.rs"]
#[cfg(test)]
mod exec_tests;
