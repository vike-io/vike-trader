//! Bybit live exec half — `ExecutionClient` over the V5 linear-perp REST + the private user-data WS.
//!
//! Built on the shared [`ExecActor`] scaffold: `submit`/`cancel` enqueue onto ONE dedicated OS thread
//! that drives [`BybitPerpRest`] (blocking REST off the single-writer core). `submit_order` emits
//! `[OrderSubmitted, OrderAccepted|OrderRejected]` synchronously; the authoritative async fills/cancels
//! return on the private WS via [`crate::user_data::spawn_bybit_perp_user_data`] straight into the core ingest.
//!
//! LIVE GATE: absent credentials never reach here — the start only spawns this when
//! `vike_secrets::credentials::api_credentials` yields `Some` for the started account (the
//! codebase's absent-creds-is-the-live-gate rule). An account with no keys does not run.
//!
//! Scope: `submit`/`cancel` (the DOM's click-to-place / cancel surface) and a native `modify`
//! (`ExecCommand::Modify` → `rest.modify_order` in the shared `run_loop`;
//! `crates/bridges/bybit/src/lib.rs`'s `caps_test` ties `supports_modify` to it). Batch is NOT
//! wired at the client seam: it falls through to the [`ExecutionClient`] fan-out default
//! (`supports_native_batch` is false) — a native batch is a follow-up (`BybitPerpRest` already has
//! `submit_batch`, but this client forwards no batch lane to it).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::Duration;
use vike_secrets::credentials::Credentials;

use vike_bridge_core::confirm::{DispatchLedger, Dispatched, confirm_closure};
use vike_bridge_core::exec_actor::{
    ConfirmFn, DEFAULT_FILL_SENTINEL, ExecActor, ExecCommand, run_loop,
};
use vike_bridge_core::ratelimit::RateGate;
use vike_bridge_core::rest::VenueRest;
use vike_bridge_core::signer::BybitV5Signer;
use vike_bridge_core::user_data::sleep_unless_stopped;
use vike_exec::{CoreGone, EventSender, ExecutionClient};
use vike_model::events::Event;
use vike_model::{OrderRequest, SymbolProperties};

use crate::funding::BybitFundingPoller;
use crate::per_symbol::{ClientFactory, CoidSymbols, PerSymbolRest};
use crate::perp::{BybitPerpRest, PATH_INSTRUMENTS, endpoints, parse_bybit_perp_instruments};
use crate::transport::{BybitTransport, UreqBybitTransport};
use crate::user_data::spawn_bybit_perp_user_data_with_resync;

/// FALLBACK cross-margin leverage the exec thread sets once at start — the literal this arm posted
/// to `/v5/position/set-leverage` unconditionally before the operator's budget was threaded in
/// (demo default; matches the smoke tests). Used ONLY when the operator expressed no leverage
/// intent, so a start with no `[risk] max_leverage` is byte-identical to every start before
/// [`leverage_for`] existed. NEVER read on the submit path — `BybitPerpRest::leverage` is consulted
/// by `set_leverage` and nothing else, which is why the read-only clients below (funding poller,
/// confirm re-query) can carry it inertly.
const DEFAULT_LEVERAGE: f64 = 2.0;
/// Rows of recent order/execution history the audit-A3 resync replays after each WS reconnect.
const RESYNC_HISTORY_LIMIT: u32 = 50;
/// Funding-settlement poll cadence (REST `/v5/account/transaction-log`; funding is NOT on the
/// private WS — see `crate::funding`'s module doc for the sign trap that rules that channel out).
/// Settlements post at most every 8h, so 60s keeps live equity current without hammering the
/// endpoint.
const FUNDING_POLL_INTERVAL: Duration = Duration::from_secs(60);

/// The account leverage this arm POSTs at startup, resolved from the operator's `[risk]` budget.
///
/// Pure, and `pub` on purpose — the SAME shape as the start's ceiling (decision 0095):
/// `crate::adapter`'s `BybitVenueAdapter::start` is handed the `ProfileRisk` and calls this ONCE per
/// start, then threads the resolved `f64` into [`BybitExecutionClient::spawn_with_recorder`]
/// (docs/decisions/0096 moved that call out of `vike_tradehub_core::make_engine`). Nothing inside the
/// adapter reaches for the profile, so the number the venue account is configured with and the
/// number the RiskGate sizes against (`ProfileRisk::im_requirement`, `im = 1.0 / max_leverage`)
/// can never disagree — that disagreement was the bug this exists to close.
///
/// Unset (`None`, or a profile that never mentions `max_leverage`) ⇒ [`DEFAULT_LEVERAGE`],
/// byte-identical to before. See `vike_bridge_core::leverage` for the full rule.
pub fn leverage_for(profile: Option<&vike_model::ProfileRisk>) -> f64 {
    vike_bridge_core::leverage::resolve_startup_leverage(profile, DEFAULT_LEVERAGE)
}

/// Fetch + map recent order/execution history to events (the audit-A3 replay body). Shared by the
/// pump's reconnect-resync AND the `run_loop` fill-sentinel; both feed the same dedup'd core ingest.
fn bybit_history_events(rest: &BybitPerpRest<UreqBybitTransport>, symbol: &str) -> Vec<Event> {
    let orders =
        rest.get_order_history(RESYNC_HISTORY_LIMIT).unwrap_or_else(|_| serde_json::json!([]));
    let execs =
        rest.get_execution_history(RESYNC_HISTORY_LIMIT).unwrap_or_else(|_| serde_json::json!([]));
    crate::history::map_bybit_history(&orders, &execs, "bybit", symbol)
}

