//! OKX live exec half — `ExecutionClient` over the V5 SWAP-perp REST + the private user-data WS.
//!
//! Built on the shared [`ExecActor`] scaffold: `submit`/`cancel` enqueue onto ONE dedicated OS thread
//! that drives [`OkxPerpRest`] (blocking REST off the single-writer core). `submit_order` emits
//! `[OrderSubmitted, OrderAccepted|OrderRejected]` synchronously (routing `order_type="stop"` to the
//! algo endpoint); the authoritative async fills/cancels return on the private WS via
//! [`spawn_okx_perp_user_data`] straight into the core ingest.
//!
//! OKX-specifics vs Bybit: the perp is priced in CONTRACTS (`ct_val` base per contract — carried in
//! both the REST client and the WS pump), demo trading is the same REST host + the
//! `x-simulated-trading: 1` header (`UreqOkxTransport::new(true)`), and the signer needs the account
//! passphrase.
//!
//! LIVE GATE: absent credentials never reach here — the composition root only spawns this when
//! `load_credentials_from` yields `Some` (absent-creds-is-the-live-gate). With no `.env` keys the
//! venue stays paper.
//!
//! v1 scope: `submit`/`cancel` only (the DOM surface). `modify`/batch fall through to the
//! [`ExecutionClient`] defaults — a native amend/batch is a follow-up.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use vike_bridge_core::credentials::Credentials;
use vike_bridge_core::exec_actor::{run_loop, ConfirmFn, ExecActor, ExecCommand};
use vike_bridge_core::rest::VenueRest;
use vike_bridge_core::signer::OkxV5Signer;
use vike_bridge_core::user_data::sleep_unless_stopped;
use vike_exec::{CoreGone, EventSender, ExecutionClient};
use vike_model::events::Event;
use vike_model::{OrderRequest, SymbolProperties};

use crate::funding::OkxFundingPoller;
use crate::perp::{
    parse_okx_perp_instruments, simulated, ws_url, OkxPerpRest, PATH_INSTRUMENTS, REST,
};
use crate::transport::{unwrap_okx, OkxTransport, UreqOkxTransport};
use crate::user_data::spawn_okx_perp_user_data_with_resync;

/// FALLBACK cross-margin leverage the exec thread sets once at start — the literal this arm posted
/// to `/api/v5/account/set-leverage` unconditionally before the operator's budget was threaded in
/// (demo default; matches the smoke tests). Used ONLY when the operator expressed no leverage
/// intent, so a mount with no `[risk] max_leverage` is byte-identical to every mount before
/// [`leverage_for`] existed. NEVER read on the submit path — `OkxPerpRest::leverage` is consulted
/// by `set_leverage` and nothing else, which is why the read-only clients below (funding poller,
/// confirm re-query) can carry it inertly.
const DEFAULT_LEVERAGE: f64 = 2.0;
/// Rows of recent order/fill history the audit-A3 resync replays after each WS reconnect.
const RESYNC_HISTORY_LIMIT: u32 = 50;
/// Gap-sentinel: after a submit/cancel, replay recent history this long later to recover a fill/cancel
/// that the WS lost (esp. the first-connect subscribe race — a market order fills before the pump is
/// subscribed, and the private WS does not replay on subscribe). Dedup'd by the core, so a normal WS
/// delivery makes it a no-op. Reconnect-driven resync covers resting-order gaps with no recent submit.
const FILL_SENTINEL: Duration = Duration::from_secs(5);
/// Funding-bill poll cadence (REST `/api/v5/account/bills`; funding is NOT on any OKX WS channel —
/// see `crate::funding`'s module doc). Settlements post at most every 8h, so 60s keeps live equity
/// current without hammering the endpoint.
const FUNDING_POLL_INTERVAL: Duration = Duration::from_secs(60);

/// The account leverage this arm POSTs at startup, resolved from the operator's `[risk]` budget.
///
/// Pure, and `pub` on purpose — the SAME shape as [`crate::perp::mainnet_enabled`]:
/// `vike_mount::make_engine` owns the `ProfileRisk` and calls this ONCE per mount, then threads the
/// resolved `f64` into [`OkxExecutionClient::spawn_with_recorder`]. Nothing inside the adapter
/// reaches for the profile, so the number the venue account is configured with and the number the
/// RiskGate sizes against (`ProfileRisk::im_requirement`, `im = 1.0 / max_leverage`) can never
/// disagree — that disagreement was the bug this exists to close.
///
/// Unset (`None`, or a profile that never mentions `max_leverage`) ⇒ [`DEFAULT_LEVERAGE`],
/// byte-identical to before. See `vike_bridge_core::leverage` for the full rule.
pub fn leverage_for(profile: Option<&vike_exec::ProfileRisk>) -> f64 {
    vike_bridge_core::leverage::resolve_startup_leverage(profile, DEFAULT_LEVERAGE)
}

