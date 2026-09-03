//! Binance live exec half — `ExecutionClient` over the signed REST + the private user-data stream,
//! routing SPOT vs USDⓈ-M PERP by a trailing `.P` on the symbol. The live twin of
//! `bybit::exec::BybitExecutionClient` (linear perp) and `okx::exec` (SWAP), assembled from the
//! existing binance parts — NOTHING here is reimplemented, this is the wiring seam:
//!
//! * SPOT (`BTCUSDT`): [`BinanceSpotRest`](crate::spot::BinanceSpotRest) over `/api/v3` +
//!   [`spawn_binance_user_data_with_resync`](crate::user_data::spawn_binance_user_data_with_resync)
//!   (the WS-API executionReport fills pump).
//! * PERP (`BTCUSDT.P`): [`BinancePerpRest`](crate::perp::BinancePerpRest) over `/fapi/v1` (native
//!   modify/batch) + [`spawn_binance_perp_user_data_with_resync`](crate::perp_user_data::spawn_binance_perp_user_data_with_resync)
//!   (the listenKey ORDER_TRADE_UPDATE fills pump).
//!
//! Built on the shared [`ExecActor`] scaffold: `submit`/`cancel`/`modify` enqueue onto ONE dedicated
//! OS thread that drives the venue REST (blocking, off the single-writer core). `submit_order` emits
//! `[OrderSubmitted, OrderAccepted|OrderRejected]` synchronously; the authoritative async
//! fills/cancels return on the private WS pump straight into the core ingest.
//!
//! `.P` ROUTING (the SAME idiom the trade/kline feeds use — see `market_feed::try_spawn`): the
//! trailing `.P` is stripped to the EXCHANGE symbol (`BTCUSDT`) that drives the venue REST/WS
//! endpoints, while the ORIGINAL `.P`-suffixed symbol stays the CORE/series label the fills carry
//! (so a perp's position/order key never collides with its spot twin). A non-`.P` symbol is
//! byte-identical to spot-only behavior.
//!
//! LIVE GATE: absent credentials never reach here — the composition root only spawns this when
//! `load_credentials_from` yields `Some` (the codebase's absent-creds-is-the-live-gate rule). With
//! no `.env` keys the venue stays paper.
//!
//! v1 scope: `submit`/`cancel`/`modify`. PERP `modify` is the NATIVE fapi amend (`PUT /fapi/v1/order`,
//! wired via `run_loop`'s `ExecCommand::Modify` → `BinancePerpRest::modify_order`); SPOT `modify` is
//! the `VenueRest` default no-op (spot has no native amend). Native fapi BATCH (submit/cancel) exists
//! on `BinancePerpRest` but is NOT reachable through the shared `ExecActor` command channel yet — it
//! carries no batch variant, so batches fan out to per-order submits via the `ExecutionClient`
//! default (same limitation as bybit; an `ExecCommand::Batch` in vike-bridge-core is the follow-up).

use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::time::Duration;

use vike_bridge_core::credentials::Credentials;
use vike_bridge_core::exec_actor::{run_loop, ExecActor, ExecCommand};
use vike_bridge_core::signer::BinanceHmacSigner;
use vike_bridge_core::transport::{RestTransport, UreqTransport};
use vike_bridge_core::Environment;
use vike_exec::{EventSender, ExecutionClient};
use vike_model::events::Event;
use vike_model::{now_ms, now_ns, OrderRequest, SymbolProperties};

use crate::family::exec_loop::split_symbol;
use crate::history::{map_binance_history, map_binance_perp_history};
use crate::perp::{
    parse_binance_perp_instruments, BinancePerpRest, DEMO_FAPI_REST, DEMO_FAPI_WS,
    MAINNET_FAPI_REST, MAINNET_FAPI_WS, PATH_EXCHANGE_INFO as FAPI_PATH_EXCHANGE_INFO,
};
use crate::perp_user_data::spawn_binance_perp_user_data_with_resync;
use crate::spot::{
    parse_symbol_properties, BinanceSpotRest, DEMO_REST, MAINNET_REST, PATH_EXCHANGE_INFO,
};
use crate::user_data::{spawn_binance_user_data_with_resync, DEMO_WS, MAINNET_WS};

