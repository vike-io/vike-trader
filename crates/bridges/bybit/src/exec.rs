//! Bybit live exec half — `ExecutionClient` over the V5 linear-perp REST + the private user-data WS.
//!
//! Built on the shared [`ExecActor`] scaffold: `submit`/`cancel` enqueue onto ONE dedicated OS thread
//! that drives [`BybitPerpRest`] (blocking REST off the single-writer core). `submit_order` emits
//! `[OrderSubmitted, OrderAccepted|OrderRejected]` synchronously; the authoritative async fills/cancels
//! return on the private WS via [`spawn_bybit_perp_user_data`] straight into the core ingest.
//!
//! LIVE GATE: absent credentials never reach here — the composition root only spawns this when
//! `load_credentials_from` yields `Some` (the codebase's absent-creds-is-the-live-gate rule). With no
//! `.env` keys the venue stays paper.
//!
//! v1 scope: `submit`/`cancel` only (the DOM's click-to-place / cancel surface). `modify`/batch fall
//! through to the [`ExecutionClient`] defaults (leave-in-place / fan-out) — a native amend/batch is a
//! follow-up (`BybitPerpRest` already has `modify_order`/`submit_batch`, but `ExecCommand` carries no
//! such variant yet).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use vike_bridge_core::credentials::Credentials;

use vike_bridge_core::exec_actor::{run_loop, ConfirmFn, ExecActor, ExecCommand};
use vike_bridge_core::rest::VenueRest;
use vike_bridge_core::signer::BybitV5Signer;
use vike_bridge_core::user_data::sleep_unless_stopped;
use vike_exec::{CoreGone, EventSender, ExecutionClient};
use vike_model::events::Event;
use vike_model::{OrderRequest, SymbolProperties};