/// Fetch + map recent order/fill history to events (the audit-A3 replay body). Shared by the pump's
/// reconnect-resync AND the `run_loop` fill-sentinel; both feed the same dedup'd core ingest.
fn okx_history_events(
    rest: &OkxPerpRest<UreqOkxTransport>,
    symbol: &str,
    ct_val: f64,
) -> Vec<Event> {
    let orders =
        rest.get_orders_history(RESYNC_HISTORY_LIMIT).unwrap_or_else(|_| serde_json::json!([]));
    let fills =
        rest.get_fills_history(RESYNC_HISTORY_LIMIT).unwrap_or_else(|_| serde_json::json!([]));
    crate::history::map_okx_history(&orders, &fills, "okx", symbol, ct_val)
}

// Wall-clock helpers: the shared `vike_model::{now_ms, now_ns}` (a plain fn item, so `now_ms`
// still coerces to the `Fn() -> i64 + Send + Sync + 'static` bound the V5 signer wants).
use vike_model::{now_ms, now_ns};

/// Fetch `symbol`'s REAL tick/lot grid + contract value (`ct_val`) from OKX public `instruments`
/// (SWAP). `None` on any network/parse failure or unknown symbol → the caller uses its fallback. Lets
/// the adapter size + price any SWAP inst correctly, not just the demo-BTC default. Public (unsigned).
///
/// `mainnet` is the already-resolved `OKX_MAINNET` verdict the mount threaded in
/// (`vike_mount::make_engine` resolves it ONCE via [`crate::perp::mainnet_enabled`]) — never
/// re-read here (STEP 2), so the grid universe can never disagree with the exec path's.
pub fn fetch_okx_instrument(symbol: &str, mainnet: bool) -> Option<(SymbolProperties, f64)> {
    fetch_okx_instrument_with_cap(symbol, mainnet)
        .map(|(properties, ct_val, _cap)| (properties, ct_val))
}

/// [`fetch_okx_instrument`] plus the venue's published account-leverage CEILING for `symbol` (the
/// instrument's `lever` field) — the SAME single `public/instruments` request, since all three
/// values ride the same row. `None` for the cap means the instrument carried no usable `lever`
/// (OKX omits it on SPOT/OPTION); the outer `None` still means the fetch/parse failed entirely.
///
/// This exists rather than widening [`fetch_okx_instrument`]'s tuple so every existing caller
/// (`vike_mount::make_engine`'s `RiskLimits` + `ct_val` pre-fetch, the live smokes) is untouched,
/// and so the cap costs no second round-trip anywhere. Consumed by [`run`] via
/// `vike_bridge_core::leverage::clamp_to_venue_cap`.
pub fn fetch_okx_instrument_with_cap(
    symbol: &str,
    mainnet: bool,
) -> Option<(SymbolProperties, f64, Option<f64>)> {
    // Grid host is shared, but the demo-vs-mainnet instrument universe is selected by the
    // `x-simulated-trading` header — follow the same switch the exec path uses so a mainnet mount
    // sizes/prices on the mainnet grid; `false` ⇒ demo (header present, byte-identical).
    let info = UreqOkxTransport::new(simulated(mainnet))
        .public(REST, PATH_INSTRUMENTS, &[("instType", "SWAP".into()), ("instId", symbol.into())])
        .ok()?;
    let data = unwrap_okx(info).map(|d| serde_json::json!({ "data": d })).ok()?;
    parse_okx_perp_instruments(&data).get(symbol).map(|i| (i.properties, i.ct_val, i.max_leverage))
}