/// Rows of recent order/trade history the audit-A3 resync replays after each WS reconnect.
const RESYNC_HISTORY_LIMIT: u32 = 50;
/// Gap-sentinel: after a submit/cancel, replay recent history this long later to recover a fill/
/// cancel the WS lost (esp. the first-connect subscribe race — a market order fills before the pump
/// is subscribed, and the private WS does not replay on subscribe). Dedup'd by the core, so a normal
/// WS delivery makes it a no-op.
const FILL_SENTINEL: Duration = Duration::from_secs(5);
/// FALLBACK cross-margin leverage the perp exec thread sets once at start — the literal this arm
/// posted to `/fapi/v1/leverage` unconditionally before the operator's budget was threaded in
/// (demo default; matches the smoke). Used ONLY when the operator expressed no leverage intent, so
/// a mount with no `[risk] max_leverage` is byte-identical to every mount before [`leverage_for`]
/// existed. NEVER read on the submit path — `BinancePerpRest::leverage` is consulted by
/// `set_leverage` and nothing else.
const DEFAULT_LEVERAGE: f64 = 2.0;

/// The account leverage this arm POSTs at startup, resolved from the operator's `[risk]` budget.
///
/// Pure, and `pub` on purpose — the SAME shape as [`resolve_env_from`] and
/// `crate::exec::mainnet_from`: `vike_mount::make_engine` owns the `ProfileRisk` and calls this
/// ONCE per mount, then threads the resolved `f64` into
/// [`BinanceExecutionClient::spawn_with_recorder`]. Nothing inside the adapter reaches for the
/// profile, so the number the venue account is configured with and the number the RiskGate sizes
/// against (`ProfileRisk::im_requirement`, `im = 1.0 / max_leverage`) can never disagree — that
/// disagreement was the bug this exists to close.
///
/// Unset (`None`, or a profile that never mentions `max_leverage`) ⇒ [`DEFAULT_LEVERAGE`],
/// byte-identical to before. See `vike_bridge_core::leverage` for the full rule.
///
/// ⚠ **NOT clamped to a venue ceiling, unlike bybit/okx** — `vike_bridge_core::leverage::
/// clamp_to_venue_cap` exists and this arm deliberately does not call it, because binance
/// publishes no readable cap on any response this adapter already fetches. VERIFIED LIVE
/// 2026-08-05: `GET /fapi/v1/exchangeInfo` (1.0 MB, every symbol) contains ZERO case-insensitive
/// `leverage` matches, and the real ceiling lives in the leverage BRACKETS at
/// `/fapi/v1/leverageBracket`, which answers `{"code":-2014,"msg":"API-key format invalid."}`
/// unauthenticated — i.e. a WHOLE NEW SIGNED request on the mount path, not a field ride-along.
/// Those brackets are also RISK-TIERED (`initialLeverage` falls as position notional rises), so
/// "the cap" here is a function of size, not the single flat number bybit/okx publish; clamping to
/// the tier-0 value would be a weaker and different claim. Adding that fetch is a deliberate
/// follow-up with its own cost discussion, not a doc-closing change — so a too-high request is
/// still rejected by the VENUE, which `run_perp`'s `set_leverage` warn already surfaces.
pub fn leverage_for(profile: Option<&vike_exec::ProfileRisk>) -> f64 {
    vike_bridge_core::leverage::resolve_startup_leverage(profile, DEFAULT_LEVERAGE)
}

/// The concrete REST clients this exec half drives (blocking `ureq` + HMAC signer).
type SpotRest = BinanceSpotRest<BinanceHmacSigner, UreqTransport>;
type PerpRest = BinancePerpRest<BinanceHmacSigner, UreqTransport>;

/// The four exec-side hosts (spot/perp REST + spot WS-API / perp listenKey WS), resolved from the
/// credentials' [`Environment`]. Mirrors the shape `vike-aster`'s `urls::urls_for` already uses, so
/// binance can hit mainnet under `Live` creds instead of hardcoding demo. **Default-safe:** ONLY
/// `Environment::Live` selects mainnet; `Demo`/`Sim` (and — via the absent-creds-is-the-live-gate
/// rule — any uncredentialed run) resolve to the exact demo hosts binance used before this existed,
/// so the default path is byte-identical.
struct BinanceHosts {
    /// Spot signed-REST origin (`/api/v3`).
    spot_rest: &'static str,
    /// USDⓈ-M futures signed-REST origin (`/fapi/v1`).
    fapi_rest: &'static str,
    /// Spot user-data WS-API host.
    spot_ws: &'static str,
    /// USDⓈ-M futures listenKey user-data WS host.
    fapi_ws: &'static str,
}

fn hosts_for(env: Environment) -> BinanceHosts {
    if env == Environment::Live {
        BinanceHosts {
            spot_rest: MAINNET_REST,
            fapi_rest: MAINNET_FAPI_REST,
            spot_ws: MAINNET_WS,
            fapi_ws: MAINNET_FAPI_WS,
        }
    } else {
        BinanceHosts {
            spot_rest: DEMO_REST,
            fapi_rest: DEMO_FAPI_REST,
            spot_ws: DEMO_WS,
            fapi_ws: DEMO_FAPI_WS,
        }
    }
}