// Wall-clock helpers: the shared `vike_model::{now_ms, now_ns}` (a plain fn item, so it still
// coerces to the `Fn() -> i64 + Send + Sync + 'static` bound `BybitV5Signer::new` wants).
use vike_model::{now_ms, now_ns};

/// Fetch `symbol`'s REAL tick/step/min-qty/min-notional grid from Bybit `instruments-info` (linear
/// category). `None` on any network/parse failure or unknown symbol → the caller uses its fallback.
/// So the adapter formats orders on the correct grid for ANY symbol, not just the demo-BTC default.
///
/// `mainnet` is the already-resolved tier verdict the start threaded in (decision 0095:
/// `crate::adapter`'s `BybitVenueAdapter::start` takes it ONCE from the tier `vike-tradehub-core` resolves
/// from the account row) — never re-read here, so the grid host can never disagree with the
/// exec host.
pub fn fetch_bybit_properties(
    creds: &Credentials,
    symbol: &str,
    mainnet: bool,
) -> Option<SymbolProperties> {
    fetch_bybit_properties_with_cap(creds, symbol, mainnet).map(|(properties, _cap)| properties)
}

/// [`fetch_bybit_properties`] plus the venue's published account-leverage CEILING for `symbol`
/// (`leverageFilter.maxLeverage`) — the SAME single `instruments-info` request, since both fields
/// ride the same row. `None` for the cap means the row carried no usable `leverageFilter`; the
/// outer `None` still means the fetch/parse failed entirely.
///
/// This exists rather than widening [`fetch_bybit_properties`]'s return type so every existing
/// caller (the exec thread's own grid fetch, `crate::adapter`'s `RiskLimits` pre-fetch, the live
/// smokes) is untouched, and so the cap costs no second round-trip anywhere. Consumed by
/// [`run`] via `vike_bridge_core::leverage::clamp_to_venue_cap`.
pub fn fetch_bybit_properties_with_cap(
    creds: &Credentials,
    symbol: &str,
    mainnet: bool,
) -> Option<(SymbolProperties, Option<f64>)> {
    fetch_bybit_instrument(creds, symbol, mainnet).map(|i| (i.properties, i.max_leverage))
}

/// The WHOLE `instruments-info` row for `symbol` — the grid, the leverage ceiling, and the venue's
/// own `contractType`. `None` on any network/parse failure or unknown symbol.
///
/// [`fetch_bybit_properties`] and [`fetch_bybit_properties_with_cap`] are projections of this, kept
/// at their original signatures so no caller churned; this is the shape the exec thread needs,
/// because the refusal it owes ([`non_linear_perpetual_refusal`]) is answered by a field those two
/// throw away. ONE request either way.
///
/// ⚠ **`category: "linear"` is still a literal here, and that is what the refusal is FOR.** MEASURED
/// 2026-09-16: this endpoint ANSWERS for an inverse symbol asked of the linear category —
/// `?category=linear&symbol=XRPUSD` returns the real `InversePerpetual` row — so the fetch cannot
/// fail its way out of the problem. It is the CALLER that must refuse, on the word this row carries.
pub fn fetch_bybit_instrument(
    creds: &Credentials,
    symbol: &str,
    mainnet: bool,
) -> Option<crate::perp::BybitInstrument> {
    // Grid host follows the same demo/mainnet switch the exec path uses so a mainnet start
    // sizes/prices on the mainnet grid; `false` ⇒ demo host (byte-identical).
    let (rest_base, _) = endpoints(mainnet);
    let info = UreqBybitTransport::new()
        .signed(
            rest_base,
            PATH_INSTRUMENTS,
            "GET",
            &[("category", serde_json::json!("linear")), ("symbol", serde_json::json!(symbol))],
            &BybitV5Signer::new(creds, now_ms),
        )
        .ok()?;
    parse_bybit_perp_instruments(&info).get(symbol).cloned()
}

