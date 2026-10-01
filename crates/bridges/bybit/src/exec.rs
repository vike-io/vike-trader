//! Bybit live exec half — `ExecutionClient` over the V5 linear-perp REST + the private user-data WS.
//!
//! Built on the shared [`ExecActor`] scaffold: `submit`/`cancel` enqueue onto ONE dedicated OS thread
//! that drives [`BybitPerpRest`] (blocking REST off the single-writer core). `submit_order` emits
//! `[OrderSubmitted, OrderAccepted|OrderRejected]` synchronously; the authoritative async fills/cancels
//! return on the private WS via [`crate::user_data::spawn_bybit_perp_user_data`] straight into the core ingest.
//!
//! LIVE GATE: absent credentials never reach here — the composition root only spawns this when
//! `load_credentials_from` yields `Some` (the codebase's absent-creds-is-the-live-gate rule). With no
//! `.env` keys the venue stays paper.
//!
//! v1 scope: `submit`/`cancel` only (the DOM's click-to-place / cancel surface). `modify`/batch fall
//! through to the [`ExecutionClient`] defaults (leave-in-place / fan-out) — a native amend/batch is a
//! follow-up (`BybitPerpRest` already has `modify_order`/`submit_batch`, but `ExecCommand` carries no
//! such variant yet).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::Duration;
use vike_bridge_core::credentials::Credentials;

use vike_bridge_core::exec_actor::{ConfirmFn, ExecActor, ExecCommand, run_loop};
use vike_bridge_core::rest::VenueRest;
use vike_bridge_core::signer::BybitV5Signer;
use vike_bridge_core::user_data::sleep_unless_stopped;
use vike_exec::{CoreGone, EventSender, ExecutionClient};
use vike_model::events::Event;
use vike_model::{OrderRequest, SymbolProperties};

use crate::funding::BybitFundingPoller;
use crate::perp::{BybitPerpRest, PATH_INSTRUMENTS, endpoints, parse_bybit_perp_instruments};
use crate::transport::{BybitTransport, UreqBybitTransport};
use crate::user_data::spawn_bybit_perp_user_data_with_resync;

/// FALLBACK cross-margin leverage the exec thread sets once at start — the literal this arm posted
/// to `/v5/position/set-leverage` unconditionally before the operator's budget was threaded in
/// (demo default; matches the smoke tests). Used ONLY when the operator expressed no leverage
/// intent, so a mount with no `[risk] max_leverage` is byte-identical to every mount before
/// [`leverage_for`] existed. NEVER read on the submit path — `BybitPerpRest::leverage` is consulted
/// by `set_leverage` and nothing else, which is why the read-only clients below (funding poller,
/// confirm re-query) can carry it inertly.
const DEFAULT_LEVERAGE: f64 = 2.0;
/// Rows of recent order/execution history the audit-A3 resync replays after each WS reconnect.
const RESYNC_HISTORY_LIMIT: u32 = 50;
/// Gap-sentinel: after a submit/cancel, replay recent history this long later to recover a fill/cancel
/// the WS lost (esp. the first-connect subscribe race — a market order fills before the pump is
/// subscribed, and the private WS does not replay on subscribe). Dedup'd by the core, so a normal WS
/// delivery makes it a no-op. Reconnect-driven resync covers resting-order gaps with no recent submit.
const FILL_SENTINEL: Duration = Duration::from_secs(5);
/// Funding-settlement poll cadence (REST `/v5/account/transaction-log`; funding is NOT on the
/// private WS — see `crate::funding`'s module doc for the sign trap that rules that channel out).
/// Settlements post at most every 8h, so 60s keeps live equity current without hammering the
/// endpoint.
const FUNDING_POLL_INTERVAL: Duration = Duration::from_secs(60);