/// `BINANCE_MAINNET` env-flag name — the exact-`"1"` opt-in that flips this venue's exec/WS/recon
/// path onto MAINNET (real funds). Parsed by the ONE converged workspace rule (STEP 2, see
/// `vike_bridge_core::mainnet`'s module doc): the EXACT string `"1"`, read from the real process
/// env OR the workspace `.env` map, process env winning. ⚠ STEP 2 CHANGED this venue — a
/// `BINANCE_MAINNET=1` line in the workspace `.env` used to parse as UNSET here (the `.env` is
/// never exported to process env) and silently kept binance on demo; it now ARMS mainnet, exactly
/// as an exported flag does. Default (unset) leaves the caller-supplied `Environment` untouched —
/// and since `make_engine` passes `Demo` unless the flag resolved true, that is byte-identical to
/// before this switch existed. The const AND both reads stay HERE, resolvable by the
/// settings-registry gate; only the PARSE is shared (see `vike_bridge_core::mainnet`'s
/// env-boundary note).
pub const MAINNET_ENV: &str = "BINANCE_MAINNET";

/// Pure `BINANCE_MAINNET` predicate over the two already-read source values (`process` = process
/// env, `map` = the workspace `.env` map): armed ONLY by the EXACT string `"1"` from either, with
/// the process value winning when both are set. Split out from the reads ([`mainnet_enabled`]) so
/// it is unit-testable without mutating global env. Delegates to the ONE shared converged rule
/// (`vike_bridge_core::mainnet::mainnet_for`) keyed on this venue's table row, so neither the value
/// grammar nor the source set can drift from the declared workspace rule.
pub fn mainnet_from(process: Option<&str>, map: Option<&str>) -> bool {
    vike_bridge_core::mainnet::mainnet_for("binance", process, map)
}

/// Reads [`MAINNET_ENV`] from the real process env AND from the caller-supplied workspace `.env`
/// map → `true` only on the exact `"1"` (the shared fold, [`mainnet_from`]); absent from both ⇒
/// `false`. `vars` is the already-loaded `.env` map the mount owns
/// (`load_workspace_dotenv`) — this fn performs the process-env read only, never file I/O, so the
/// settings-registry gate keeps resolving `BINANCE_MAINNET` through the const.
pub fn mainnet_enabled(vars: &std::collections::HashMap<String, String>) -> bool {
    mainnet_from(
        std::env::var(MAINNET_ENV).ok().as_deref(),
        vars.get(MAINNET_ENV).map(String::as_str),
    )
}

/// Resolve the EFFECTIVE environment binance's hosts bind to: an armed `BINANCE_MAINNET` UPGRADES
/// to `Live` (mainnet, real funds), otherwise `env` is returned verbatim. This is the ONE place the
/// flag joins the existing `Environment`-keyed [`hosts_for`] switch, and it is **default-safe**:
/// the flag can only upgrade — never downgrade — so `mainnet = false` leaves `Demo` as `Demo` (the
/// exact hosts binance used before), keeping the default path byte-identical. Idempotent (`Live`
/// stays `Live`), so applying it at more than one entry point is harmless.
///
/// Pure, and `pub` on purpose: `vike_mount::make_engine` resolves the flag ONCE per mount (it owns
/// the `.env` map) and calls THIS to turn it into the `Environment` it then threads into
/// [`fetch_binance_properties`] and [`BinanceExecutionClient::spawn_with_recorder`]. Nothing inside
/// the adapter re-reads the flag any more, so the credential tier, the grid pre-fetch, the exec
/// hosts and the reconcile client can never disagree about which network they are on.
pub fn resolve_env_from(env: Environment, mainnet: bool) -> Environment {
    if mainnet {
        Environment::Live
    } else {
        env
    }
}

/// One fresh SPOT REST client for `symbol` (its own transport + signer, so it can live on its own
/// thread), with the base URL resolved from `env` (demo vs mainnet). `base_asset` only matters for
/// `connect()` reconcile (unused on submit/cancel/history here), so an empty fallback is harmless.
fn mk_spot_rest(
    env: Environment,
    creds: &Credentials,
    symbol: &str,
    properties: SymbolProperties,
    base_asset: &str,
    link_id: Option<String>,
) -> SpotRest {
    BinanceSpotRest {
        signer: BinanceHmacSigner::new(creds, now_ms),
        transport: UreqTransport::new("binance").with_rate_gate(crate::ratelimit::spot_rest_gate()),
        base_url: hosts_for(env).spot_rest.to_string(),
        symbol: symbol.to_string(),
        properties,
        base_asset: base_asset.to_string(),
        link_id,
    }
}