/// The venue's own `contractType` for this venue's LINEAR-only exec plane: `None` when the symbol
/// may be traded, `Some(message)` when it may not.
///
/// # ⚠ Why a refusal and not a route
///
/// `docs/decisions/0061-an-instrument-names-its-kind.md` phase 4 made the DATA plane address
/// bybit's inverse book — the catalog offers those instruments, they chart, they backfill. The EXEC
/// plane did not move: `crates/bridges/bybit/src/perp.rs` spells `category: "linear"` as a literal at
/// ten signed sites, `recon_client.rs` at four more, and `funding.rs` at one. Threading a category
/// through all of them is a change whose blast radius is an order on a live account, and it needs
/// the demo probes this branch did not run — every one of those endpoints is SIGNED, so the
/// kline endpoint's measured leniency says nothing about them.
///
/// What is NOT acceptable is the state that leaves behind if nothing refuses, because today's
/// default is not a refusal — it is a wrong-grid ACCEPTANCE:
///
///   1. `instruments-info?category=linear&symbol=<inverse>` SUCCEEDS (MEASURED 2026-09-16) and
///      returns the real coin-settled row;
///   2. `parse_bybit_perp_instruments` gates on nothing, so that row becomes a `SymbolProperties`
///      and a `RiskLimits`;
///   3. `contract_size` stays unset (its comment: *"Bybit LINEAR perps are 1:1 with the base
///      asset"*), so `SymbolProperties::multiplier()` folds to 1.0 while an inverse contract's qty
///      is USD NOTIONAL and its value is `qty / price` in the settle coin.
///
/// So the order formats on a real grid, for the right contract, under a category label the venue
/// resolves leniently, with every notional the gate reasons about wrong by roughly the price. That
/// is the arm that does not fail loudly, and this function is what makes it fail loudly.
///
/// Pure: the caller supplies the word the venue said. An ABSENT `contractType` is NOT refused —
/// that is the shape of a row the venue changed or a fetch that failed, and refusing every order on
/// a missing field would take a working linear start off the venue for a parse miss. The fetch
/// failing already falls back to the caller's default grid, exactly as before.
#[must_use]
pub fn non_linear_perpetual_refusal(symbol: &str, contract_type: Option<&str>) -> Option<String> {
    match contract_type {
        None | Some("LinearPerpetual") => None,
        Some(other) => Some(format!(
            "bybit exec REFUSES {symbol:?}: the venue reports contractType {other:?}, and this \
             adapter's order, reconcile and funding paths all spell `category: \"linear\"` as a \
             literal. Signing here would send an order naming a category this symbol does not \
             trade in, on a grid whose quantity unit this adapter reads as base asset when the \
             venue means USD notional — so nothing is being sent. The market-data side of this \
             instrument works: it charts and backfills (`category=inverse`). Trading it needs the \
             exec category threaded through `crates/bridges/bybit/src/perp.rs`, \
             `recon_client.rs` and `funding.rs`, plus `contract_size` populated for coin-settled \
             contracts, behind demo smokes."
        )),
    }
}

/// **The exec thread when [`non_linear_perpetual_refusal`] fired** — a [`VenueRest`] that never
/// touches the wire and never lets an order vanish.
///
/// It is the whole refusal mechanism, and it is a `VenueRest` rather than a bespoke loop on
/// purpose: [`run_loop`] already owns the command protocol (the emitter split, the terminal-event
/// contract, the shutdown), so the refusing path and the trading path cannot drift about what a
/// submitted order is owed. No REST client is built, no `set_leverage` is posted, and no private WS
/// is opened — the process authenticates as this account for exec at all.
struct RefusingRest {
    /// The refusal text, repeated verbatim on every rejected order so an operator reading one
    /// event sees the same sentence the startup log carried.
    why: String,
}

impl VenueRest for RefusingRest {
    /// The emitter split, kept: `OrderSubmitted` synchronously, then a TERMINAL `OrderRejected`.
    /// *"No order may silently vanish — a dead venue path must synthesize a terminal
    /// `OrderRejected`"* is the venue-adapter contract, and this is the deadest possible venue path.
    fn submit_order(&self, request: &OrderRequest) -> Vec<Event> {
        vec![
            Event::OrderSubmitted(vike_model::events::OrderSubmitted {
                client_order_id: request.client_order_id.clone(),
                ts: request.ts,
            }),
            Event::OrderRejected(vike_model::events::OrderRejected {
                client_order_id: request.client_order_id.clone(),
                reason: self.why.clone().into(),
                ts: request.ts,
            }),
        ]
    }

    /// `Ok(())` — "unknown ≠ rejection" is this trait's cancel rule, and nothing was ever resting.
    /// Cancelling an order that was rejected at submit must not report a second failure.
    fn cancel_order(
        &self,
        _client_order_id: &str,
    ) -> Result<(), vike_bridge_core::transport::VenueApiError> {
        Ok(())
    }
}

/// One funding-poll iteration: pull new settlements and forward them to the core ingest. `Err`
/// only when the core is gone (the ingest receiver dropped) — the caller's loop then exits
/// instead of polling forever into a void, mirroring every other pump thread's self-exit.
///
/// A fetch failure is retried next cadence and warned ONCE per consecutive-failure streak
/// (`fail_streak` counts it; a success resets it) — a 60s background thread, never the hot fold,
/// so one warn per streak is both visible and bounded.
fn poll_funding_once<T: BybitTransport>(
    poller: &mut BybitFundingPoller<'_, T>,
    events: &EventSender,
    fail_streak: &mut u32,
) -> Result<(), CoreGone> {
    match poller.poll() {
        Err(e) => {
            *fail_streak += 1;
            if *fail_streak == 1 {
                tracing::warn!(
                    target: "vike_bybit",
                    error = %e,
                    "funding poll fetch failed; retrying each cadence (warns once per streak)"
                );
            }
        }
        Ok(evs) => {
            *fail_streak = 0;
            for ev in evs {
                events.blocking_send(ev)?;
            }
        }
    }
    Ok(())
}