use crate::funding::BybitFundingPoller;
use crate::perp::{endpoints, parse_bybit_perp_instruments, BybitPerpRest, PATH_INSTRUMENTS};
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
/// Pure, and `pub` on purpose — the SAME shape as [`crate::perp::mainnet_enabled`]:
/// `vike_mount::make_engine` owns the `ProfileRisk` and calls this ONCE per mount, then threads the
/// resolved `f64` into [`BybitExecutionClient::spawn_with_recorder`]. Nothing inside the adapter
/// reaches for the profile, so the number the venue account is configured with and the number the
/// RiskGate sizes against (`ProfileRisk::im_requirement`, `im = 1.0 / max_leverage`) can never
/// disagree — that disagreement was the bug this exists to close.
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
/// `mainnet` is the already-resolved `BYBIT_MAINNET` verdict the mount threaded in
/// (`vike_mount::make_engine` resolves it ONCE via [`crate::perp::mainnet_enabled`]) — never
/// re-read here (STEP 2), so the grid host can never disagree with the exec host.
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
/// caller (the exec thread's own grid fetch, `vike_mount::make_engine`'s `RiskLimits` pre-fetch,
/// the live smokes) is untouched, and so the cap costs no second round-trip anywhere. Consumed by
/// [`run`] via `vike_bridge_core::leverage::clamp_to_venue_cap`.
pub fn fetch_bybit_properties_with_cap(
    creds: &Credentials,
    symbol: &str,
    mainnet: bool,
) -> Option<(SymbolProperties, Option<f64>)> {
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
    parse_bybit_perp_instruments(&info).get(symbol).map(|i| (i.properties, i.max_leverage))
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
                // The mount's already-resolved `BYBIT_MAINNET` verdict, captured at spawn — this
                // thread never re-reads global env, so it can never poll a different network than
                // the exec thread trades on.
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
    /// `mainnet` is the already-resolved `BYBIT_MAINNET` verdict (see
    /// [`crate::perp::mainnet_enabled`]); `false` binds the demo host pair — the explicit,
    /// self-documenting default for a smoke/test caller, which since STEP 2 can no longer be
    /// silently upgraded to mainnet by a stray exported flag.
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
    /// the mount site (`attribution_code_from(vars, "bybit")`); threaded down to the ONE `BybitPerpRest`
    /// signer actually used for `submit_order` (`run`'s `rest`), so every order-create request carries
    /// `X-Referer: <id>`. `None` (every non-recon caller, e.g. [`Self::spawn`]) is byte-identical.
    ///
    /// `mainnet` (STEP 2 of the `{VENUE}_MAINNET` convergence) is likewise resolved ONCE at the
    /// mount site ([`crate::perp::mainnet_enabled`], which reads the process env AND the workspace
    /// `.env` map) and threaded down to EVERY host selection this client makes — the confirm
    /// re-query client, the funding poller and the exec thread's three REST clients + user-data WS.
    /// No thread below re-reads the flag, so one mount can never end up signing mainnet credentials
    /// against demo hosts. `false` ⇒ the demo pair, byte-identical to before this switch existed.
    ///
    /// `leverage` is the account leverage the exec thread posts once at start
    /// (`POST /v5/position/set-leverage`, buy == sell), resolved ONCE at the mount site from the
    /// operator's `[risk]` budget via [`leverage_for`] — so the venue account and the RiskGate's
    /// margin math run on the SAME number. Pass [`DEFAULT_LEVERAGE`] (what [`Self::spawn`] does)
    /// for the historical literal.
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
) {
    // Fetch the venue's REAL instrument grid for `symbol` (off the core thread); fall back to the
    // caller's demo-BTC default only if the fetch fails, so orders format on the right tick/step.
    // Only the REAL fetched grid is ever recorded into the PIT properties store (never the fallback).
    // The SAME response also carries `leverageFilter.maxLeverage` — this symbol's published
    // account-leverage ceiling. Captured here (free: one request, two fields) and applied to the
    // startup `set_leverage` below. A failed fetch yields `None`, i.e. no cap and no clamp.
    let (properties, venue_leverage_cap) = match fetch_bybit_properties_with_cap(
        &creds, &symbol, mainnet,
    ) {
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
    // Demo/mainnet host pair for this mount's PRIVATE exec + user-data WS. The flag was resolved
    // ONCE at the mount (`BYBIT_MAINNET`, exact `"1"` from process env or the workspace `.env`,
    // process winning) and threaded in — this thread never re-reads it. `false` ⇒ the demo pair,
    // byte-identical to before this switch existed. All REST clients here + the WS pump bind to it.
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

#[cfg(test)]
mod tests {
    //! Bybit-specific tests: the live per-symbol properties fetch (ignored; network) and the PIT
    //! properties recording. The generic `run_loop` command→event tests (mock `VenueRest`) moved
    //! WITH `run_loop` to `vike_bridge_core::exec_actor::run_loop_tests`; the bybit reality-tie
    //! (`caps_for("bybit").supports_modify`) lives in the lib `caps_test`.
    use super::*;

    /// The leverage this arm POSTs to `/v5/position/set-leverage` is the OPERATOR's
    /// `[risk] max_leverage`, and an unset profile still yields the historical literal (`2.0`) —
    /// the property that makes this change safe to ship onto a live account. NO network:
    /// `leverage_for` is the pure decision `make_engine` calls once, before anything is spawned.
    #[test]
    fn perp_leverage_follows_the_operator_risk_budget() {
        // Unset — both shapes of "the operator said nothing" — keeps today's literal EXACTLY.
        assert_eq!(leverage_for(None), DEFAULT_LEVERAGE);
        assert_eq!(leverage_for(None), 2.0, "the historical hardcoded set-leverage value");
        let silent = vike_exec::ProfileRisk { max_leverage: None, ..Default::default() };
        assert_eq!(leverage_for(Some(&silent)), 2.0);

        // Set — the venue account is configured with the operator's own number, in BOTH
        // directions (the two ways the old hardcode was dangerous).
        let ten = vike_exec::ProfileRisk { max_leverage: Some(10.0), ..Default::default() };
        assert_eq!(leverage_for(Some(&ten)), 10.0);
        let one = vike_exec::ProfileRisk { max_leverage: Some(1.0), ..Default::default() };
        assert_eq!(leverage_for(Some(&one)), 1.0);

        // ONE number governs both sides: what we POST is the reciprocal of the initial-margin
        // fraction the RiskGate sizes against.
        assert_eq!(ten.im_requirement(), Some(0.1));
        assert_eq!(ten.im_requirement(), Some(1.0 / leverage_for(Some(&ten))));
        // The same invariant, via the shared helper the rule itself ships.
        assert!(vike_bridge_core::leverage::agrees_with_im_requirement(&ten, DEFAULT_LEVERAGE));
    }

    /// LIVE proof the startup fetch is REAL + per-symbol (not the hardcoded fallback): BTCUSDT and
    /// ETHUSDT have different venue grids, so the fetched ticks must differ. Read-only (no orders).
    #[test]
    #[ignore = "network + demo creds — run manually"]
    fn fetch_returns_real_per_symbol_properties() {
        use vike_bridge_core::credentials::{
            load_credentials_from, load_workspace_dotenv, Environment,
        };
        vike_log::test_init();
        let vars = load_workspace_dotenv();
        let Some(creds) = load_credentials_from("bybit", Environment::Demo, &vars) else {
            return; // no creds → skip
        };
        // `false` = the demo host, explicitly — this is a demo-creds smoke, so it must never be
        // silently upgraded to the mainnet grid by a stray exported `BYBIT_MAINNET`.
        let btc =
            fetch_bybit_properties(&creds, "BTCUSDT", false).expect("BTCUSDT instruments-info");
        let eth =
            fetch_bybit_properties(&creds, "ETHUSDT", false).expect("ETHUSDT instruments-info");
        tracing::info!(
            target: "vike_bybit",
            "fetched BTC tick={} step={} | ETH tick={} step={}",
            btc.tick_size, btc.step_size, eth.tick_size, eth.step_size
        );
        assert!(btc.tick_size > 0.0 && eth.tick_size > 0.0);
        assert_ne!(
            btc.tick_size, eth.tick_size,
            "per-symbol fetch must differ (BTC {} vs ETH {}) — else it's the fallback",
            btc.tick_size, eth.tick_size
        );
    }

    /// PIT filter recording (task 4): `record_properties` writes the REAL fetched grid into the store
    /// when a recorder is present, keyed by venue `"bybit"`. Uses the DataFusion-free `MemHistStore`
    /// test double (vike-data's `test-support` dev-feature) so this venue's default build never pulls
    /// DataFusion in.
    #[test]
    fn bybit_records_fetched_properties_when_recorder_present() {
        use std::sync::Arc;
        use vike_data::HistStore;
        let store = Arc::new(vike_data::MemHistStore::new());
        let rec = Some(Arc::new(vike_data::PropertiesRecorder::new(store.clone(), true)));
        let f = SymbolProperties {
            tick_size: 0.1,
            step_size: 0.001,
            min_qty: 0.001,
            min_notional: 5.0,
            ..Default::default()
        };
        vike_data::PropertiesRecorder::record_opt(
            &rec,
            "bybit",
            "BTCUSDT",
            f,
            1_577_836_800_000_000_000,
        );
        assert_eq!(
            store.scan_symbol_properties("bybit", "BTCUSDT", vike_data::TsRange::all()).unwrap(),
            vec![(1_577_836_800_000i64, f)]
        );
    }

    #[test]
    fn bybit_no_recorder_writes_nothing() {
        use std::sync::Arc;
        use vike_data::HistStore;
        let store = Arc::new(vike_data::MemHistStore::new());
        vike_data::PropertiesRecorder::record_opt(
            &None,
            "bybit",
            "BTCUSDT",
            SymbolProperties::default(),
            1_577_836_800_000_000_000,
        );
        assert!(store
            .scan_symbol_properties("bybit", "BTCUSDT", vike_data::TsRange::all())
            .unwrap()
            .is_empty());
    }

    /// A fake [`BybitTransport`] that answers `/v5/account/transaction-log` with a canned
    /// SETTLEMENT row (or an empty list) — the offline twin of the live `BybitFundingPoller`
    /// smoke, proving `poll_funding_once`'s spawn glue (not just the pure decoder, which
    /// `r6_bybit_parity.rs` already pins).
    struct FakeFundingTransport {
        rows: Vec<serde_json::Value>,
        /// When `Some`, assert the query carries this exact `startTime` (the floor optimization
        /// param); `None` asserts the param is ABSENT (a floorless probe's query, unchanged).
        expect_start_time: Option<String>,
    }

    impl BybitTransport for FakeFundingTransport {
        fn signed(
            &self,
            _base_url: &str,
            path: &str,
            _method: &str,
            params: &[(&str, serde_json::Value)],
            _signer: &BybitV5Signer,
        ) -> Result<serde_json::Value, vike_bridge_core::transport::VenueApiError> {
            assert_eq!(path, crate::funding::LOG_PATH);
            let start = params.iter().find(|(k, _)| *k == "startTime").map(|(_, v)| v.clone());
            match &self.expect_start_time {
                Some(want) => assert_eq!(start, Some(serde_json::json!(want))),
                None => assert_eq!(start, None, "a floorless poll must not send startTime"),
            }
            Ok(serde_json::json!({"retCode": 0, "retMsg": "OK", "result": {"list": self.rows}}))
        }
    }

    fn settlement_row(id: &str, funding: &str, ts: &str) -> serde_json::Value {
        serde_json::json!({
            "id": id, "type": "SETTLEMENT", "symbol": "BTCUSDT",
            "funding": funding, "feeRate": "0.0001", "transactionTime": ts
        })
    }

    /// The spawn glue's one poll iteration forwards decoded `Event::Funding`s onto the core
    /// ingest lane — the path a background poll thread drives every cadence.
    #[test]
    fn poll_funding_once_forwards_decoded_events() {
        let rest = BybitPerpRest {
            signer: BybitV5Signer::new(
                &Credentials { api_key: "k".into(), api_secret: "s".into(), passphrase: None },
                || 0,
            ),
            transport: FakeFundingTransport {
                rows: vec![settlement_row("r1", "1.25", "1000")],
                expect_start_time: None,
            },
            base_url: "http://example.invalid".to_string(),
            symbol: "BTCUSDT".to_string(),
            properties: SymbolProperties::default(),
            leverage: DEFAULT_LEVERAGE,
        };
        let mut poller = BybitFundingPoller::new(&rest, "BTCUSDT", 0);
        let (events, mut ingest) = vike_exec::event_channel(8);
        let mut streak = 0u32;
        poll_funding_once(&mut poller, &events, &mut streak).expect("core is alive");
        assert_eq!(streak, 0, "a successful poll resets the failure streak");

        let mut got = Vec::new();
        while let Ok(vike_exec::Ingest::Event(e)) = ingest.try_recv() {
            got.push(e);
        }
        match got.as_slice() {
            [Event::Funding(f)] => {
                assert_eq!(f.venue, "bybit");
                assert_eq!(f.symbol, "BTCUSDT");
                assert_eq!(f.amount, 1.25, "received-positive, no sign flip");
            }
            other => panic!("expected one Event::Funding, got {other:?}"),
        }

        // A second poll with the SAME row (as a real re-fetch might return before settling)
        // yields nothing new — the dedup-by-id guard the live poller already owns.
        let more = poll_funding_once(&mut poller, &events, &mut streak);
        assert!(more.is_ok());
        assert!(ingest.try_recv().is_err(), "the already-seen row must not re-emit");
    }

    /// The restart law: a poller floored at its spawn instant (as `spawn_funding_poll` does) must
    /// NEVER emit a settlement that posted BEFORE the floor — those are already embedded in the
    /// authoritative venue balance — while a settlement AT/after the floor is emitted exactly once.
    /// Simulates the (re)start-with-empty-seen-set case the in-memory id-dedup cannot cover.
    #[test]
    fn poll_funding_skips_settlements_before_the_spawn_floor() {
        const FLOOR: i64 = 5_000;
        let rest = BybitPerpRest {
            signer: BybitV5Signer::new(
                &Credentials { api_key: "k".into(), api_secret: "s".into(), passphrase: None },
                || 0,
            ),
            transport: FakeFundingTransport {
                // The venue answers with history straddling the floor (as the default ~7d window
                // would on a restart): pre-floor, exactly-at-floor, and post-floor rows.
                rows: vec![
                    settlement_row("old", "9.99", "1000"),
                    settlement_row("edge", "0.25", "5000"),
                    settlement_row("new", "1.25", "9000"),
                ],
                // The floor also rides as the startTime request param (the optimization).
                expect_start_time: Some(FLOOR.to_string()),
            },
            base_url: "http://example.invalid".to_string(),
            symbol: "BTCUSDT".to_string(),
            properties: SymbolProperties::default(),
            leverage: DEFAULT_LEVERAGE,
        };
        let mut poller = BybitFundingPoller::new(&rest, "BTCUSDT", FLOOR);
        let (events, mut ingest) = vike_exec::event_channel(8);
        let mut streak = 0u32;
        poll_funding_once(&mut poller, &events, &mut streak).expect("core is alive");

        let mut got = Vec::new();
        while let Ok(vike_exec::Ingest::Event(e)) = ingest.try_recv() {
            got.push(e);
        }
        match got.as_slice() {
            [Event::Funding(edge), Event::Funding(new)] => {
                assert_eq!(edge.ts, 5000, "ts == floor is emitted (floor is inclusive)");
                assert_eq!(new.ts, 9000);
                assert_eq!(new.amount, 1.25);
            }
            other => panic!("expected exactly the at/post-floor settlements, got {other:?}"),
        }

        // The next cadence re-serves the same window: nothing re-emits (id-dedup) and the
        // pre-floor row stays excluded.
        poll_funding_once(&mut poller, &events, &mut streak).expect("core is alive");
        assert!(ingest.try_recv().is_err(), "no duplicates on the second poll");
    }

    /// When the core is gone (ingest receiver dropped), the poll glue reports `Err(CoreGone)` so
    /// its background thread's loop can self-exit instead of polling forever into a void.
    #[test]
    fn poll_funding_once_reports_core_gone() {
        let rest = BybitPerpRest {
            signer: BybitV5Signer::new(
                &Credentials { api_key: "k".into(), api_secret: "s".into(), passphrase: None },
                || 0,
            ),
            transport: FakeFundingTransport {
                rows: vec![settlement_row("r2", "0.5", "1000")],
                expect_start_time: None,
            },
            base_url: "http://example.invalid".to_string(),
            symbol: "BTCUSDT".to_string(),
            properties: SymbolProperties::default(),
            leverage: DEFAULT_LEVERAGE,
        };
        let mut poller = BybitFundingPoller::new(&rest, "BTCUSDT", 0);
        let (events, ingest) = vike_exec::event_channel(8);
        drop(ingest); // the core is gone
        let mut streak = 0u32;
        assert!(poll_funding_once(&mut poller, &events, &mut streak).is_err());
    }

    /// A transport failure is NOT core-gone: the glue reports `Ok` (keep polling), counts the
    /// consecutive-failure streak (the once-per-streak warn key), and a later success resets it.
    #[test]
    fn poll_funding_fetch_failure_counts_the_streak() {
        struct FailingTransport;
        impl BybitTransport for FailingTransport {
            fn signed(
                &self,
                _base_url: &str,
                _path: &str,
                _method: &str,
                _params: &[(&str, serde_json::Value)],
                _signer: &BybitV5Signer,
            ) -> Result<serde_json::Value, vike_bridge_core::transport::VenueApiError> {
                Err(vike_bridge_core::transport::VenueApiError {
                    code: 0,
                    msg: "connect refused".to_string(),
                })
            }
        }
        let rest = BybitPerpRest {
            signer: BybitV5Signer::new(
                &Credentials { api_key: "k".into(), api_secret: "s".into(), passphrase: None },
                || 0,
            ),
            transport: FailingTransport,
            base_url: "http://example.invalid".to_string(),
            symbol: "BTCUSDT".to_string(),
            properties: SymbolProperties::default(),
            leverage: DEFAULT_LEVERAGE,
        };
        let mut poller = BybitFundingPoller::new(&rest, "BTCUSDT", 0);
        let (events, mut ingest) = vike_exec::event_channel(8);
        let mut streak = 0u32;
        assert!(poll_funding_once(&mut poller, &events, &mut streak).is_ok());
        assert!(poll_funding_once(&mut poller, &events, &mut streak).is_ok());
        assert_eq!(streak, 2, "consecutive failures accumulate");
        assert!(ingest.try_recv().is_err(), "failures emit nothing");
    }
}