/// One fresh PERP (fapi) REST client for the EXCHANGE `api_symbol` (its own transport + signer), with
/// the base URL resolved from `env`.
///
/// `leverage` is the mount-resolved account leverage ([`leverage_for`]) — the ONLY consumer is
/// `BinancePerpRest::set_leverage`, which `run_perp` fires once at start.
fn mk_perp_rest(
    env: Environment,
    creds: &Credentials,
    api_symbol: &str,
    properties: SymbolProperties,
    link_id: Option<String>,
    leverage: f64,
) -> PerpRest {
    BinancePerpRest {
        signer: BinanceHmacSigner::new(creds, now_ms),
        transport: UreqTransport::new("binance").with_rate_gate(crate::ratelimit::perp_rest_gate()),
        base_url: hosts_for(env).fapi_rest.to_string(),
        symbol: api_symbol.to_string(),
        properties,
        leverage,
        link_id,
    }
}

/// Fetch `symbol`'s REAL grid + base asset, ROUTING by the `.P` suffix: perp reads the fapi
/// `/fapi/v1/exchangeInfo`, spot reads `/api/v3/exchangeInfo`, both under `env`'s hosts. `None` on any
/// network/parse failure or unknown symbol → the caller uses its fallback grid. So the adapter formats
/// orders on the correct grid for ANY symbol on the right venue.
///
/// `env` is the EFFECTIVE environment — the caller has already folded `BINANCE_MAINNET` into it via
/// [`resolve_env_from`] (STEP 2: the flag is resolved ONCE per mount and threaded, never re-read
/// here). So a mainnet mount reads the mainnet grid because its caller said `Live`, not because
/// this function consulted global env behind the caller's back.
pub fn fetch_binance_properties(
    env: Environment,
    creds: &Credentials,
    symbol: &str,
) -> Option<(SymbolProperties, String)> {
    let (api_symbol, is_perp) = split_symbol(symbol);
    let _ = creds; // exchangeInfo is a PUBLIC endpoint (no signing) — creds kept for signature parity
    if is_perp {
        let transport =
            UreqTransport::new("binance").with_rate_gate(crate::ratelimit::perp_rest_gate());
        let info = transport
            .public(
                hosts_for(env).fapi_rest,
                FAPI_PATH_EXCHANGE_INFO,
                &[("symbol", api_symbol.clone())],
            )
            .ok()?;
        let inst = parse_binance_perp_instruments(&info).get(&api_symbol.to_uppercase())?.clone();
        Some((inst.properties, inst.base_asset))
    } else {
        let transport =
            UreqTransport::new("binance").with_rate_gate(crate::ratelimit::spot_rest_gate());
        let info = transport
            .public(hosts_for(env).spot_rest, PATH_EXCHANGE_INFO, &[("symbol", api_symbol.clone())])
            .ok()?;
        let properties = *parse_symbol_properties(&info).get(&api_symbol.to_uppercase())?;
        let base_asset = info
            .get("symbols")
            .and_then(|s| s.as_array())
            .and_then(|arr| arr.first())
            .and_then(|e| e.get("baseAsset"))
            .and_then(|b| b.as_str())
            .unwrap_or("")
            .to_string();
        Some((properties, base_asset))
    }
}

/// Fetch + map recent SPOT order/trade history to events (the audit-A3 replay body). Shared by the
/// pump's reconnect-resync AND the `run_loop` fill-sentinel; both feed the same dedup'd core ingest.
fn spot_history_events(rest: &SpotRest, symbol: &str) -> Vec<Event> {
    let orders =
        rest.get_all_orders(RESYNC_HISTORY_LIMIT).unwrap_or_else(|_| serde_json::json!([]));
    let trades = rest.get_my_trades(RESYNC_HISTORY_LIMIT).unwrap_or_else(|_| serde_json::json!([]));
    map_binance_history(&orders, &trades, "binance", symbol)
}

/// Perp twin of [`spot_history_events`] (fapi `allOrders` + `userTrades` → `map_binance_perp_history`).
fn perp_history_events(rest: &PerpRest, symbol: &str) -> Vec<Event> {
    let orders =
        rest.get_all_orders(RESYNC_HISTORY_LIMIT).unwrap_or_else(|_| serde_json::json!([]));
    let trades =
        rest.get_user_trades(RESYNC_HISTORY_LIMIT).unwrap_or_else(|_| serde_json::json!([]));
    map_binance_perp_history(&orders, &trades, "binance", symbol)
}