/// Spawn the funding-settlement background poll thread (attached via
/// [`vike_bridge_core::exec_actor::ExecActor::with_background`]): its OWN REST client (never
/// shares the submit/resync/sentinel clients on the command thread), polling
/// [`FUNDING_POLL_INTERVAL`] until `stop` fires or the core goes away. `properties` is unused by
/// the funding GET (no order formatting), so a default is fine.
///
/// The poller is floored at THIS spawn instant (`now_ms()`): settlements that posted before the
/// start are already inside the authoritative venue balance the account seeds from, so emitting
/// them would double-count — see `crate::funding`'s module doc (the restart law).
///
/// CROSS-MOUNT HAZARD (for a future multi-mount-per-venue-account world): the transaction-log
/// query is ACCOUNT-WIDE (no `symbol` param — deliberate, so every engine symbol's settlement
/// reaches the fold). If several bybit perp starts ever share one venue account, EACH start's
/// poller would emit EVERY symbol's settlements → N-fold duplication through the non-idempotent
/// `apply_funding`. Whoever adds multi-mount must dedupe this lane account-wide (one poller per
/// account, not per start).
fn spawn_funding_poll(
    creds: Credentials,
    symbol: String,
    events: EventSender,
    mainnet: bool,
) -> (Arc<AtomicBool>, thread::JoinHandle<()>) {
    let stop = Arc::new(AtomicBool::new(false));
    let stop_thread = stop.clone();
    let join = thread::Builder::new()
        .name("bybit-funding-poll".to_string())
        .spawn(move || {
            let rest = BybitPerpRest {
                signer: BybitV5Signer::new(&creds, now_ms),
                transport: UreqBybitTransport::new(),
                // The start's already-resolved mainnet verdict (decision 0095:
                // `AdapterInputs::tier`, as `BybitVenueAdapter::start` reads it), captured
                // at spawn — this thread never re-resolves it, so it can never poll a different
                // network than the exec thread trades on.
                base_url: endpoints(mainnet).0.to_string(),
                symbol: symbol.clone(),
                properties: SymbolProperties::default(),
                // INERT here: this client only GETs `/v5/account/transaction-log` and never calls
                // `set_leverage` (the field's sole reader), so the fallback is correct by
                // construction — the operator's value rides the exec thread's client below.
                leverage: DEFAULT_LEVERAGE,
            };
            // Floor = spawn instant: only settlements landing WHILE started are folded.
            let mut poller = BybitFundingPoller::new(&rest, &symbol, now_ms());
            let mut fail_streak = 0u32;
            while !stop_thread.load(Ordering::Relaxed) {
                if poll_funding_once(&mut poller, &events, &mut fail_streak).is_err() {
                    return; // core gone — self-exit like every other pump thread
                }
                sleep_unless_stopped(&stop_thread, FUNDING_POLL_INTERVAL);
            }
        })
        .expect("spawn bybit-funding-poll thread");
    (stop, join)
}

/// Live Bybit linear-perp exec client. `submit`/`cancel` enqueue onto the REST thread (non-blocking);
/// every venue event returns through the core ingest. Dropping it (or `detach`) stops the REST thread
/// AND the user-data pump — deterministic teardown via the owned [`ExecActor`].
pub struct BybitExecutionClient(ExecActor, Arc<CoidSymbols>);

impl BybitExecutionClient {
    /// Hand this client the HALT sentinel it watches: the venue's start passes
    /// `AdapterInputs::process.halt_path`, the one file `vike-tradehub-core` resolved for the whole process
    /// (`vike_bridge_core::exec_actor::ExecActor::with_halt_path` carries the contract). A client
    /// built without it watches NOTHING — `crates/vike-ops/tests/settings_secrets/bridge_inputs_gate.rs` holds that
    /// every live start calls this.
    #[must_use]
    pub fn with_halt_path(self, path: std::path::PathBuf) -> Self {
        Self(self.0.with_halt_path(path), self.1)
    }

    /// Spawn the exec thread (which fetches the instrument grid, sets leverage, starts the user-data
    /// pump, then drains commands). `symbol` is the V5 linear instrument (e.g. `"BTCUSDT"`);
    /// `fallback_properties` is used ONLY if the exec thread's `instruments-info` fetch fails (a
    /// demo-BTC default) — the real grid is fetched at startup so any symbol formats correctly.
    ///
    /// `mainnet` is the already-resolved ceiling verdict (decision 0095: the start's ceiling
    /// chooses the network); `false` binds the demo host pair — the explicit, self-documenting
    /// default for a smoke/test caller, which no environment variable can silently upgrade.
    pub fn spawn(
        creds: Credentials,
        symbol: String,
        fallback_properties: SymbolProperties,
        events: EventSender,
        mainnet: bool,
    ) -> Self {
        Self::spawn_with_recorder(
            creds,
            symbol,
            fallback_properties,
            events,
            None,
            None,
            None,
            mainnet,
            DEFAULT_LEVERAGE,
            false,
        )
    }

