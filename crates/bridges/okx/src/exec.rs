//! OKX live exec half — `ExecutionClient` over the V5 SWAP-perp REST + the private user-data WS.
//!
//! Built on the shared [`ExecActor`] scaffold: `submit`/`cancel` enqueue onto ONE dedicated OS thread
//! that drives [`OkxPerpRest`] (blocking REST off the single-writer core). `submit_order` emits
//! `[OrderSubmitted, OrderAccepted|OrderRejected]` synchronously (routing `order_type="stop"` to the
//! algo endpoint); the authoritative async fills/cancels return on the private WS via
//! [`crate::user_data::spawn_okx_perp_user_data`] straight into the core ingest.
//!
//! OKX-specifics vs Bybit: the perp is priced in CONTRACTS (`ct_val` base per contract — carried in
//! both the REST client and the WS pump), demo trading is the same REST host + the
//! `x-simulated-trading: 1` header (`UreqOkxTransport::new(true)`), and the signer needs the account
//! passphrase.
//!
//! LIVE GATE: absent credentials never reach here — the start only spawns this when
//! `vike_secrets::credentials::api_credentials` yields `Some` for the started account
//! (absent-creds-is-the-live-gate). An account with no complete key trio does not run.
//!
//! v1 scope: `submit`/`cancel` only (the DOM surface). `modify`/batch fall through to the
//! [`ExecutionClient`] defaults — a native amend/batch is a follow-up.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::Duration;

use vike_bridge_core::confirm::{DispatchLedger, Dispatched, confirm_closure};
use vike_bridge_core::exec_actor::{
    ConfirmFn, DEFAULT_FILL_SENTINEL, ExecActor, ExecCommand, run_loop,
};
use vike_bridge_core::rest::VenueRest;
use vike_bridge_core::signer::OkxV5Signer;
use vike_bridge_core::user_data::sleep_unless_stopped;
use vike_exec::{CoreGone, EventSender, ExecutionClient};
use vike_model::events::Event;
use vike_model::{OrderRequest, SymbolProperties};
use vike_secrets::credentials::Credentials;

use crate::funding::OkxFundingPoller;
use crate::perp::{
    OkxInstrument, OkxPerpRest, PATH_INSTRUMENTS, REST, parse_okx_perp_instruments, simulated,
    ws_url,
};
use crate::transport::{OkxTransport, UreqOkxTransport, unwrap_okx};
use crate::user_data::spawn_okx_perp_user_data_with_resync;

/// FALLBACK cross-margin leverage the exec thread sets once at start — the literal this arm posted
/// to `/api/v5/account/set-leverage` unconditionally before the operator's budget was threaded in
/// (demo default; matches the smoke tests). Used ONLY when the operator expressed no leverage
/// intent, so a start with no `[risk] max_leverage` is byte-identical to every start before
/// [`leverage_for`] existed. NEVER read on the submit path — `OkxPerpRest::leverage` is consulted
/// by `set_leverage` and nothing else, which is why the read-only clients below (funding poller,
/// confirm re-query) can carry it inertly.
const DEFAULT_LEVERAGE: f64 = 2.0;
/// Rows of recent order/fill history the audit-A3 resync replays after each WS reconnect.
const RESYNC_HISTORY_LIMIT: u32 = 50;
/// Funding-bill poll cadence (REST `/api/v5/account/bills`; funding is NOT on any OKX WS channel —
/// see `crate::funding`'s module doc). Settlements post at most every 8h, so 60s keeps live equity
/// current without hammering the endpoint.
const FUNDING_POLL_INTERVAL: Duration = Duration::from_secs(60);