/// Live Binance exec client (spot AND perp, chosen by the symbol's `.P` suffix). `submit`/`cancel`/
/// `modify` enqueue onto the REST thread (non-blocking); every venue event returns through the core
/// ingest. Dropping it (or `detach`) stops the REST thread AND the user-data pump — deterministic
/// teardown via the owned [`ExecActor`].
pub struct BinanceExecutionClient(ExecActor);

impl BinanceExecutionClient {
    /// Spawn the exec thread (fetches the instrument grid, starts the user-data pump, then drains
    /// commands). `env` selects the REST/WS hosts (`Live` → mainnet, else demo — default-safe, see
    /// [`hosts_for`]). `symbol` is the CORE label — a trailing `.P` selects the USDⓈ-M perp venue,
    /// otherwise spot (e.g. `"BTCUSDT.P"` vs `"BTCUSDT"`). `fallback_properties` is used ONLY if the
    /// startup exchangeInfo fetch fails — the real grid is fetched at startup so any symbol formats
    /// correctly.
    pub fn spawn(
        env: Environment,
        creds: Credentials,
        symbol: String,
        fallback_properties: SymbolProperties,
        events: EventSender,
    ) -> Self {
        Self::spawn_with_recorder(
            env,
            creds,
            symbol,
            fallback_properties,
            events,
            None,
            None,
            None,
            DEFAULT_LEVERAGE,
        )
    }

    /// Same as [`Self::spawn`], plus an opt-in `properties_rec`: when present, the REAL fetched
    /// instrument grid (never the fallback) is recorded into the PIT properties store right after the
    /// startup exchangeInfo fetch resolves (via [`crate::filters_rec::record_properties_all`], keyed
    /// by the CORE `.P`-preserving label so it matches the series). `properties_rec` is moved into
    /// the exec thread (`PropertiesRecorder` is `Send + Sync`).
    ///
    /// `on_reconcile` (reconciliation-activation Task 7) is threaded straight down to the venue's
    /// `run_resync_supervisor` call (spot or perp, whichever `symbol` routes to) — `Some` pokes the
    /// reconcile driver after every reconnect's event-replay settles; `None` (every non-recon
    /// caller, e.g. [`Self::spawn`]) reproduces the pre-Task-7 event-replay-only behavior
    /// byte-for-byte.
    ///
    /// `link_id` (unified cross-venue attribution, task 6) is the Binance Broker/Link id resolved
    /// ONCE by the caller (`vike-mount`'s `make_engine`, via `attribution_code_from(vars, "binance")`)
    /// and stamped onto every client-order-id string the SUBMIT-capable REST client sends the venue;
    /// `None` (unconfigured, or every non-mount caller e.g. [`Self::spawn`]) reproduces the
    /// pre-task-6 wire body byte-for-byte.
    ///
    /// `leverage` is the account leverage the PERP driver posts once at start
    /// (`POST /fapi/v1/leverage`), resolved ONCE at the mount site from the operator's `[risk]`
    /// budget via [`leverage_for`] — so the venue account and the RiskGate's margin math run on the
    /// SAME number. Pass [`DEFAULT_LEVERAGE`] (what [`Self::spawn`] does) for the historical
    /// literal. Inert on a SPOT symbol: spot has no leverage endpoint and never reads it.
    #[allow(clippy::too_many_arguments)] // task 6's link_id pushed this past the default threshold
    pub fn spawn_with_recorder(
        env: Environment,
        creds: Credentials,
        symbol: String,
        fallback_properties: SymbolProperties,
        events: EventSender,
        properties_rec: Option<Arc<vike_data::PropertiesRecorder>>,
        on_reconcile: Option<mpsc::Sender<()>>,
        link_id: Option<String>,
        leverage: f64,
    ) -> Self {
        let actor = ExecActor::spawn("binance-exec", events.clone(), move |rx| {
            run(
                env,
                creds,
                symbol,
                fallback_properties,
                events,
                rx,
                properties_rec,
                on_reconcile,
                link_id,
                leverage,
            )
        });
        Self(actor)
    }
}