    /// Same as [`Self::spawn`], plus an opt-in `properties_rec`: when present, the REAL fetched
    /// instrument grid (never the fallback) is recorded into the PIT properties store right after the
    /// startup `instruments-info` fetch resolves. `properties_rec` is moved into the exec thread
    /// (`PropertiesRecorder` is `Send + Sync`).
    ///
    /// `on_reconcile` (reconciliation-activation Task 7) is threaded straight down to the venue's
    /// `run_resync_supervisor` call — `Some` fires the recon trigger after every reconnect's
    /// event-replay settles; `None` (every non-recon caller, e.g. [`Self::spawn`]) reproduces the
    /// pre-Task-7 event-replay-only behavior byte-for-byte.
    ///
    /// `broker_id` (unified cross-venue attribution, task 5) is the FD-broker code resolved ONCE at
    /// the start site (`crate::adapter`'s `BybitVenueAdapter::start`, via `attribution_code_from` over
    /// the credential map); threaded down to the ONE `BybitPerpRest`
    /// signer actually used for `submit_order` (`run`'s `rest`), so every order-create request carries
    /// `X-Referer: <id>`. `None` (every non-recon caller, e.g. [`Self::spawn`]) is byte-identical.
    ///
    /// `mainnet` (decision 0095: the start's ceiling chooses the network) is likewise resolved ONCE
    /// at the start site and threaded down to EVERY host selection this client makes — the confirm
    /// re-query client, the funding poller and the exec thread's three REST clients + user-data WS.
    /// No thread below re-reads the flag, so one start can never end up signing mainnet credentials
    /// against demo hosts. `false` ⇒ the demo pair, byte-identical to before this switch existed.
    ///
    /// `leverage` is the account leverage the exec thread posts once at start
    /// (`POST /v5/position/set-leverage`, buy == sell), resolved ONCE at the start site from the
    /// operator's `[risk]` budget via [`leverage_for`] — so the venue account and the RiskGate's
    /// margin math run on the SAME number. Pass [`DEFAULT_LEVERAGE`] (what [`Self::spawn`] does)
    /// for the historical literal.
    ///
    /// `fast_exec` is the `venue.bybit.fast_exec` setting, read once by the start through
    /// [`crate::user_data::fast_exec`] and handed to the private-socket pump. [`Self::spawn`]
    /// passes `false`.
    pub fn spawn_with_recorder(
        creds: Credentials,
        symbol: String,
        fallback_properties: SymbolProperties,
        events: EventSender,
        properties_rec: Option<Arc<vike_data::PropertiesRecorder>>,
        on_reconcile: Option<mpsc::Sender<()>>,
        broker_id: Option<String>,
        mainnet: bool,
        leverage: f64,
        fast_exec: bool,
    ) -> Self {
        // Active-confirm re-query (audit ex1 residual), in the ONE shared form of
        // `vike_bridge_core::confirm` (the form the proposed decision 0171 describes): the ExecActor runs the closure on its OWN worker
        // thread when the core issues `Command::ConfirmOrder`, so a wedged adapter is prodded with NO
        // network on the core fold. ONE REST client serves every confirm (bounded, backed off per coid),
        // over the exact ambiguous-submit re-query. `ledger` is stamped by `Dispatched` around the
        // command loop's client in `run`: "absent" rejects only a coid whose submit has FINISHED, and a
        // failed query says nothing. The status GET is keyed on orderLinkId only, so
        // `fallback_properties` (unused by the GET) is fine.
        // The GET also names the order's SYMBOL: the one client is a `PerSymbolRest`, so a coid the
        // command loop recorded as a foreign order (`coids`, shared with the exec thread's router)
        // is asked about on ITS symbol, any other coid on the mounted one. A foreign-symbol client
        // here reads no grid (a status GET never formats a price), so building one costs no network.
        let ledger = Arc::new(DispatchLedger::default());
        let coids = Arc::new(CoidSymbols::default());
        let adopt_coids = coids.clone();
        let confirm_rest = move |creds: &Credentials, symbol: String| BybitPerpRest {
            signer: BybitV5Signer::new(creds, now_ms),
            transport: UreqBybitTransport::new(),
            // The start's resolved verdict, captured — never re-read on this worker.
            base_url: endpoints(mainnet).0.to_string(),
            symbol,
            properties: fallback_properties,
            // INERT here: a status re-query never calls `set_leverage` (the field's sole
            // reader), so the fallback is correct by construction.
            leverage: DEFAULT_LEVERAGE,
        };
        let foreign_creds = creds.clone();
        let make: ClientFactory<BybitPerpRest<UreqBybitTransport>> =
            Box::new(move |label| Ok(confirm_rest(&foreign_creds, label.to_uppercase())));
        let confirm: ConfirmFn = confirm_closure(
            PerSymbolRest::new(
                symbol.clone(),
                confirm_rest(&creds, symbol.clone()),
                make,
                coids.clone(),
            ),
            ledger.clone(),
            |_| true,
        );
        // Funding-settlement poll: perp-only by construction (this crate's ONLY exec client is the
        // linear-perp one — spot is deferred, see the crate doc), so spawning it here is inherently
        // scoped to perp starts.
        let (funding_stop, funding_join) =
            spawn_funding_poll(creds.clone(), symbol.clone(), events.clone(), mainnet);
        let actor = ExecActor::spawn("bybit-exec", events.clone(), move |rx| {
            run(
                creds,
                symbol,
                fallback_properties,
                events,
                rx,
                properties_rec,
                on_reconcile,
                broker_id,
                mainnet,
                leverage,
                fast_exec,
                ledger,
                coids,
            )
        })
        .with_confirm(confirm)
        .with_background(funding_stop, funding_join);
        Self(actor, adopt_coids)
    }
}