/// One funding-poll iteration: pull new bills and forward them to the core ingest. `Err` only when
/// the core is gone (the ingest receiver dropped) — the caller's loop then exits instead of polling
/// forever into a void, mirroring every other pump thread's self-exit.
///
/// A fetch failure is retried next cadence and warned ONCE per consecutive-failure streak
/// (`fail_streak` counts it; a success resets it) — a 60s background thread, never the hot fold,
/// so one warn per streak is both visible and bounded.
fn poll_funding_once<T: OkxTransport>(
    poller: &mut OkxFundingPoller<'_, T>,
    events: &EventSender,
    fail_streak: &mut u32,
) -> Result<(), CoreGone> {
    match poller.poll() {
        Err(e) => {
            *fail_streak += 1;
            if *fail_streak == 1 {
                tracing::warn!(
                    target: "vike_okx",
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

/// Spawn the funding-bill background poll thread (attached via
/// [`vike_bridge_core::exec_actor::ExecActor::with_background`]): its OWN REST client (never
/// shares the submit/resync/sentinel clients on the command thread), polling
/// [`FUNDING_POLL_INTERVAL`] until `stop` fires or the core goes away. `properties`/`ct_val` are
/// unused by the bills GET (a symbol-scoped read, no order formatting), so defaults are fine.
///
/// The poller is floored at THIS spawn instant (`now_ms()`): bills that posted before the mount
/// are already inside the authoritative venue balance the account seeds from, so emitting them
/// would double-count — see `crate::funding`'s module doc (the restart law).
///
/// CROSS-MOUNT HAZARD (for a future multi-mount-per-venue-account world): the bills query is
/// `instId`-scoped, so distinct symbols on one account do NOT collide — but TWO mounts of the
/// SAME instId sharing an account would each emit that symbol's bills → duplication through the
/// non-idempotent `apply_funding`. Whoever adds multi-mount must keep this lane one-poller-per-
/// (account, instId).
fn spawn_funding_poll(
    creds: Credentials,
    symbol: String,
    events: EventSender,
    mainnet: bool,
) -> (Arc<AtomicBool>, thread::JoinHandle<()>) {
    let stop = Arc::new(AtomicBool::new(false));
    let stop_thread = stop.clone();
    let join = thread::Builder::new()
        .name("okx-funding-poll".to_string())
        .spawn(move || {
            let rest = OkxPerpRest {
                signer: OkxV5Signer::new(&creds, now_ms),
                // The mount's already-resolved `OKX_MAINNET` verdict, captured at spawn: armed
                // drops the `x-simulated-trading` header; `false` ⇒ demo (present). This thread
                // never re-reads global env, so it can never poll a different network than the
                // exec thread trades on.
                transport: UreqOkxTransport::new(simulated(mainnet)),
                base_url: REST.to_string(),
                symbol: symbol.clone(),
                properties: SymbolProperties::default(),
                ct_val: 0.0,
                // INERT here: this client only GETs `/api/v5/account/bills` and never calls
                // `set_leverage` (the field's sole reader), so the fallback is correct by
                // construction — the operator's value rides the exec thread's client below.
                leverage: DEFAULT_LEVERAGE,
                broker_code: None, // the bills GET never builds an order body
            };
            // Floor = spawn instant: only bills landing WHILE mounted are folded.
            let mut poller = OkxFundingPoller::new(&rest, &symbol, now_ms());
            let mut fail_streak = 0u32;
            while !stop_thread.load(Ordering::Relaxed) {
                if poll_funding_once(&mut poller, &events, &mut fail_streak).is_err() {
                    return; // core gone — self-exit like every other pump thread
                }
                sleep_unless_stopped(&stop_thread, FUNDING_POLL_INTERVAL);
            }
        })
        .expect("spawn okx-funding-poll thread");
    (stop, join)
}

/// Live OKX SWAP-perp exec client. `submit`/`cancel` enqueue onto the REST thread (non-blocking);
/// every venue event returns through the core ingest. Dropping it (or `detach`) stops the REST thread
/// AND the user-data pump — deterministic teardown via the owned [`ExecActor`].
pub struct OkxExecutionClient(ExecActor);

impl OkxExecutionClient {
    /// Spawn the exec thread (fetches the instrument grid, sets leverage, starts the user-data pump,
    /// then drains commands). `symbol` is the SWAP instId (e.g. `"BTC-USDT-SWAP"`);
    /// `fallback_properties`/`fallback_ct_val` are used ONLY if the exec thread's `instruments` fetch
    /// fails (demo-BTC defaults) — the real grid + contract value are fetched at startup.
    ///
    /// `mainnet` is the already-resolved `OKX_MAINNET` verdict (see
    /// [`crate::perp::mainnet_enabled`]); `false` keeps the `x-simulated-trading` header and the
    /// demo WS — the explicit, self-documenting default for a smoke/test caller, which since STEP 2
    /// can no longer be silently upgraded to mainnet by a stray exported flag.
    pub fn spawn(
        creds: Credentials,
        symbol: String,
        fallback_properties: SymbolProperties,
        fallback_ct_val: f64,
        events: EventSender,
        mainnet: bool,
    ) -> Self {
        Self::spawn_with_recorder(
            creds,
            symbol,
            fallback_properties,
            fallback_ct_val,
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
    /// startup `instruments` fetch resolves. `properties_rec` is moved into the exec thread
    /// (`PropertiesRecorder` is `Send + Sync`).
    ///
    /// `on_reconcile` (reconciliation-activation Task 7) is threaded straight down to the venue's
    /// `run_resync_supervisor` call — `Some` pokes the reconcile driver after every reconnect's
    /// event-replay settles; `None` (every non-recon caller, e.g. [`Self::spawn`]) reproduces the
    /// pre-Task-7 event-replay-only behavior byte-for-byte.
    ///
    /// `broker_code` (unified cross-venue attribution, task 4) is the OKX FD-broker code resolved
    /// ONCE by the caller (`vike-mount`'s `make_engine`, via `attribution_code_from(vars, "okx")`)
    /// and stamped as the wire `tag` on every order this mount submits; `None` (unconfigured, or
    /// every non-mount caller e.g. [`Self::spawn`]) reproduces the pre-task-4 wire body
    /// byte-for-byte (no `tag` key at all).
    ///
    /// `mainnet` (STEP 2 of the `{VENUE}_MAINNET` convergence) is likewise resolved ONCE at the
    /// mount site ([`crate::perp::mainnet_enabled`], which reads the process env AND the workspace
    /// `.env` map) and threaded down to EVERY network selection this client makes — the confirm
    /// re-query client, the funding poller, and the exec thread's REST clients + private WS URL. No
    /// thread below re-reads the flag, so one mount can never end up signing mainnet credentials
    /// against the simulated-trading header. `false` ⇒ demo, byte-identical to before this switch
    /// existed.
    ///
    /// `leverage` is the account leverage the exec thread posts once at start
    /// (`POST /api/v5/account/set-leverage`, `mgnMode: "cross"`), resolved ONCE at the mount site
    /// from the operator's `[risk]` budget via [`leverage_for`] — so the venue account and the
    /// RiskGate's margin math run on the SAME number. Pass [`DEFAULT_LEVERAGE`] (what
    /// [`Self::spawn`] does) for the historical literal.
    #[allow(clippy::too_many_arguments)] // task 4's broker_code + STEP-2's mainnet pushed this past the default threshold
    pub fn spawn_with_recorder(
        creds: Credentials,
        symbol: String,
        fallback_properties: SymbolProperties,
        fallback_ct_val: f64,
        events: EventSender,
        properties_rec: Option<Arc<vike_data::PropertiesRecorder>>,
        on_reconcile: Option<mpsc::Sender<()>>,
        broker_code: Option<String>,
        mainnet: bool,
        leverage: f64,
    ) -> Self {
        // Active-confirm re-query (audit ex1 residual): the ExecActor runs this on its OWN worker
        // thread when the core issues `Command::ConfirmOrder`, so a wedged adapter is prodded with NO
        // network on the core fold. It builds a fresh short-lived REST client (like the resync +
        // sentinel clients) and reuses the exact ambiguous-submit re-query via `confirm_order`. The
        // status GET is keyed on clOrdId only, so `fallback_properties`/`fallback_ct_val` (unused by the
        // GET) are fine.
        let confirm: ConfirmFn = {
            let creds = creds.clone();
            let symbol = symbol.clone();
            Arc::new(move |coid: &str| {
                let rest = OkxPerpRest {
                    signer: OkxV5Signer::new(&creds, now_ms),
                    // The mount's resolved verdict, captured — never re-read on this worker. Armed
                    // drops the sim header; `false` ⇒ demo (present), byte-identical.
                    transport: UreqOkxTransport::new(simulated(mainnet)),
                    base_url: REST.to_string(),
                    symbol: symbol.clone(),
                    properties: fallback_properties,
                    ct_val: fallback_ct_val,
                    // INERT here: a status re-query never calls `set_leverage` (the field's sole
                    // reader), so the fallback is correct by construction.
                    leverage: DEFAULT_LEVERAGE,
                    broker_code: None, // a status re-query never builds an order body
                };
                rest.confirm_order(coid, now_ms())
            })
        };
        // Funding-bill poll: perp-only by construction (this crate's ONLY exec client is the
        // SWAP-perp one — spot has no exec client here), so spawning it here is inherently scoped
        // to perp mounts.
        let (funding_stop, funding_join) =
            spawn_funding_poll(creds.clone(), symbol.clone(), events.clone(), mainnet);
        let actor = ExecActor::spawn("okx-exec", events.clone(), move |rx| {
            run(
                creds,
                symbol,
                fallback_properties,
                fallback_ct_val,
                events,
                rx,
                properties_rec,
                on_reconcile,
                broker_code,
                mainnet,
                leverage,
            )
        })
        .with_confirm(confirm)
        .with_background(funding_stop, funding_join);
        Self(actor)
    }
}

impl ExecutionClient for OkxExecutionClient {
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
/// commands until `Shutdown`. ALL network I/O lives HERE — off the single-writer core thread.
#[allow(clippy::too_many_arguments)] // Task 7's on_reconcile / task 4's broker_code pushed this past the default threshold
fn run(
    creds: Credentials,
    symbol: String,
    fallback_properties: SymbolProperties,
    fallback_ct_val: f64,
    events: EventSender,
    rx: Receiver<ExecCommand>,
    properties_rec: Option<Arc<vike_data::PropertiesRecorder>>,
    on_reconcile: Option<mpsc::Sender<()>>,
    broker_code: Option<String>,
    mainnet: bool,
    leverage: f64,
) {
    // Fetch the venue's REAL grid + contract value for `symbol` (off the core thread); fall back to the
    // caller's demo-BTC defaults only if it fails, so orders size/price on the right lot/tick/ct_val.
    // Only the REAL fetched grid is ever recorded into the PIT properties store (never the fallback).
    // The SAME response also carries `lever` — this instrument's published account-leverage
    // ceiling. Captured here (free: one request, three fields) and applied to the startup
    // `set_leverage` below. A failed fetch yields `None`, i.e. no cap and no clamp.
    let (properties, ct_val, venue_leverage_cap) = match fetch_okx_instrument_with_cap(
        &symbol, mainnet,
    ) {
        Some((real, real_ct_val, cap)) => {
            vike_data::PropertiesRecorder::record_opt(
                &properties_rec,
                "okx",
                &symbol,
                real,
                now_ns(),
            );
            (real, real_ct_val, cap)
        }
        None => {
            tracing::warn!(target: "vike_okx", %symbol, "instruments fetch failed; using fallback properties+ct_val");
            (fallback_properties, fallback_ct_val, None)
        }
    };
    // Ceiling: the mount resolved the operator's REQUEST, this is the venue's own limit for this
    // instrument. `min(request, cap)` when a cap is known; UNKNOWN (no `lever`, or the fetch above
    // failed) ⇒ the requested value rides through untouched, byte-identical to before this clamp
    // existed. `vike_bridge_core::leverage`'s module doc is the authority, including the residual
    // it leaves open (the RiskGate still sizes against the un-clamped request).
    let leverage = vike_bridge_core::leverage::clamp_to_venue_cap(
        leverage,
        venue_leverage_cap,
        crate::perp::VENUE,
        &symbol,
    );
    // Demo/mainnet selector for this mount's PRIVATE exec + user-data WS. The flag was resolved
    // ONCE at the mount (`OKX_MAINNET`, exact `"1"` from process env or the workspace `.env`,
    // process winning) and threaded in — this thread never re-reads it. OKX shares the REST host
    // across both, so `sim` (the `x-simulated-trading` header) and the WS URL are what switch.
    // `false` ⇒ demo (`sim` = true, demo WS) — byte-identical to the historical hardcoded
    // `new(true)` + `DEMO_WS`.
    let sim = simulated(mainnet);
    let rest = OkxPerpRest {
        signer: OkxV5Signer::new(&creds, now_ms),
        transport: UreqOkxTransport::new(sim),
        base_url: REST.to_string(),
        symbol: symbol.clone(),
        properties,
        ct_val,
        // The mount-resolved operator leverage ([`leverage_for`]) — this is the ONE client that
        // posts it (`set_leverage` just below); unset profile ⇒ `DEFAULT_LEVERAGE`, unchanged.
        leverage,
        // The ONE client that ever builds an order body (submit_order, on `run_loop` below) —
        // carries the resolved FD-broker code so submitted orders stamp `tag`.
        broker_code: broker_code.clone(),
    };
    // Network, off the core thread; a benign "leverage unchanged" is tolerated inside.
    // Best-effort, UNCHANGED control flow (a rejection never kills the mount) — but the discarded
    // `Result` is now WARNED on: a silently-failed leverage POST is the exact divergence this
    // wiring exists to close (the account keeps its old leverage while the RiskGate sizes against
    // the configured one), and binance/aster already warn here. ⚠ OKX re-raises on ANY non-`'0'`
    // code (see `OkxPerpRest::set_leverage`), so this can also fire on a benign repeat — a warn,
    // never an error, for exactly that reason.
    if let Err(e) = rest.set_leverage() {
        tracing::warn!(target: "vike_okx", %symbol, leverage, code = e.code, msg = %e.msg, "set_leverage failed (continuing) — the account keeps its CURRENT leverage while the RiskGate sizes against the configured one");
    }
    // Audit A3: a SEPARATE REST client for the reconnect-resync supervisor (the submit client lives on
    // this thread) — replays recent order/fill history after every WS reconnect so a fill/cancel that
    // landed during the gap is recovered instead of leaving a phantom-live order.
    let resync_rest = OkxPerpRest {
        signer: OkxV5Signer::new(&creds, now_ms),
        transport: UreqOkxTransport::new(sim),
        base_url: REST.to_string(),
        symbol: symbol.clone(),
        properties,
        ct_val,
        // Same mount-resolved value as the submit client; history-only, so it never posts it.
        leverage,
        broker_code: None, // history-only reads, never builds an order body
    };
    // A THIRD client for the run_loop fill-sentinel (also off the pump thread, so its history calls
    // never contend with the pump's own resync).
    let sentinel_rest = OkxPerpRest {
        signer: OkxV5Signer::new(&creds, now_ms),
        transport: UreqOkxTransport::new(sim),
        base_url: REST.to_string(),
        symbol: symbol.clone(),
        properties,
        ct_val,
        // Same mount-resolved value as the submit client; history-only, so it never posts it.
        leverage,
        broker_code: None, // history-only reads, never builds an order body
    };
    let resync_symbol = symbol.clone();
    let sentinel_symbol = symbol.clone();
    let feed = spawn_okx_perp_user_data_with_resync(
        ws_url(mainnet).to_string(),
        creds.api_key.clone(),
        creds.api_secret.clone(),
        creds.passphrase.clone().unwrap_or_default(),
        symbol,
        ct_val,
        events.clone(),
        move || okx_history_events(&resync_rest, &resync_symbol, ct_val),
        on_reconcile,
    );
    run_loop(
        &rest,
        &events,
        rx,
        || okx_history_events(&sentinel_rest, &sentinel_symbol, ct_val),
        FILL_SENTINEL,
    );
    let _ = feed.shutdown();
}

#[cfg(test)]
mod tests {
    //! OKX-specific tests: the PIT properties recording. The generic `run_loop` command→event tests
    //! (mock `VenueRest`) moved WITH `run_loop` to `vike_bridge_core::exec_actor::run_loop_tests`;
    //! the okx reality-tie (`caps_for("okx").supports_modify`) lives in the lib `caps_test`.
    use super::*;

    /// The leverage this arm POSTs to `/api/v5/account/set-leverage` is the OPERATOR's
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

    /// PIT filter recording (task 5): `record_properties` writes the REAL fetched grid into the store
    /// when a recorder is present, keyed by venue `"okx"`. Uses the DataFusion-free `MemHistStore`
    /// test double (vike-data's `test-support` dev-feature) so this venue's default build never pulls
    /// DataFusion in.
    #[test]
    fn okx_records_fetched_properties_when_recorder_present() {
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
            "okx",
            "BTC-USDT-SWAP",
            f,
            1_577_836_800_000_000_000,
        );
        assert_eq!(
            store
                .scan_symbol_properties("okx", "BTC-USDT-SWAP", vike_data::TsRange::all())
                .unwrap(),
            vec![(1_577_836_800_000i64, f)]
        );
    }

    #[test]
    fn okx_no_recorder_writes_nothing() {
        use std::sync::Arc;
        use vike_data::HistStore;
        let store = Arc::new(vike_data::MemHistStore::new());
        vike_data::PropertiesRecorder::record_opt(
            &None,
            "okx",
            "BTC-USDT-SWAP",
            SymbolProperties::default(),
            1_577_836_800_000_000_000,
        );
        assert!(store
            .scan_symbol_properties("okx", "BTC-USDT-SWAP", vike_data::TsRange::all())
            .unwrap()
            .is_empty());
    }

    /// A fake [`OkxTransport`] that answers `/api/v5/account/bills` with a canned funding bill (or
    /// an empty list) — the offline twin of the live `OkxFundingPoller` smoke, proving
    /// `poll_funding_once`'s spawn glue (not just the pure decoder, which `r6_okx_parity.rs`
    /// already pins if present).
    struct FakeFundingTransport {
        rows: Vec<serde_json::Value>,
        /// When `Some`, assert the query carries this exact `begin` (the floor optimization
        /// param); `None` asserts the param is ABSENT (a floorless probe's query, unchanged).
        expect_begin: Option<String>,
    }

    impl OkxTransport for FakeFundingTransport {
        fn signed(
            &self,
            _base_url: &str,
            path: &str,
            _method: &str,
            params: &[(&str, serde_json::Value)],
            _signer: &OkxV5Signer,
        ) -> Result<serde_json::Value, vike_bridge_core::transport::VenueApiError> {
            assert_eq!(path, crate::perp::PATH_BILLS);
            let begin = params.iter().find(|(k, _)| *k == "begin").map(|(_, v)| v.clone());
            match &self.expect_begin {
                Some(want) => assert_eq!(begin, Some(serde_json::json!(want))),
                None => assert_eq!(begin, None, "a floorless poll must not send begin"),
            }
            Ok(serde_json::json!({"code": "0", "msg": "", "data": self.rows}))
        }

        fn public(
            &self,
            _base_url: &str,
            _path: &str,
            _params: &[(&str, String)],
        ) -> Result<serde_json::Value, vike_bridge_core::transport::VenueApiError> {
            unreachable!("the funding poll never calls the public GET")
        }
    }

    fn funding_bill(bill_id: &str, pnl: &str, ts: &str) -> serde_json::Value {
        serde_json::json!({
            "billId": bill_id, "type": "8", "instId": "BTC-USDT-SWAP",
            "pnl": pnl, "balChg": pnl, "ts": ts
        })
    }

    /// The spawn glue's one poll iteration forwards decoded `Event::Funding`s onto the core
    /// ingest lane — the path a background poll thread drives every cadence.
    #[test]
    fn poll_funding_once_forwards_decoded_events() {
        let rest = OkxPerpRest {
            signer: OkxV5Signer::new(
                &Credentials {
                    api_key: "k".into(),
                    api_secret: "s".into(),
                    passphrase: Some("p".into()),
                },
                || 0,
            ),
            transport: FakeFundingTransport {
                rows: vec![funding_bill("b1", "0.42", "1000")],
                expect_begin: None,
            },
            base_url: "http://example.invalid".to_string(),
            symbol: "BTC-USDT-SWAP".to_string(),
            properties: SymbolProperties::default(),
            ct_val: 0.0,
            leverage: DEFAULT_LEVERAGE,
            broker_code: None,
        };
        let mut poller = OkxFundingPoller::new(&rest, "BTC-USDT-SWAP", 0);
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
                assert_eq!(f.venue, "okx");
                assert_eq!(f.symbol, "BTC-USDT-SWAP");
                assert_eq!(f.amount, 0.42, "received-positive, no sign flip");
            }
            other => panic!("expected one Event::Funding, got {other:?}"),
        }

        // A second poll with the SAME bill (as a real re-fetch might return before settling)
        // yields nothing new — the dedup-by-billId guard the live poller already owns.
        assert!(poll_funding_once(&mut poller, &events, &mut streak).is_ok());
        assert!(ingest.try_recv().is_err(), "the already-seen bill must not re-emit");
    }

    /// The restart law: a poller floored at its spawn instant (as `spawn_funding_poll` does) must
    /// NEVER emit a bill that posted BEFORE the floor — those are already embedded in the
    /// authoritative venue balance — while a bill AT/after the floor is emitted exactly once.
    /// Simulates the (re)start-with-empty-seen-set case the in-memory billId-dedup cannot cover.
    #[test]
    fn poll_funding_skips_bills_before_the_spawn_floor() {
        const FLOOR: i64 = 5_000;
        let rest = OkxPerpRest {
            signer: OkxV5Signer::new(
                &Credentials {
                    api_key: "k".into(),
                    api_secret: "s".into(),
                    passphrase: Some("p".into()),
                },
                || 0,
            ),
            transport: FakeFundingTransport {
                // The venue answers with history straddling the floor (as the default window
                // would on a restart): pre-floor, exactly-at-floor, and post-floor bills.
                rows: vec![
                    funding_bill("old", "9.99", "1000"),
                    funding_bill("edge", "0.10", "5000"),
                    funding_bill("new", "0.42", "9000"),
                ],
                // The floor also rides as the begin request param (the optimization).
                expect_begin: Some(FLOOR.to_string()),
            },
            base_url: "http://example.invalid".to_string(),
            symbol: "BTC-USDT-SWAP".to_string(),
            properties: SymbolProperties::default(),
            ct_val: 0.0,
            leverage: DEFAULT_LEVERAGE,
            broker_code: None,
        };
        let mut poller = OkxFundingPoller::new(&rest, "BTC-USDT-SWAP", FLOOR);
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
                assert_eq!(new.amount, 0.42);
            }
            other => panic!("expected exactly the at/post-floor bills, got {other:?}"),
        }

        // The next cadence re-serves the same window: nothing re-emits (billId-dedup) and the
        // pre-floor bill stays excluded.
        poll_funding_once(&mut poller, &events, &mut streak).expect("core is alive");
        assert!(ingest.try_recv().is_err(), "no duplicates on the second poll");
    }

    /// When the core is gone (ingest receiver dropped), the poll glue reports `Err(CoreGone)` so
    /// its background thread's loop can self-exit instead of polling forever into a void.
    #[test]
    fn poll_funding_once_reports_core_gone() {
        let rest = OkxPerpRest {
            signer: OkxV5Signer::new(
                &Credentials {
                    api_key: "k".into(),
                    api_secret: "s".into(),
                    passphrase: Some("p".into()),
                },
                || 0,
            ),
            transport: FakeFundingTransport {
                rows: vec![funding_bill("b2", "0.1", "1000")],
                expect_begin: None,
            },
            base_url: "http://example.invalid".to_string(),
            symbol: "BTC-USDT-SWAP".to_string(),
            properties: SymbolProperties::default(),
            ct_val: 0.0,
            leverage: DEFAULT_LEVERAGE,
            broker_code: None,
        };
        let mut poller = OkxFundingPoller::new(&rest, "BTC-USDT-SWAP", 0);
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
        impl OkxTransport for FailingTransport {
            fn signed(
                &self,
                _base_url: &str,
                _path: &str,
                _method: &str,
                _params: &[(&str, serde_json::Value)],
                _signer: &OkxV5Signer,
            ) -> Result<serde_json::Value, vike_bridge_core::transport::VenueApiError> {
                Err(vike_bridge_core::transport::VenueApiError {
                    code: 0,
                    msg: "connect refused".to_string(),
                })
            }

            fn public(
                &self,
                _base_url: &str,
                _path: &str,
                _params: &[(&str, String)],
            ) -> Result<serde_json::Value, vike_bridge_core::transport::VenueApiError> {
                unreachable!("the funding poll never calls the public GET")
            }
        }
        let rest = OkxPerpRest {
            signer: OkxV5Signer::new(
                &Credentials {
                    api_key: "k".into(),
                    api_secret: "s".into(),
                    passphrase: Some("p".into()),
                },
                || 0,
            ),
            transport: FailingTransport,
            base_url: "http://example.invalid".to_string(),
            symbol: "BTC-USDT-SWAP".to_string(),
            properties: SymbolProperties::default(),
            ct_val: 0.0,
            leverage: DEFAULT_LEVERAGE,
            broker_code: None,
        };
        let mut poller = OkxFundingPoller::new(&rest, "BTC-USDT-SWAP", 0);
        let (events, mut ingest) = vike_exec::event_channel(8);
        let mut streak = 0u32;
        assert!(poll_funding_once(&mut poller, &events, &mut streak).is_ok());
        assert!(poll_funding_once(&mut poller, &events, &mut streak).is_ok());
        assert_eq!(streak, 2, "consecutive failures accumulate");
        assert!(ingest.try_recv().is_err(), "failures emit nothing");
    }
}