impl ExecutionClient for BinanceExecutionClient {
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

/// The exec thread: resolve the grid (routed by `.P`), record it, then dispatch to the spot or perp
/// driver. ALL network I/O lives on this thread — off the single-writer core.
// Internal exec-thread plumbing: `env` (rung 5) joins the seven fields the driver already threads
// (creds, symbol, grid, events, command rx, PIT recorder, reconcile poke). Bundling them would only
// move the arg list into a struct with no readability gain, so allow the count here.
#[allow(clippy::too_many_arguments)]
fn run(
    env: Environment,
    creds: Credentials,
    symbol: String,
    fallback_properties: SymbolProperties,
    events: EventSender,
    rx: Receiver<ExecCommand>,
    properties_rec: Option<Arc<vike_data::PropertiesRecorder>>,
    on_reconcile: Option<mpsc::Sender<()>>,
    link_id: Option<String>,
    leverage: f64,
) {
    // `env` arrives EFFECTIVE: the mount resolved `BINANCE_MAINNET` once (`resolve_env_from`) and
    // threaded the result in, so every downstream host (`hosts_for`, the REST clients, the
    // user-data WS, the in-thread grid fetch) binds to whatever network the caller selected. This
    // thread deliberately does NOT re-read the flag (STEP 2) — a spawned thread re-reading global
    // env is exactly how a mount ends up signing mainnet creds against demo hosts.
    let (api_symbol, is_perp) = split_symbol(&symbol);
    // Fetch the venue's REAL instrument grid for `symbol` (off the core thread); fall back to the
    // caller's default only if the fetch fails, so orders format on the right tick/step. Only the
    // REAL fetched grid is ever recorded into the PIT properties store (never the fallback).
    let (properties, base_asset) = match fetch_binance_properties(env, &creds, &symbol) {
        Some((real, base)) => {
            crate::filters_rec::record_properties_all(
                &properties_rec,
                [(symbol.to_uppercase(), real)],
                now_ns(),
            );
            (real, base)
        }
        None => {
            tracing::warn!(target: "vike_binance::exec", %symbol, is_perp, "exchangeInfo fetch failed; using fallback properties");
            (fallback_properties, String::new())
        }
    };
    if is_perp {
        run_perp(
            env,
            &creds,
            &symbol,
            &api_symbol,
            properties,
            events,
            rx,
            on_reconcile,
            link_id,
            leverage,
        );
    } else {
        // SPOT has no set-leverage endpoint — `leverage` is deliberately not threaded here.
        run_spot(env, &creds, &symbol, properties, &base_asset, events, rx, on_reconcile, link_id);
    }
}

/// SPOT driver: three REST clients (submit / resync-supervisor / gap-sentinel), the shared server-
/// time skew correction, the WS-API fills pump (+ audit-A3 resync), then the command loop.
#[allow(clippy::too_many_arguments)]
fn run_spot(
    env: Environment,
    creds: &Credentials,
    symbol: &str,
    properties: SymbolProperties,
    base_asset: &str,
    events: EventSender,
    rx: Receiver<ExecCommand>,
    on_reconcile: Option<mpsc::Sender<()>>,
    link_id: Option<String>,
) {
    // Only the submit-capable client carries the resolved link id; the resync/sentinel clients are
    // history-only reads (`get_all_orders`/`get_my_trades`) that never identify an order by coid.
    let rest = mk_spot_rest(env, creds, symbol, properties, base_asset, link_id);
    let resync_rest = mk_spot_rest(env, creds, symbol, properties, base_asset, None);
    let sentinel_rest = mk_spot_rest(env, creds, symbol, properties, base_asset, None);
    // Correct each signer's clock skew once (spot signed requests validate the timestamp).
    match rest.server_time_offset(now_ms()) {
        Ok(offset) => {
            rest.signer.set_offset_ms(offset);
            resync_rest.signer.set_offset_ms(offset);
            sentinel_rest.signer.set_offset_ms(offset);
        }
        Err(e) => {
            tracing::warn!(target: "vike_binance::exec", %symbol, code = e.code, msg = %e.msg, "server time fetch failed; signing with 0 offset");
        }
    }

    let resync_symbol = symbol.to_string();
    let sentinel_symbol = symbol.to_string();
    // Private WS-API user-data (+ audit-A3 resync): authoritative fills/cancels straight into ingest.
    let feed = spawn_binance_user_data_with_resync(
        hosts_for(env).spot_ws.to_string(),
        creds.api_key.clone(),
        creds.api_secret.clone(),
        symbol.to_string(),
        events.clone(),
        move || spot_history_events(&resync_rest, &resync_symbol),
        on_reconcile,
    );
    run_loop(
        &rest,
        &events,
        rx,
        || spot_history_events(&sentinel_rest, &sentinel_symbol),
        FILL_SENTINEL,
    );
    let _ = feed.shutdown();
}

/// PERP driver: three fapi REST clients, a one-shot set-leverage, the listenKey fills pump (+ audit-
/// A3 resync), then the command loop. The REST clients use the EXCHANGE `api_symbol`; the fills pump
/// + history use the CORE `series_symbol` (the `.P` label) so fills fold into the right position.
#[allow(clippy::too_many_arguments)]
fn run_perp(
    env: Environment,
    creds: &Credentials,
    series_symbol: &str,
    api_symbol: &str,
    properties: SymbolProperties,
    events: EventSender,
    rx: Receiver<ExecCommand>,
    on_reconcile: Option<mpsc::Sender<()>>,
    link_id: Option<String>,
    leverage: f64,
) {
    // Only the submit-capable client carries the resolved link id — see `run_spot`'s identical note.
    // All three carry the SAME mount-resolved `leverage`; only `rest` ever posts it (below).
    let rest = mk_perp_rest(env, creds, api_symbol, properties, link_id, leverage);
    let resync_rest = mk_perp_rest(env, creds, api_symbol, properties, None, leverage);
    let sentinel_rest = mk_perp_rest(env, creds, api_symbol, properties, None, leverage);
    // fapi change-leverage is idempotent (HTTP-200 even at target); best-effort — a failure just
    // leaves the account's current leverage in place and orders still submit. UNCHANGED handling:
    // the value is now the operator's, but a rejection still only warns and never kills the mount.
    if let Err(e) = rest.set_leverage() {
        tracing::warn!(target: "vike_binance::exec", symbol = %series_symbol, leverage, code = e.code, msg = %e.msg, "set_leverage failed (continuing) — the account keeps its CURRENT leverage while the RiskGate sizes against the configured one");
    }

    let resync_symbol = series_symbol.to_string();
    let sentinel_symbol = series_symbol.to_string();
    // Private listenKey user-data (+ audit-A3 resync): fills carry the CORE `.P` label.
    let hosts = hosts_for(env);
    let feed = spawn_binance_perp_user_data_with_resync(
        hosts.fapi_rest.to_string(),
        hosts.fapi_ws.to_string(),
        creds.api_key.clone(),
        series_symbol.to_string(),
        events.clone(),
        move || perp_history_events(&resync_rest, &resync_symbol),
        on_reconcile,
    );
    run_loop(
        &rest,
        &events,
        rx,
        || perp_history_events(&sentinel_rest, &sentinel_symbol),
        FILL_SENTINEL,
    );
    let _ = feed.shutdown();
}

#[cfg(test)]
mod tests {
    //! Venue-specific wiring tests: `.P` host routing and rung-5 host resolution, asserted with NO
    //! network. The generic `run_loop` command→event tests (mock `VenueRest`) moved WITH `run_loop`
    //! to `vike_bridge_core::exec_actor::run_loop_tests`; the binance reality-tie
    //! (`caps_for("binance").supports_modify`) lives in the lib `caps_test`.
    use super::*;