impl ExecutionClient for BybitExecutionClient {
    /// Teach the router which symbol each restored order rests on (the core announces them at boot
    /// and after a gatewayhub restart): a cancel of an order this process never submitted would
    /// otherwise go to the MOUNTED symbol, where the venue answers "unknown order" and the adapter
    /// reports a LIVE order cancelled.
    fn adopt_restored(&mut self, orders: &[(String, String)]) {
        for (coid, symbol) in orders {
            if !symbol.is_empty() {
                self.1.record(coid, symbol);
            }
        }
    }

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

/// The exec thread's SUBMIT client — the one [`BybitPerpRest`] that places, cancels, amends and
/// sets leverage (`run_loop`'s `rest`), and so the ONE bybit transport on the exec side that rides
/// the order-rate gate ([`crate::ratelimit::rest_rate_gate`], the `BYBIT.orders` row: 18 per 1 s,
/// ~10% under the venue's 20/s per-UID create/cancel budget).
///
/// ⚠ **A gate that is not built here is not on the wire.** Every smoke attaches the gate to its own
/// transport, so the gate was green everywhere a test looked and absent from the one place orders
/// leave the process: a runaway submit/cancel loop (a requote storm, a mass-cancel) ran at whatever
/// pace the blocking transport allowed, straight into the venue's per-UID limit and then its
/// 10-minute IP ban — which answers HTTP 403 and so reads in code like a bad key. Below the budget
/// the gate admits at once and delays nothing; it only holds back what the venue would refuse.
///
/// **Why only this client, and why a window of its own:**
/// - The resync, fill-sentinel and confirm clients below speak only `signed_requery`, which is
///   rate-gate-EXEMPT by design (a throttle would reintroduce the stall its short-timeout agent
///   bounds), so a gate on them would be inert; they stay as they were.
/// - The funding poller (one `signed` GET a minute) and the grid fetch (one at adapter start, one at venue start)
///   are reads on endpoints of their own: they draw none of the create/cancel budget, so they do
///   not share this window.
/// - The reconcile client carries ITS OWN window from the same factory
///   (`crate::recon_client::recon_client`). They are not merged on purpose: a reconcile burst
///   queued ahead of a cancel in one FIFO window is the order-path delay the exempt `signed_requery`
///   split exists to prevent.
/// - What this does NOT do is aggregate across starts: the gate is per start (per symbol), so two
///   starts on one account each have their own 18/s against the venue's one per-UID 20/s.
///
/// Unified cross-venue attribution, task 5: this is the ONE `BybitPerpRest` used for
/// `submit_order`/`cancel_order`/`modify_order`/`set_leverage`, so `broker_id` (the resolved
/// FD-broker code) rides its SIGNER and every order-related REST request from the exec thread
/// stamps `X-Referer`. `None` (unconfigured) is byte-identical to before this field existed.
/// `leverage` is the mount-resolved operator leverage ([`leverage_for`], clamped) — this is the ONE
/// client that posts it (`set_leverage` right after this call); unset profile ⇒ [`DEFAULT_LEVERAGE`],
/// unchanged.
fn order_rest(
    creds: &Credentials,
    rest_base: &str,
    symbol: &str,
    properties: SymbolProperties,
    leverage: f64,
    broker_id: Option<String>,
) -> BybitPerpRest<UreqBybitTransport> {
    order_rest_gated(
        creds,
        rest_base,
        symbol,
        properties,
        leverage,
        broker_id,
        crate::ratelimit::rest_rate_gate(),
    )
}

/// [`order_rest`] on a GIVEN order-rate window. The mounted client and every foreign-symbol client of
/// one exec thread are built on clones of ONE gate: the venue's 20/s budget is per UID, so N symbols
/// must share the 18/s window, not each take their own.
fn order_rest_gated(
    creds: &Credentials,
    rest_base: &str,
    symbol: &str,
    properties: SymbolProperties,
    leverage: f64,
    broker_id: Option<String>,
    gate: RateGate,
) -> BybitPerpRest<UreqBybitTransport> {
    BybitPerpRest {
        signer: BybitV5Signer::new(creds, now_ms).with_broker_id(broker_id),
        transport: UreqBybitTransport::new().with_rate_gate(gate),
        base_url: rest_base.to_string(),
        symbol: symbol.to_string(),
        properties,
        leverage,
    }
}

/// **The one write of the account's leverage for a symbol the engine was NOT mounted on** (the
/// gatewayhub lane's per-symbol hand-over, `VenueAdapter::set_symbol_leverage`): [`BybitPerpRest::set_leverage`]
/// itself, the very verb the exec thread posts at its start (110043 "not modified" swallowed there),
/// on a client of its own because the exec thread's foreign-symbol clients post none on purpose
/// (`foreign_rest_factory`). `leverage` is written as given: the lane's probe refused a figure above
/// the symbol's maximum, so nothing is clamped here. `Err` is the venue's own words.
pub fn write_symbol_leverage(
    creds: &Credentials,
    mainnet: bool,
    symbol: &str,
    leverage: f64,
    broker_id: Option<String>,
) -> Result<(), String> {
    post_symbol_leverage(&symbol_leverage_rest(creds, mainnet, symbol, leverage, broker_id))?;
    tracing::info!(target: "vike_bybit", %symbol, leverage, "set-leverage POSTED for a symbol admitted on demand (the gatewayhub reads it back)");
    Ok(())
}

/// The client [`write_symbol_leverage`] posts through: the order client's own build ([`order_rest`], on
/// the order-rate window and with the broker code), pointed at the admitted symbol.
fn symbol_leverage_rest(
    creds: &Credentials,
    mainnet: bool,
    symbol: &str,
    leverage: f64,
    broker_id: Option<String>,
) -> BybitPerpRest<UreqBybitTransport> {
    order_rest(
        creds,
        endpoints(mainnet).0,
        &symbol.to_uppercase(),
        SymbolProperties::default(),
        leverage,
        broker_id,
    )
}

/// [`write_symbol_leverage`]'s post and its error words, over any transport.
fn post_symbol_leverage<T: BybitTransport>(rest: &BybitPerpRest<T>) -> Result<(), String> {
    rest.set_leverage().map_err(|e| format!("set-leverage for {} was refused: {e}", rest.symbol))
}

/// The grid an order on a symbol OTHER than the mounted one may be signed on, from the venue's own
/// `instruments-info` row for it: `Err(reason)` (shown to the operator in the terminal
/// `OrderRejected`) when the fetch failed or the symbol is unknown (`None`: never a guessed or
/// BTC-shaped fallback grid), or when the venue says it is not a linear perpetual
/// ([`non_linear_perpetual_refusal`], the same refusal the mounted symbol gets).
fn foreign_grid(
    symbol: &str,
    row: Option<crate::perp::BybitInstrument>,
) -> Result<SymbolProperties, String> {
    let Some(row) = row else {
        return Err(format!(
            "bybit: cannot sign {symbol:?}: no linear instrument grid for it \
             (unknown symbol, or instruments-info was unreachable); the order was not sent"
        ));
    };
    // A row that names no contract type is not shown to be linear: for a FOREIGN symbol (no
    // mounted-start default to keep working) that is a refusal, consistent with a failed fetch.
    if row.contract_type.is_none() {
        return Err(format!(
            "bybit: cannot sign {symbol:?}: instruments-info did not say its contract type, so it \n             is not shown to be a linear perpetual; the order was not sent"
        ));
    }
    match non_linear_perpetual_refusal(symbol, row.contract_type.as_deref()) {
        Some(why) => Err(why),
        None => Ok(row.properties),
    }
}

/// The [`PerSymbolRest`] factory: a signing client for another symbol of the SAME account (same
/// keys, host, leverage value, broker id and order-rate window as the mounted client), on that
/// symbol's own fetched grid. The grid fetch is one blocking `instruments-info` GET on the exec
/// thread, bounded by the transport's timeout. `leverage` is carried but NOT posted for the new
/// symbol: the venue keeps that symbol's current leverage (phase 1f of the orders-follow-the-symbol
/// spec is not done here).
fn foreign_rest_factory(
    creds: Credentials,
    mainnet: bool,
    leverage: f64,
    broker_id: Option<String>,
    gate: RateGate,
) -> ClientFactory<BybitPerpRest<UreqBybitTransport>> {
    Box::new(move |symbol| {
        // instruments-info rows are keyed by the upper-case symbol
        let symbol = symbol.to_uppercase();
        let properties = foreign_grid(&symbol, fetch_bybit_instrument(&creds, &symbol, mainnet))?;
        // ⚠ NO `set_leverage` here: this factory also runs after a restart, when the account may hold
        // a position on this symbol at a leverage set on the venue; posting the mounted symbol's
        // configured value would move its liquidation price.
        Ok(order_rest_gated(
            &creds,
            endpoints(mainnet).0,
            &symbol,
            properties,
            leverage,
            broker_id.clone(),
            gate.clone(),
        ))
    })
}

/// The exec thread: build the REST client, set leverage, start the private-WS pump, then drain
/// commands until `Shutdown`. ALL network I/O (leverage, submit/cancel, WS) lives HERE — off the
/// single-writer core thread. On `Shutdown` the pump is stopped + joined before returning.
#[expect(clippy::too_many_arguments)]
fn run(
    creds: Credentials,
    symbol: String,
    fallback_properties: SymbolProperties,
    events: EventSender,
    rx: Receiver<ExecCommand>,
    properties_rec: Option<Arc<vike_data::PropertiesRecorder>>,
    on_reconcile: Option<mpsc::Sender<()>>,
    broker_id: Option<String>,
    mainnet: bool,
    leverage: f64,
    fast_exec: bool,
    ledger: Arc<DispatchLedger>,
    coids: Arc<CoidSymbols>,
) {
    // Fetch the venue's REAL instrument grid for `symbol` (off the core thread); fall back to the
    // caller's demo-BTC default only if the fetch fails, so orders format on the right tick/step.
    // Only the REAL fetched grid is ever recorded into the PIT properties store (never the fallback).
    // The SAME response also carries `leverageFilter.maxLeverage` — this symbol's published
    // account-leverage ceiling. Captured here (free: one request, two fields) and applied to the
    // startup `set_leverage` below. A failed fetch yields `None`, i.e. no cap and no clamp.
    let row = fetch_bybit_instrument(&creds, &symbol, mainnet);
    // ⚠ **THE EXEC REFUSAL, on the venue's own word and before anything is built.** The fetch above
    // is the SAME request this arm always made; what is new is that its `contractType` is read
    // rather than discarded. A non-linear perpetual stops here: no REST client, no `set_leverage`
    // POST, no private WS, no signed request of any kind — just a loop that rejects every command
    // with the reason. See `non_linear_perpetual_refusal` for why the default without it is a
    // wrong-grid ACCEPTANCE rather than a failure.
    if let Some(why) =
        non_linear_perpetual_refusal(&symbol, row.as_ref().and_then(|i| i.contract_type.as_deref()))
    {
        tracing::error!(target: "vike_bybit", %symbol, "{why}");
        run_loop(&RefusingRest { why }, &events, rx, Vec::new, DEFAULT_FILL_SENTINEL);
        return;
    }
    let (properties, venue_leverage_cap) = match row.map(|i| (i.properties, i.max_leverage)) {
        Some((real, cap)) => {
            vike_data::PropertiesRecorder::record_opt(
                &properties_rec,
                "bybit",
                &symbol,
                real,
                now_ns(),
            );
            (real, cap)
        }
        None => {
            tracing::warn!(target: "vike_bybit", %symbol, "instruments-info fetch failed; using fallback properties");
            (fallback_properties, None)
        }
    };
    // Ceiling: the start resolved the operator's REQUEST, this is the venue's own limit for this
    // symbol. `min(request, cap)` when a cap is known; UNKNOWN (no `leverageFilter`, or the fetch
    // above failed) ⇒ the requested value rides through untouched, byte-identical to before this
    // clamp existed. `vike_bridge_core::leverage`'s module doc is the authority, including the
    // residual it leaves open (the RiskGate still sizes against the un-clamped request).
    let leverage = vike_bridge_core::leverage::clamp_to_venue_cap(
        leverage,
        venue_leverage_cap,
        crate::perp::VENUE,
        &symbol,
    );
    // Demo/mainnet host pair for this start's PRIVATE exec + user-data WS. Decision 0095: the
    // ceiling alone decides now (`AdapterInputs::tier`, as `BybitVenueAdapter::start` reads
    // it), resolved ONCE at the start and threaded in — this thread never re-resolves it. `false` ⇒ the demo
    // pair, byte-identical to before the ceiling took over this decision. All REST clients here +
    // the WS pump bind to it.
    let (rest_base, ws_base) = endpoints(mainnet);
    let rest = order_rest(&creds, rest_base, &symbol, properties, leverage, broker_id.clone());
    // ONE order-rate window for the mounted client and every foreign-symbol client built later: the
    // foreign ones are handed the mounted client's own gate. (`order_rest` always gates; the
    // fallback is unreachable, there only so a missing gate cannot panic an exec thread.)
    let gate = rest.transport.rate_gate().cloned().unwrap_or_else(crate::ratelimit::rest_rate_gate);
    // Network, off the core thread; 110043 ("leverage not modified") is swallowed inside.
    // Best-effort, UNCHANGED control flow (a rejection never kills the start) — but the discarded
    // `Result` is now WARNED on: a silently-failed leverage POST is the exact divergence this
    // wiring exists to close (the account keeps its old leverage while the RiskGate sizes against
    // the configured one), and binance/aster already warn here.
    if let Err(e) = rest.set_leverage() {
        tracing::warn!(target: "vike_bybit", %symbol, leverage, code = e.code, msg = %e.msg, "set_leverage failed (continuing) — the account keeps its CURRENT leverage while the RiskGate sizes against the configured one");
    }
    // From here the command loop's client signs each order on the ORDER's symbol: the mounted symbol
    // (or none) goes to `rest` above unchanged, any other symbol to a client built on first use.
    // A foreign order's CANCEL and status take a client with no grid fetch (a symbol and a coid are all
    // they send; the grid and leverage are never read), so they do not wait for `instruments-info`.
    let light: ClientFactory<BybitPerpRest<UreqBybitTransport>> = {
        let (creds, broker_id, gate) = (creds.clone(), broker_id.clone(), gate.clone());
        Box::new(move |symbol| {
            Ok(order_rest_gated(
                &creds,
                endpoints(mainnet).0,
                &symbol.to_uppercase(),
                fallback_properties,
                DEFAULT_LEVERAGE,
                broker_id.clone(),
                gate.clone(),
            ))
        })
    };
    let routed = PerSymbolRest::new(
        symbol.clone(),
        rest,
        foreign_rest_factory(creds.clone(), mainnet, leverage, broker_id, gate),
        coids,
    )
    .with_light_factory(light);
    // Audit A3: a SEPARATE REST client for the resync supervisor — the submit client above lives on
    // THIS thread and can't be shared with the pump's supervisor thread. After every private-WS
    // reconnect the supervisor replays recent order/execution history through this client, so a
    // fill/cancel/reject that landed during the reconnect gap is recovered (the core dedups the
    // overlap) instead of leaving a phantom-live order.
    let resync_rest = BybitPerpRest {
        signer: BybitV5Signer::new(&creds, now_ms),
        transport: UreqBybitTransport::new(),
        base_url: rest_base.to_string(),
        symbol: symbol.clone(),
        properties,
        // Same mount-resolved value as the submit client; history-only, so it never posts it.
        leverage,
    };
    // A THIRD client for the run_loop fill-sentinel (off the pump thread, so its history calls never
    // contend with the pump's own resync).
    let sentinel_rest = BybitPerpRest {
        signer: BybitV5Signer::new(&creds, now_ms),
        transport: UreqBybitTransport::new(),
        base_url: rest_base.to_string(),
        symbol: symbol.clone(),
        properties,
        // Same mount-resolved value as the submit client; history-only, so it never posts it.
        leverage,
    };
    let resync_symbol = symbol.clone();
    let sentinel_symbol = symbol.clone();
    // Private user-data WS (+ resync): authoritative fills/cancels push straight into the core ingest.
    let feed = spawn_bybit_perp_user_data_with_resync(
        ws_base.to_string(),
        creds.api_key.expose().clone(),
        creds.api_secret.expose().clone(),
        symbol,
        events.clone(),
        move || bybit_history_events(&resync_rest, &resync_symbol),
        on_reconcile,
        fast_exec,
    );
    // The gap-sentinel replays recent history `DEFAULT_FILL_SENTINEL` after the last command, to
    // recover a fill/cancel the WS lost (the first-connect subscribe race); the core dedups the
    // overlap, so a normal WS delivery makes it a no-op. A venue-free mechanism default.
    // The ledger stamps every dispatched submit, so the active confirm trusts "absent" only for one.
    run_loop(
        &Dispatched { rest: routed, ledger },
        &events,
        rx,
        || bybit_history_events(&sentinel_rest, &sentinel_symbol),
        DEFAULT_FILL_SENTINEL,
    );
    // Shutdown reached: stop + join the pump + resync supervisor (deterministic teardown).
    let _ = feed.shutdown();
}

#[path = "exec_tests.rs"]
#[cfg(test)]
mod exec_tests;