/// The account leverage this arm POSTs at startup, resolved from the operator's `[risk]` budget.
///
/// Pure, and `pub` on purpose — the SAME shape as the mount's ceiling (decision 0095):
/// `crate::mount`'s `BybitVenueMount::mount` is handed the `ProfileRisk` and calls this ONCE per
/// mount, then threads the resolved `f64` into [`BybitExecutionClient::spawn_with_recorder`]
/// (docs/decisions/0096 moved that call out of `vike_mount::make_engine`). Nothing inside the
/// adapter reaches for the profile, so the number the venue account is configured with and the
/// number the RiskGate sizes against (`ProfileRisk::im_requirement`, `im = 1.0 / max_leverage`)
/// can never disagree — that disagreement was the bug this exists to close.
///
/// Unset (`None`, or a profile that never mentions `max_leverage`) ⇒ [`DEFAULT_LEVERAGE`],
/// byte-identical to before. See `vike_bridge_core::leverage` for the full rule.
pub fn leverage_for(profile: Option<&vike_exec::ProfileRisk>) -> f64 {
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
/// `mainnet` is the already-resolved ceiling verdict the mount threaded in (decision 0095:
/// `crate::mount`'s `BybitVenueMount::mount` takes it ONCE from the ceiling `vike-mount` resolves
/// from `policy.venues.bybit`) — never re-read here, so the grid host can never disagree with the
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
/// caller (the exec thread's own grid fetch, `crate::mount`'s `RiskLimits` pre-fetch, the live
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
    // Grid host follows the same demo/mainnet switch the exec path uses so a mainnet mount
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
/// a missing field would take a working linear mount off the venue for a parse miss. The fetch
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
/// mount are already inside the authoritative venue balance the account seeds from, so emitting
/// them would double-count — see `crate::funding`'s module doc (the restart law).
///
/// CROSS-MOUNT HAZARD (for a future multi-mount-per-venue-account world): the transaction-log
/// query is ACCOUNT-WIDE (no `symbol` param — deliberate, so every mounted symbol's settlement
/// reaches the fold). If several bybit perp mounts ever share one venue account, EACH mount's
/// poller would emit EVERY symbol's settlements → N-fold duplication through the non-idempotent
/// `apply_funding`. Whoever adds multi-mount must dedupe this lane account-wide (one poller per
/// account, not per mount).
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
                // The mount's already-resolved mainnet verdict (decision 0095:
                // `MountInputs::live_permitted`, as `BybitVenueMount::mount` reads it), captured
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
            // Floor = spawn instant: only settlements landing WHILE mounted are folded.
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
pub struct BybitExecutionClient(ExecActor);

impl BybitExecutionClient {
    /// Spawn the exec thread (which fetches the instrument grid, sets leverage, starts the user-data
    /// pump, then drains commands). `symbol` is the V5 linear instrument (e.g. `"BTCUSDT"`);
    /// `fallback_properties` is used ONLY if the exec thread's `instruments-info` fetch fails (a
    /// demo-BTC default) — the real grid is fetched at startup so any symbol formats correctly.
    ///
    /// `mainnet` is the already-resolved ceiling verdict (decision 0095: the mount's ceiling
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
    /// `run_resync_supervisor` call — `Some` pokes the reconcile driver after every reconnect's
    /// event-replay settles; `None` (every non-recon caller, e.g. [`Self::spawn`]) reproduces the
    /// pre-Task-7 event-replay-only behavior byte-for-byte.
    ///
    /// `broker_id` (unified cross-venue attribution, task 5) is the FD-broker code resolved ONCE at
    /// the mount site (`crate::mount`'s `BybitVenueMount::mount`, via `attribution_code_from` over
    /// the credential map); threaded down to the ONE `BybitPerpRest`
    /// signer actually used for `submit_order` (`run`'s `rest`), so every order-create request carries
    /// `X-Referer: <id>`. `None` (every non-recon caller, e.g. [`Self::spawn`]) is byte-identical.
    ///
    /// `mainnet` (decision 0095: the mount's ceiling chooses the network) is likewise resolved ONCE
    /// at the mount site and threaded down to EVERY host selection this client makes — the confirm
    /// re-query client, the funding poller and the exec thread's three REST clients + user-data WS.
    /// No thread below re-reads the flag, so one mount can never end up signing mainnet credentials
    /// against demo hosts. `false` ⇒ the demo pair, byte-identical to before this switch existed.
    ///
    /// `leverage` is the account leverage the exec thread posts once at start
    /// (`POST /v5/position/set-leverage`, buy == sell), resolved ONCE at the mount site from the
    /// operator's `[risk]` budget via [`leverage_for`] — so the venue account and the RiskGate's
    /// margin math run on the SAME number. Pass [`DEFAULT_LEVERAGE`] (what [`Self::spawn`] does)
    /// for the historical literal.
    ///
    /// `fast_exec` is the `venue.bybit.fast_exec` setting, read once by the mount through
    /// [`crate::user_data::fast_exec`] and handed to the private-socket pump. [`Self::spawn`]
    /// passes `false`.
    #[allow(clippy::too_many_arguments)] // STEP-2's `mainnet` pushed this past the default threshold
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
        // Active-confirm re-query (audit ex1 residual): the ExecActor runs this on its OWN worker
        // thread when the core issues `Command::ConfirmOrder`, so a wedged adapter is prodded with
        // NO network on the core fold. It builds a fresh short-lived REST client (like the resync +
        // sentinel clients) and reuses the exact ambiguous-submit re-query via `confirm_order`. The
        // status GET is keyed on orderLinkId only, so `fallback_properties` (unused by the GET) is fine.
        let confirm: ConfirmFn = {
            let creds = creds.clone();
            let symbol = symbol.clone();
            Arc::new(move |coid: &str| {
                let rest = BybitPerpRest {
                    signer: BybitV5Signer::new(&creds, now_ms),
                    transport: UreqBybitTransport::new(),
                    // The mount's resolved verdict, captured — never re-read on this worker.
                    base_url: endpoints(mainnet).0.to_string(),
                    symbol: symbol.clone(),
                    properties: fallback_properties,
                    // INERT here: a status re-query never calls `set_leverage` (the field's sole
                    // reader), so the fallback is correct by construction.
                    leverage: DEFAULT_LEVERAGE,
                };
                rest.confirm_order(coid, now_ms())
            })
        };
        // Funding-settlement poll: perp-only by construction (this crate's ONLY exec client is the
        // linear-perp one — spot is deferred, see the crate doc), so spawning it here is inherently
        // scoped to perp mounts.
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
            )
        })
        .with_confirm(confirm)
        .with_background(funding_stop, funding_join);
        Self(actor)
    }
}