    /// `.P` routing (NO network): the trailing `.P` selects the fapi hosts + strips to the exchange
    /// symbol; a bare symbol stays spot. This is the exact decision `run` makes before it dispatches
    /// to `run_perp` / `run_spot`, so a perp symbol provably reaches the fapi REST + WS endpoints.
    #[test]
    fn dot_p_routes_to_fapi_hosts_and_strips_symbol() {
        // perp: `.P` stripped, fapi hosts chosen
        assert_eq!(split_symbol("BTCUSDT.P"), ("BTCUSDT".to_string(), true));
        // spot: unchanged, non-perp
        assert_eq!(split_symbol("BTCUSDT"), ("BTCUSDT".to_string(), false));
        // the hosts the drivers pin (the endpoints `run_perp`/`run_spot` actually use)
        assert_eq!(DEMO_FAPI_REST, "https://demo-fapi.binance.com");
        assert_eq!(DEMO_FAPI_WS, "wss://fstream.binancefuture.com/ws");
        assert_eq!(DEMO_REST, "https://demo-api.binance.com");
        assert_eq!(DEMO_WS, "wss://demo-ws-api.binance.com/ws-api/v3");
    }

    /// Rung-5 host resolution: ONLY `Live` selects mainnet; `Demo`/`Sim` (and, via the
    /// absent-creds-is-the-live-gate rule, any uncredentialed run) stay on the exact demo hosts
    /// binance used before env-threading — so the default order path is byte-identical.
    #[test]
    fn hosts_for_live_is_mainnet_others_stay_demo() {
        let live = hosts_for(Environment::Live);
        assert_eq!(live.spot_rest, MAINNET_REST);
        assert_eq!(live.fapi_rest, MAINNET_FAPI_REST);
        assert_eq!(live.spot_ws, MAINNET_WS);
        assert_eq!(live.fapi_ws, MAINNET_FAPI_WS);

        for env in [Environment::Demo, Environment::Sim] {
            let demo = hosts_for(env);
            assert_eq!(demo.spot_rest, DEMO_REST, "{env:?} must stay on demo hosts");
            assert_eq!(demo.fapi_rest, DEMO_FAPI_REST, "{env:?} must stay on demo hosts");
            assert_eq!(demo.spot_ws, DEMO_WS, "{env:?} must stay on demo hosts");
            assert_eq!(demo.fapi_ws, DEMO_FAPI_WS, "{env:?} must stay on demo hosts");
        }
    }