/// The account leverage this arm POSTs at startup, resolved from the operator's `[risk]` budget.
///
/// Pure, and `pub` on purpose — the SAME shape as the start's ceiling (decision 0095):
/// `crate::adapter`'s `OkxVenueAdapter::start`, handed the `ProfileRisk` in its request, calls this
/// ONCE per start, then threads the resolved `f64` into
/// [`OkxExecutionClient::spawn_with_recorder`]. Nothing inside the adapter
/// reaches for the profile, so the number the venue account is configured with and the number the
/// RiskGate sizes against (`ProfileRisk::im_requirement`, `im = 1.0 / max_leverage`) can never
/// disagree — that disagreement was the bug this exists to close.
///
/// Unset (`None`, or a profile that never mentions `max_leverage`) ⇒ [`DEFAULT_LEVERAGE`],
/// byte-identical to before. See `vike_bridge_core::leverage` for the full rule.
pub fn leverage_for(profile: Option<&vike_model::ProfileRisk>) -> f64 {
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
/// `mainnet` is the already-resolved ceiling verdict the start threaded in (decision 0095:
/// `crate::adapter`'s `OkxVenueAdapter::start` resolves it ONCE, from the account's `AdapterInputs::tier`)
/// — never re-read here, so the grid universe can never disagree with the exec path's.
pub fn fetch_okx_instrument(symbol: &str, mainnet: bool) -> Option<(SymbolProperties, f64)> {
    fetch_okx_instrument_row(symbol, mainnet).map(|i| (i.properties, i.ct_val))
}

/// [`fetch_okx_instrument`]'s WHOLE row for `symbol`: the grid, `ctVal`, the venue's published
/// account-leverage CEILING (the instrument's `lever` field; `None` when the instrument carried no
/// usable one, OKX omits it on SPOT/OPTION) and the row's `ctType` word — the SAME single
/// `public/instruments` request, since all of them ride the same row. The outer `None` means the
/// fetch/parse failed entirely.
///
/// This exists rather than widening [`fetch_okx_instrument`]'s tuple so every existing caller
/// (`crate::adapter`'s `RiskLimits` + `ct_val` pre-fetch, the live smokes) is untouched, and so
/// neither the cap nor the contract type costs a second round-trip anywhere. Consumed by [`run`]:
/// the cap via `vike_bridge_core::leverage::clamp_to_venue_cap`, the type by
/// [`inverse_swap_refusal`].
pub fn fetch_okx_instrument_row(symbol: &str, mainnet: bool) -> Option<OkxInstrument> {
    // Grid host is shared, but the demo-vs-mainnet instrument universe is selected by the
    // `x-simulated-trading` header — follow the same switch the exec path uses so a mainnet start
    // sizes/prices on the mainnet grid; `false` ⇒ demo (header present, byte-identical).
    let info = UreqOkxTransport::new(simulated(mainnet))
        .public(REST, PATH_INSTRUMENTS, &[("instType", "SWAP".into()), ("instId", symbol.into())])
        .ok()?;
    let data = unwrap_okx(info).map(|d| serde_json::json!({ "data": d })).ok()?;
    parse_okx_perp_instruments(&data).get(symbol).cloned()
}

/// **The exec REFUSAL for a coin-margined (inverse) swap**, on the venue's own word (`ctType`)
/// and before anything is built (decision 0170, plan S0.1).
///
/// An inverse swap (`BTC-USD-SWAP`) counts a contract in USD — `ctVal` is 100 and `ctValCcy` is
/// `USD` — and this adapter's whole order path reads a quantity as BASE and divides it by `ctVal` in
/// [`OkxPerpRest::to_contracts`]: a 0.5 BTC order would become 0.005 contracts, floor to zero, and
/// the grid `properties_in_base` hands the gate is 100x too coarse. Nothing refused it; the order
/// formatted on a real grid, for the right instrument, in the wrong unit. A twin of bybit's
/// `non_linear_perpetual_refusal`, and for the same reason it is a refusal and not a fix: the
/// decision supplies the vocabulary (`ContractValue`), not the permission to trade a new class of
/// contract.
///
/// Pure: the caller supplies the row the fetch returned. **A row that could not be fetched is
/// REFUSED too (fail closed)**: with no row, `ctType` is unknown, and the fallback grid is BTC-USDT's —
/// a `BTC-USD-SWAP` engine would mount on it in the wrong unit. An ABSENT `ctType` on a row that WAS
/// fetched is NOT refused: that is a row the venue changed, and refusing on a missing field would
/// take a working linear start off the venue for a parse miss. A start refused for a failed fetch
/// is retried by the supervisor's next start.
#[must_use]
pub fn inverse_swap_refusal(symbol: &str, row: Option<&OkxInstrument>) -> Option<String> {
    let Some(row) = row else {
        return Some(format!(
            "okx exec REFUSES {symbol:?}: the instrument row could not be fetched, so its ctType is \
             unknown and an inverse swap (a contract counted in USD) would mount on the fallback \
             BTC-USDT grid in the wrong unit — nothing is being sent until the row can be read"
        ));
    };
    match row.contract_type.as_deref() {
        None | Some("linear") => None,
        Some(other) => Some(format!(
            "okx exec REFUSES {symbol:?}: the venue reports ctType {other:?}, a contract counted in \
             the QUOTE currency (`ctVal` is USD on an inverse swap) where this adapter reads a \
             quantity as base asset and divides it by `ctVal` — so nothing is being sent. The \
             market-data side of this instrument works. Trading it needs the order path to carry \
             decision 0170's `ContractValue` (an inverse value, a quote-denominated quantity) end to \
             end, behind demo smokes."
        )),
    }
}

/// **The exec thread when [`inverse_swap_refusal`] fired** — a [`VenueRest`] that never touches the
/// wire and never lets an order vanish: `OrderSubmitted`, then a TERMINAL `OrderRejected` carrying
/// the reason (the venue-adapter contract: no order may silently vanish). A twin of bybit's
/// `RefusingRest`, kept private to its bridge: the shared home (`vike-bridge-core`) changes in one
/// coordinated PR informed by every consumer.
struct RefusingRest {
    why: String,
}

impl VenueRest for RefusingRest {
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

    /// `Ok(())`: nothing was ever resting, and cancelling a refused order must not report a second
    /// failure.
    fn cancel_order(
        &self,
        _client_order_id: &str,
    ) -> Result<(), vike_bridge_core::transport::VenueApiError> {
        Ok(())
    }
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
/// The poller is floored at THIS spawn instant (`now_ms()`): bills that posted before the start
/// are already inside the authoritative venue balance the account seeds from, so emitting them
/// would double-count — see `crate::funding`'s module doc (the restart law).
///
/// CROSS-MOUNT HAZARD (for a future multi-mount-per-venue-account world): the bills query is
/// `instId`-scoped, so distinct symbols on one account do NOT collide — but TWO starts of the
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
                // The start's already-resolved mainnet verdict (decision 0095: the account's
                // `AdapterInputs::tier`, as `crate::adapter` resolves it), captured at spawn: armed
                // drops the `x-simulated-trading` header; `false` ⇒ demo (present). This thread
                // never re-resolves it, so it can never poll a different network than the exec
                // thread trades on.
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
            // Floor = spawn instant: only bills landing WHILE started are folded.
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
    /// Hand this client the HALT sentinel it watches: the venue's start passes
    /// `AdapterInputs::process.halt_path`, the one file `vike-tradehub-core` resolved for the whole process
    /// (`vike_bridge_core::exec_actor::ExecActor::with_halt_path` carries the contract). A client
    /// built without it watches NOTHING — `crates/vike-ops/tests/settings_secrets/bridge_inputs_gate.rs` holds that
    /// every live start calls this.
    #[must_use]
    pub fn with_halt_path(self, path: std::path::PathBuf) -> Self {
        Self(self.0.with_halt_path(path))
    }

    /// Spawn the exec thread (fetches the instrument grid, sets leverage, starts the user-data pump,
    /// then drains commands). `symbol` is the SWAP instId (e.g. `"BTC-USDT-SWAP"`);
    /// `fallback_properties`/`fallback_ct_val` are used ONLY if the exec thread's `instruments` fetch
    /// fails (demo-BTC defaults) — the real grid + contract value are fetched at startup.
    ///
    /// `mainnet` is the already-resolved ceiling verdict (decision 0095: the start's ceiling
    /// chooses the network); `false` keeps the `x-simulated-trading` header and the demo WS — the
    /// explicit, self-documenting default for a smoke/test caller, which no environment variable
    /// can silently upgrade.
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
    /// `run_resync_supervisor` call — `Some` fires the recon trigger after every reconnect's
    /// event-replay settles; `None` (every non-recon caller, e.g. [`Self::spawn`]) reproduces the
    /// pre-Task-7 event-replay-only behavior byte-for-byte.
    ///
    /// `broker_code` (unified cross-venue attribution, task 4) is the OKX FD-broker code resolved
    /// ONCE by the caller (`crate::adapter`'s `OkxVenueAdapter::start`, via `attribution_code_from`)
    /// and stamped as the wire `tag` on every order this start submits; `None` (unconfigured, or
    /// every non-mount caller e.g. [`Self::spawn`]) reproduces the pre-task-4 wire body
    /// byte-for-byte (no `tag` key at all).
    ///
    /// `mainnet` (decision 0095: the start's ceiling chooses the network) is likewise resolved ONCE
    /// at the start site and threaded down to EVERY network selection this client makes — the confirm
    /// re-query client, the funding poller, and the exec thread's REST clients + private WS URL. No
    /// thread below re-reads the flag, so one start can never end up signing mainnet credentials
    /// against the simulated-trading header. `false` ⇒ demo, byte-identical to before this switch
    /// existed.
    ///
    /// `leverage` is the account leverage the exec thread posts once at start
    /// (`POST /api/v5/account/set-leverage`, `mgnMode: "cross"`), resolved ONCE at the start site
    /// from the operator's `[risk]` budget via [`leverage_for`] — so the venue account and the
    /// RiskGate's margin math run on the SAME number. Pass [`DEFAULT_LEVERAGE`] (what
    /// [`Self::spawn`] does) for the historical literal.
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
        // Active-confirm re-query (audit ex1 residual), in the ONE shared form of
        // `vike_bridge_core::confirm` (the form the proposed decision 0171 describes): the ExecActor runs the closure on its OWN worker
        // thread when the core issues `Command::ConfirmOrder`, so a wedged adapter is prodded with NO
        // network on the core fold. ONE REST client serves every confirm (bounded, backed off per coid),
        // over the exact ambiguous-submit re-query. `ledger` is stamped by `Dispatched` around the
        // command loop's client in `run`: "absent" rejects only a coid whose submit has FINISHED, and a
        // failed query says nothing. The status GET is keyed on clOrdId only, so
        // `fallback_properties`/`fallback_ct_val` (unused by the GET) are fine.
        let ledger = Arc::new(DispatchLedger::default());
        let confirm: ConfirmFn = confirm_closure(
            OkxPerpRest {
                signer: OkxV5Signer::new(&creds, now_ms),
                // The start's resolved verdict, captured — never re-read on this worker. Armed
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
            },
            ledger.clone(),
            |_| true,
        );
        // Funding-bill poll: perp-only by construction (this crate's ONLY exec client is the
        // SWAP-perp one — spot has no exec client here), so spawning it here is inherently scoped
        // to perp starts.
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
                ledger,
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

/// The exec thread's SUBMIT client — the one [`OkxPerpRest`] that places, cancels, amends and sets
/// leverage (`run_loop`'s `rest`), and so the ONE okx transport on the exec side that rides the
/// order-rate gate ([`crate::ratelimit::rest_rate_gate`], the `OKX.orders` row: 50 per 2 s, ~17%
/// under the venue's 60/2s place-order budget per UID and instrument).
///
/// ⚠ **A gate that is not built here is not on the wire.** Every smoke attaches the gate to its own
/// transport, so the gate was green everywhere a test looked and absent from the one place orders
/// leave the process: `crate::ratelimit`'s per-transport-is-per-instrument argument holds only if
/// THIS transport carries the gate, and until it did a runaway submit/cancel loop on one instId ran
/// at whatever pace the blocking transport allowed, into the venue's own refusal. Below the budget
/// the gate admits at once and delays nothing; it only holds back what the venue would refuse.
///
/// **Why only this client, and why a window of its own:**
/// - The resync, fill-sentinel and confirm clients below speak only `signed_requery`, which is
///   rate-gate-EXEMPT by design (a throttle would reintroduce the stall its short-timeout agent
///   bounds), so a gate on them would be inert; they stay as they were.
/// - The funding poller (one `signed` GET a minute) and the instrument fetch (public, one at start,
///   one at start) are reads on endpoints of their own: they draw none of the place-order budget,
///   so they do not share this window.
/// - The reconcile client carries ITS OWN window from the same factory
///   (`crate::recon_client::recon_client`). They are not merged on purpose: a reconcile burst
///   queued ahead of a cancel in one FIFO window is the order-path delay the exempt `signed_requery`
///   split exists to prevent.
/// - One window per start is the table's model (N symbols, N windows): the per-instrument bucket is
///   what the venue meters, and the 1000/2s sub-account ceiling above it is what N windows must
///   stay under (`crate::ratelimit`'s module doc has the arithmetic).
///
/// `broker_code` is the resolved FD-broker code: this is the ONE client that ever builds an order
/// body (`submit_order`, on `run_loop`), so submitted orders stamp `tag`. `leverage` is the
/// mount-resolved operator leverage ([`leverage_for`], clamped) — this is the ONE client that posts
/// it (`set_leverage` right after this call); unset profile ⇒ [`DEFAULT_LEVERAGE`], unchanged.
fn order_rest(
    creds: &Credentials,
    symbol: &str,
    properties: SymbolProperties,
    ct_val: f64,
    leverage: f64,
    broker_code: Option<String>,
    sim: bool,
) -> OkxPerpRest<UreqOkxTransport> {
    OkxPerpRest {
        signer: OkxV5Signer::new(creds, now_ms),
        transport: UreqOkxTransport::new(sim).with_rate_gate(crate::ratelimit::rest_rate_gate()),
        base_url: REST.to_string(),
        symbol: symbol.to_string(),
        properties,
        ct_val,
        leverage,
        broker_code,
    }
}

/// The exec thread: build the REST client, set leverage, start the private-WS pump, then drain
/// commands until `Shutdown`. ALL network I/O lives HERE — off the single-writer core thread.
#[expect(clippy::too_many_arguments)] // Task 7's on_reconcile / task 4's broker_code pushed this past the default threshold
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
    ledger: Arc<DispatchLedger>,
) {
    // Fetch the venue's REAL grid + contract value for `symbol` (off the core thread); fall back to the
    // caller's demo-BTC defaults only if it fails, so orders size/price on the right lot/tick/ct_val.
    // Only the REAL fetched grid is ever recorded into the PIT properties store (never the fallback).
    // The SAME response also carries `lever` — this instrument's published account-leverage
    // ceiling. Captured here (free: one request, three fields) and applied to the startup
    // `set_leverage` below. A failed fetch yields `None`, i.e. no cap and no clamp.
    let row = fetch_okx_instrument_row(&symbol, mainnet);
    // ⚠ **THE EXEC REFUSAL, on the venue's own word and before anything is built** (decision 0170):
    // an inverse swap stops here — no REST client, no `set_leverage` POST, no private WS, no signed
    // request of any kind — just a loop that rejects every command with the reason. See
    // [`inverse_swap_refusal`] for why the default without it is a wrong-UNIT acceptance.
    if let Some(why) = inverse_swap_refusal(&symbol, row.as_ref()) {
        tracing::error!(target: "vike_okx", %symbol, "{why}");
        run_loop(&RefusingRest { why }, &events, rx, Vec::new, DEFAULT_FILL_SENTINEL);
        return;
    }
    let (properties, ct_val, venue_leverage_cap) = match row
        .map(|i| (i.properties, i.ct_val, i.max_leverage))
    {
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
    // Ceiling: the start resolved the operator's REQUEST, this is the venue's own limit for this
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
    // Demo/mainnet selector for this start's PRIVATE exec + user-data WS. Decision 0095: the
    // account's tier alone decides now (`AdapterInputs::tier`, as `crate::adapter` resolves it), resolved
    // ONCE at the start and threaded in — this thread never re-resolves it. OKX shares the REST
    // host across both, so `sim` (the `x-simulated-trading` header) and the WS URL are what switch.
    // `false` ⇒ demo (`sim` = true, demo WS) — byte-identical to the historical hardcoded
    // `new(true)` + `DEMO_WS`.
    let sim = simulated(mainnet);
    let rest = order_rest(&creds, &symbol, properties, ct_val, leverage, broker_code, sim);
    // Network, off the core thread; a benign "leverage unchanged" is tolerated inside.
    // Best-effort, UNCHANGED control flow (a rejection never kills the start) — but the discarded
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
        creds.api_key.expose().clone(),
        creds.api_secret.expose().clone(),
        creds.passphrase.as_ref().map(|p| p.expose().clone()).unwrap_or_default(),
        symbol,
        ct_val,
        events.clone(),
        move || okx_history_events(&resync_rest, &resync_symbol, ct_val),
        on_reconcile,
    );
    // The gap-sentinel replays recent history `DEFAULT_FILL_SENTINEL` after the last command, to
    // recover a fill/cancel the WS lost (the first-connect subscribe race); the core dedups the
    // overlap, so a normal WS delivery makes it a no-op. A venue-free mechanism default.
    // The ledger stamps every dispatched submit, so the active confirm trusts "absent" only for one.
    run_loop(
        &Dispatched { rest, ledger },
        &events,
        rx,
        || okx_history_events(&sentinel_rest, &sentinel_symbol, ct_val),
        DEFAULT_FILL_SENTINEL,
    );
    let _ = feed.shutdown();
}

#[path = "exec_tests.rs"]
#[cfg(test)]
mod exec_tests;