impl ExecutionClient for BybitExecutionClient {
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

/// The exec thread: build the REST client, set leverage, start the private-WS pump, then drain
/// commands until `Shutdown`. ALL network I/O (leverage, submit/cancel, WS) lives HERE — off the
/// single-writer core thread. On `Shutdown` the pump is stopped + joined before returning.
#[allow(clippy::too_many_arguments)]
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
        run_loop(&RefusingRest { why }, &events, rx, Vec::new, FILL_SENTINEL);
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
    // Ceiling: the mount resolved the operator's REQUEST, this is the venue's own limit for this
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
    // Demo/mainnet host pair for this mount's PRIVATE exec + user-data WS. Decision 0095: the
    // ceiling alone decides now (`MountInputs::live_permitted`, as `BybitVenueMount::mount` reads
    // it), resolved ONCE at the mount and threaded in — this thread never re-resolves it. `false` ⇒ the demo
    // pair, byte-identical to before the ceiling took over this decision. All REST clients here +
    // the WS pump bind to it.
    let (rest_base, ws_base) = endpoints(mainnet);
    let rest = BybitPerpRest {
        // Unified cross-venue attribution, task 5: the ONE `BybitPerpRest` used for
        // `submit_order`/`cancel_order`/`modify_order`/`set_leverage` (`run_loop`'s `rest`) carries
        // the resolved FD-broker code, so every order-related REST request from this thread stamps
        // `X-Referer`. `None` (unconfigured) is byte-identical to before this field existed.
        signer: BybitV5Signer::new(&creds, now_ms).with_broker_id(broker_id),
        transport: UreqBybitTransport::new(),
        base_url: rest_base.to_string(),
        symbol: symbol.clone(),
        properties,
        // The mount-resolved operator leverage ([`leverage_for`]) — this is the ONE client that
        // posts it (`set_leverage` just below); unset profile ⇒ `DEFAULT_LEVERAGE`, unchanged.
        leverage,
    };
    // Network, off the core thread; 110043 ("leverage not modified") is swallowed inside.
    // Best-effort, UNCHANGED control flow (a rejection never kills the mount) — but the discarded
    // `Result` is now WARNED on: a silently-failed leverage POST is the exact divergence this
    // wiring exists to close (the account keeps its old leverage while the RiskGate sizes against
    // the configured one), and binance/aster already warn here.
    if let Err(e) = rest.set_leverage() {
        tracing::warn!(target: "vike_bybit", %symbol, leverage, code = e.code, msg = %e.msg, "set_leverage failed (continuing) — the account keeps its CURRENT leverage while the RiskGate sizes against the configured one");
    }
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
        creds.api_key.clone(),
        creds.api_secret.clone(),
        symbol,
        events.clone(),
        move || bybit_history_events(&resync_rest, &resync_symbol),
        on_reconcile,
        fast_exec,
    );
    run_loop(
        &rest,
        &events,
        rx,
        || bybit_history_events(&sentinel_rest, &sentinel_symbol),
        FILL_SENTINEL,
    );
    // Shutdown reached: stop + join the pump + resync supervisor (deterministic teardown).
    let _ = feed.shutdown();
}

#[path = "exec_tests.rs"]
#[cfg(test)]
mod exec_tests;