    /// The leverage this arm POSTs to `/fapi/v1/leverage` is the OPERATOR's `[risk] max_leverage`,
    /// and an unset profile still yields the historical literal (`2.0`) — the property that makes
    /// this change safe to ship onto a live account. NO network: `leverage_for` is the pure
    /// decision `make_engine` calls once, before anything is spawned.
    #[test]
    fn perp_leverage_follows_the_operator_risk_budget() {
        // Unset — both shapes of "the operator said nothing" — keeps today's literal EXACTLY.
        assert_eq!(leverage_for(None), DEFAULT_LEVERAGE);
        assert_eq!(leverage_for(None), 2.0, "the historical hardcoded /fapi/v1/leverage value");
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

    /// `BINANCE_MAINNET` gate: default-safe — unset / non-`"1"` values leave `env` untouched; ONLY
    /// the exact `"1"` upgrades to mainnet. Asserted through the pure `mainnet_from` +
    /// `resolve_env_from` cores (no global-env mutation).
    #[test]
    fn binance_mainnet_flag_upgrades_only_on_exact_one() {
        assert!(!mainnet_from(None, None), "unset ⇒ no upgrade");
        assert!(!mainnet_from(Some("0"), None), "0 ⇒ no upgrade");
        assert!(!mainnet_from(Some("true"), None), "fuzzy truthy ⇒ no upgrade (exact-1 only)");
        assert!(mainnet_from(Some("1"), None), "exact 1 ⇒ upgrade");
        // STEP 2: the workspace `.env` map is a REAL source now — a `.env`-only line arms mainnet
        // where it used to be a silent no-op (the audit finding).
        assert!(mainnet_from(None, Some("1")), ".env-only 1 ⇒ upgrade (STEP-2 gain)");
        assert!(!mainnet_from(None, Some("true")), ".env-only fuzzy truthy ⇒ no upgrade");
        // …and the process value still wins outright when both are set.
        assert!(!mainnet_from(Some("0"), Some("1")), "disarming process masks an arming .env");
        assert!(mainnet_from(Some("1"), Some("0")), "arming process beats a disarming .env");

        // Flag UNSET: env is returned verbatim, so `Demo` (what make_engine passes) stays demo →
        // `hosts_for` yields the exact demo hosts (byte-identical default path).
        assert_eq!(resolve_env_from(Environment::Demo, false), Environment::Demo);
        assert_eq!(resolve_env_from(Environment::Sim, false), Environment::Sim);
        let demo = hosts_for(resolve_env_from(Environment::Demo, false));
        assert_eq!(demo.spot_rest, DEMO_REST);
        assert_eq!(demo.fapi_ws, DEMO_FAPI_WS);

        // Flag SET: any caller env upgrades to Live → mainnet hosts.
        assert_eq!(resolve_env_from(Environment::Demo, true), Environment::Live);
        assert_eq!(resolve_env_from(Environment::Live, true), Environment::Live); // idempotent
        let main = hosts_for(resolve_env_from(Environment::Demo, true));
        assert_eq!(main.spot_rest, MAINNET_REST);
        assert_eq!(main.fapi_rest, MAINNET_FAPI_REST);
        assert_eq!(main.spot_ws, MAINNET_WS);
        assert_eq!(main.fapi_ws, MAINNET_FAPI_WS);
    }

    /// This venue HAS a row in the shared per-venue switch table, so [`mainnet_from`]'s fold is
    /// actually gated by it (a switchless venue can never be armed). Removing binance's row would
    /// silently pin this venue to demo forever — this pin makes that a deliberate edit.
    #[test]
    fn binance_has_a_row_in_the_shared_switch_table() {
        assert_eq!(
            vike_bridge_core::mainnet::mainnet_switch_for("binance"),
            Some(vike_bridge_core::mainnet::MainnetSwitch)
        );
    }

    /// The `.env`-map read is threaded from the CALLER's map, not from process env — proven with a
    /// map this test owns (no global-env mutation, so it is safe under a parallel test runner).
    #[test]
    fn mainnet_enabled_consults_the_caller_supplied_dotenv_map() {
        let mut vars = std::collections::HashMap::new();
        vars.insert(MAINNET_ENV.to_string(), "1".to_string());
        // Only assert the direction that cannot be spoofed by a stray exported flag: an arming map
        // entry must arm (an exported `BINANCE_MAINNET=1` would arm it anyway, so the negative
        // direction is left to the env-free `mainnet_from` cases above).
        assert!(mainnet_enabled(&vars), ".env-map `1` must arm mainnet");
    }
}
