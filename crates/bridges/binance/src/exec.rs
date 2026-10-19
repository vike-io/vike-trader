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
//! LIVE GATE: absent credentials never reach here — the start only spawns this when
//! `vike_secrets::credentials::api_credentials` yields `Some` for the started account (the
//! codebase's absent-creds-is-the-live-gate rule). An account with no keys does not run.
//!
//! v1 scope: `submit`/`cancel`/`modify`. PERP `modify` is the NATIVE fapi amend (`PUT /fapi/v1/order`,
//! wired via `run_loop`'s `ExecCommand::Modify` → `BinancePerpRest::modify_order`); SPOT `modify` is
//! an atomic `POST /api/v3/order/cancelReplace` under the SAME client order id
//! (`BinanceSpotRest::modify_order`: untouched LIMIT orders only; every other case answers
//! `OrderModifyRejected`, never silence). Native fapi BATCH (submit/cancel) exists
//! on `BinancePerpRest` but is NOT reachable through the shared `ExecActor` command channel yet — it
//! carries no batch variant, so batches fan out to per-order submits via the `ExecutionClient`
//! default (same limitation as bybit; an `ExecCommand::Batch` in vike-bridge-core is the follow-up).

use std::sync::Arc;
use std::sync::mpsc::{self, Receiver};

use vike_bridge_core::confirm::{DispatchLedger, Dispatched, confirm_closure};
use vike_bridge_core::exec_actor::{
    ConfirmFn, DEFAULT_FILL_SENTINEL, ExecActor, ExecCommand, run_loop,
};
use vike_bridge_core::ratelimit::RateGate;
use vike_bridge_core::signer::{BinanceHmacSigner, Signer};
use vike_bridge_core::transport::{RestTransport, UreqTransport};
use vike_exec::{EventSender, ExecutionClient};
use vike_model::AccountTier;
use vike_model::events::Event;
use vike_model::{OrderRequest, SymbolProperties, now_ms, now_ns};
use vike_secrets::credentials::Credentials;

use crate::family::exec_loop::split_symbol;
use crate::history::{map_binance_history, map_binance_perp_history};
use crate::per_symbol::{ClientFactory, CoidSymbols, PerSymbolRest};
use crate::perp::{
    BinancePerpRest, DEMO_FAPI_REST, DEMO_FAPI_WS, MAINNET_FAPI_REST, MAINNET_FAPI_WS,
    PATH_EXCHANGE_INFO as FAPI_PATH_EXCHANGE_INFO, parse_binance_perp_instruments,
};
use crate::perp_user_data::spawn_binance_perp_user_data_with_resync;
use crate::spot::{
    BinanceSpotRest, DEMO_REST, MAINNET_REST, PATH_EXCHANGE_INFO, parse_symbol_properties,
};
use crate::user_data::{DEMO_WS, MAINNET_WS, spawn_binance_user_data_with_resync};

/// Rows of recent order/trade history the audit-A3 resync replays after each WS reconnect.
const RESYNC_HISTORY_LIMIT: u32 = 50;
/// FALLBACK cross-margin leverage the perp exec thread sets once at start — the literal this arm
/// posted to `/fapi/v1/leverage` unconditionally before the operator's budget was threaded in
/// (demo default; matches the smoke). Used ONLY when the operator expressed no leverage intent, so
/// a start with no `[risk] max_leverage` is byte-identical to every start before [`leverage_for`]
/// existed. NEVER read on the submit path — `BinancePerpRest::leverage` is consulted by
/// `set_leverage` and nothing else.
const DEFAULT_LEVERAGE: f64 = 2.0;

/// The account leverage this arm POSTs at startup, resolved from the operator's `[risk]` budget.
///
/// Pure, and `pub` on purpose — the SAME shape as [`resolve_env_from`]: the start resolves the
/// network from the ceiling (decision 0095) and passes `mainnet`; `crate::adapter`'s
/// `BinanceVenueAdapter::start` is handed the `ProfileRisk` and calls this ONCE per start, then
/// threads the resolved `f64` into [`BinanceExecutionClient::spawn_with_recorder`]. Nothing inside
/// the adapter reaches for the profile, so the number the venue account is configured with and the
/// number the RiskGate sizes against (`ProfileRisk::im_requirement`, `im = 1.0 / max_leverage`)
/// can never disagree — that disagreement was the bug this exists to close.
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
/// unauthenticated — i.e. a WHOLE NEW SIGNED request on the start path, not a field ride-along.
/// Those brackets are also RISK-TIERED (`initialLeverage` falls as position notional rises), so
/// "the cap" here is a function of size, not the single flat number bybit/okx publish; clamping to
/// the tier-0 value would be a weaker and different claim. Adding that fetch is a deliberate
/// follow-up with its own cost discussion, not a doc-closing change — so a too-high request is
/// still rejected by the VENUE, which `run_perp`'s `set_leverage` warn already surfaces.
pub fn leverage_for(profile: Option<&vike_model::ProfileRisk>) -> f64 {
    vike_bridge_core::leverage::resolve_startup_leverage(profile, DEFAULT_LEVERAGE)
}

/// The concrete REST clients this exec half drives (blocking `ureq` + HMAC signer).
type SpotRest = BinanceSpotRest<BinanceHmacSigner, UreqTransport>;
type PerpRest = BinancePerpRest<BinanceHmacSigner, UreqTransport>;

/// The four exec-side hosts (spot/perp REST + spot WS-API / perp listenKey WS), resolved from the
/// account's tier ([`AccountTier`]). Mirrors the shape `vike-aster`'s `urls::urls_for` already uses,
/// so binance can hit mainnet under a `live` account instead of hardcoding demo. **Default-safe:**
/// ONLY [`AccountTier::Live`] selects mainnet; `Demo`/`Paper` (and — via the
/// absent-creds-is-the-live-gate rule — any uncredentialed run) resolve to the exact demo hosts
/// binance used before this existed, so the default path is byte-identical.
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

fn hosts_for(env: AccountTier) -> BinanceHosts {
    match env {
        AccountTier::Live => BinanceHosts {
            spot_rest: MAINNET_REST,
            fapi_rest: MAINNET_FAPI_REST,
            spot_ws: MAINNET_WS,
            fapi_ws: MAINNET_FAPI_WS,
        },
        AccountTier::Demo | AccountTier::Paper => BinanceHosts {
            spot_rest: DEMO_REST,
            fapi_rest: DEMO_FAPI_REST,
            spot_ws: DEMO_WS,
            fapi_ws: DEMO_FAPI_WS,
        },
    }
}

/// Resolve the EFFECTIVE tier binance's hosts bind to, given whether the start resolved the LIVE
/// (mainnet) tier: `mainnet = true` UPGRADES to [`AccountTier::Live`] (real funds), otherwise `env`
/// is returned verbatim. This is the ONE place that decision joins the existing tier-keyed
/// [`hosts_for`] switch, and it is **default-safe**: `mainnet` can only upgrade — never downgrade —
/// so `mainnet = false` leaves `Demo` as `Demo`. Idempotent (`Live` stays `Live`), so applying it at
/// more than one entry point is harmless.
///
/// ⚠ **The account's tier IS the network for this venue** (decision 0095, as amended) —
/// `crate::adapter`'s `BinanceVenueAdapter::start` takes `mainnet` from the tier
/// (`AdapterInputs::tier`, which `vike-tradehub-core` folds from the account row's `tier` — never a
/// `BINANCE_MAINNET` flag any more)
/// and calls THIS to turn it into the [`AccountTier`] it then threads into
/// [`fetch_binance_properties`] and [`BinanceExecutionClient::spawn_with_recorder`]. Nothing
/// inside the adapter reads any flag, so the account's credentials, the grid pre-fetch, the exec
/// hosts and the reconcile client can never disagree about which network they are on.
pub fn resolve_env_from(env: AccountTier, mainnet: bool) -> AccountTier {
    if mainnet { AccountTier::Live } else { env }
}

/// One fresh SPOT REST client for `symbol` (its own transport + signer, so it can live on its own
/// thread), with the base URL resolved from `env` (demo vs mainnet). `base_asset` only matters for
/// `connect()` reconcile (unused on submit/cancel/history here), so an empty fallback is harmless.
fn mk_spot_rest(
    env: AccountTier,
    creds: &Credentials,
    symbol: &str,
    properties: SymbolProperties,
    base_asset: &str,
    link_id: Option<String>,
) -> SpotRest {
    mk_spot_rest_gated(
        env,
        creds,
        symbol,
        properties,
        base_asset,
        link_id,
        crate::ratelimit::spot_rest_gate(),
    )
}

/// [`mk_spot_rest`] on a GIVEN order-rate window: the mounted spot client and every foreign-symbol
/// spot client of one exec thread are built on clones of ONE gate, because the venue's order budget
/// is per account, not per symbol.
fn mk_spot_rest_gated(
    env: AccountTier,
    creds: &Credentials,
    symbol: &str,
    properties: SymbolProperties,
    base_asset: &str,
    link_id: Option<String>,
    gate: RateGate,
) -> SpotRest {
    BinanceSpotRest {
        signer: BinanceHmacSigner::new(creds, now_ms),
        transport: UreqTransport::new("binance").with_rate_gate(gate),
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
    env: AccountTier,
    creds: &Credentials,
    api_symbol: &str,
    properties: SymbolProperties,
    link_id: Option<String>,
    leverage: f64,
) -> PerpRest {
    mk_perp_rest_gated(
        env,
        creds,
        api_symbol,
        properties,
        link_id,
        leverage,
        crate::ratelimit::perp_rest_gate(),
    )
}

/// [`mk_perp_rest`] on a GIVEN order-rate window (see [`mk_spot_rest_gated`]).
fn mk_perp_rest_gated(
    env: AccountTier,
    creds: &Credentials,
    api_symbol: &str,
    properties: SymbolProperties,
    link_id: Option<String>,
    leverage: f64,
    gate: RateGate,
) -> PerpRest {
    BinancePerpRest {
        signer: BinanceHmacSigner::new(creds, now_ms),
        transport: UreqTransport::new("binance").with_rate_gate(gate),
        base_url: hosts_for(env).fapi_rest.to_string(),
        symbol: api_symbol.to_string(),
        properties,
        leverage,
        link_id,
    }
}

/// The venue's active order-status re-query for [`ExecActor::with_confirm`]: the core's
/// `Command::ConfirmOrder` lands here on a short-lived worker thread (blocking REST, never on the
/// core fold), and runs the SAME by-coid lookup the ambiguous-submit arm uses
/// (`vike_bridge_core::confirm`, with [`crate::family::confirm`]'s status words): found -> `OrderAccepted` (+ the terminal when the venue already
/// closed it), -2013 -> the "venue confirms absent" reject ONLY once the exec thread has finished
/// this coid's submit (`ledger`), a FAILED query -> nothing. ONE REST client serves every confirm
/// (one TLS pool), bounded and backed off by [`confirm_closure`]. The submit client's `link_id` is
/// carried because the venue stored the PREFIXED coid; the grid is the fallback one (a status GET
/// never reads it). Spot reads the server-time skew once, lazily, on the first confirm (spot signed
/// requests validate the timestamp; perp is not given one, as `run_perp` does not).
///
/// The one client is a [`PerSymbolRest`]: a coid the exec thread recorded as a FOREIGN order
/// (`coids`) is asked about on its own symbol, any other on the mounted one. The foreign-symbol clients
/// here read no grid (a status GET never formats a price), so building one costs no network.
fn confirm_fn(
    env: AccountTier,
    creds: &Credentials,
    symbol: &str,
    fallback_properties: SymbolProperties,
    link_id: Option<String>,
    ledger: Arc<DispatchLedger>,
    coids: Arc<CoidSymbols>,
) -> ConfirmFn {
    let (api_symbol, is_perp) = split_symbol(symbol);
    if is_perp {
        let rest = mk_perp_rest(
            env,
            creds,
            &api_symbol,
            fallback_properties,
            link_id.clone(),
            DEFAULT_LEVERAGE,
        );
        let creds = creds.clone();
        let make: ClientFactory<PerpRest> = Box::new(move |label| {
            let api_symbol = lane_symbol(label, true)?;
            Ok(mk_perp_rest(
                env,
                &creds,
                &api_symbol,
                fallback_properties,
                link_id.clone(),
                DEFAULT_LEVERAGE,
            ))
        });
        confirm_closure(PerSymbolRest::new(symbol, rest, make, coids), ledger, |_| true)
    } else {
        let rest = mk_spot_rest(env, creds, symbol, fallback_properties, "", link_id.clone());
        let creds = creds.clone();
        let make: ClientFactory<SpotRest> = Box::new(move |label| {
            let api_symbol = lane_symbol(label, false)?;
            Ok(mk_spot_rest(env, &creds, &api_symbol, fallback_properties, "", link_id.clone()))
        });
        confirm_closure(PerSymbolRest::new(symbol, rest, make, coids), ledger, |routed| {
            routed.with_mounted(|rest| match rest.server_time_offset(now_ms()) {
                Ok(offset) => {
                    rest.signer.set_offset_ms(offset);
                    true
                }
                // best-effort, retried on the next confirm: this one signs with 0 offset
                Err(_) => false,
            })
        })
    }
}

/// **The one write of the account's leverage for a perpetual the engine was NOT mounted on** (the
/// gatewayhub lane's per-symbol hand-over, `VenueAdapter::set_symbol_leverage`):
/// [`BinancePerpRest::set_leverage`] itself (`POST /fapi/v1/leverage`, the figure cast `as i64`,
/// idempotent on the venue), the verb `run_perp` posts at its start, on a client of its own because
/// `foreign_perp_factory` posts none on purpose. `symbol` is the engine's spelling (`ETHUSDT.P`); a
/// SPOT symbol has no leverage and is refused. `leverage` is a whole number and is written as given:
/// the lane's probe refused a figure above the symbol's maximum. `Err` is the venue's own words.
pub fn write_symbol_leverage(
    creds: &Credentials,
    env: AccountTier,
    symbol: &str,
    leverage: f64,
    link_id: Option<String>,
) -> Result<(), String> {
    let (api_symbol, is_perp) = split_symbol(&symbol.to_uppercase());
    if !is_perp {
        return Err(format!(
            "{symbol} is a binance SPOT symbol: leverage is a USDⓈ-M futures setting (a perpetual \
             is spelled with the .P suffix)"
        ));
    }
    let rest =
        mk_perp_rest(env, creds, &api_symbol, SymbolProperties::default(), link_id, leverage);
    post_symbol_leverage(&rest)?;
    tracing::info!(target: "vike_binance::exec", %symbol, leverage, "set-leverage POSTED for a perpetual admitted on demand (the gatewayhub reads it back)");
    Ok(())
}

/// [`write_symbol_leverage`]'s post and its error words, over any transport.
fn post_symbol_leverage<S: Signer, T: RestTransport>(
    rest: &BinancePerpRest<S, T>,
) -> Result<(), String> {
    rest.set_leverage().map_err(|e| format!("set-leverage for {} was refused: {e}", rest.symbol))
}

/// The exchange symbol of an order on `label`, or why this account's lane cannot take it. An engine
/// runs ONE lane (spot or USDⓈ-M perp, by the mounted symbol's `.P`), with ONE user-data stream: an
/// order on the OTHER lane would be signed but its fills would never arrive, so it is refused here
/// rather than traded blind. (Two lanes in one engine is phase 1c of the orders-follow-the-symbol
/// spec.)
fn lane_symbol(label: &str, want_perp: bool) -> Result<String, String> {
    let (api_symbol, is_perp) = split_symbol(label);
    if is_perp == want_perp {
        return Ok(api_symbol.to_uppercase());
    }
    let (this, other) = if want_perp { ("perp", "spot") } else { ("spot", "perp") };
    Err(format!(
        "binance: cannot sign {label:?}: this account's engine runs the {this} lane and {label:?} \
         is a {other} symbol; the {other} lane has no user-data stream here, so its fills would \
         never arrive. The order was not sent"
    ))
}

/// Why an order on `label` is refused when its grid cannot be read: never a guessed or BTC-shaped
/// fallback grid.
fn no_grid_reason(label: &str) -> String {
    format!(
        "binance: cannot sign {label:?}: exchangeInfo has no usable grid for it (unknown symbol, a \
         COIN-M contract, or the venue was unreachable); the order was not sent"
    )
}

/// The [`PerSymbolRest`] factory for a PERP engine: a signing client for another perp of the SAME
/// account (same keys, host, broker link id, leverage value and order-rate window as the mounted
/// client) on that symbol's own fetched grid. The grid fetch is one blocking public `exchangeInfo`
/// GET on the exec thread, bounded by the transport's timeout. `leverage` is carried but NOT posted
/// for the new symbol: the venue keeps its current leverage there (phase 1f is not done here).
fn foreign_perp_factory(
    env: AccountTier,
    creds: Credentials,
    link_id: Option<String>,
    leverage: f64,
    gate: RateGate,
) -> ClientFactory<PerpRest> {
    Box::new(move |label| {
        let label = label.to_uppercase();
        let api_symbol = lane_symbol(&label, true)?;
        let (properties, _) =
            fetch_binance_properties(env, &creds, &label).ok_or_else(|| no_grid_reason(&label))?;
        // ⚠ NO `set_leverage` here, deliberately: this factory also runs after every restart, when the
        // account may already hold a position on this symbol at a leverage set on the venue; posting
        // the MOUNTED symbol's configured value would move that position's liquidation price. A new
        // symbol keeps the leverage the venue holds for it (the RiskGate still sizes against the
        // configured one: `docs/ops/known-limitations-2026-10.md`).
        Ok(mk_perp_rest_gated(
            env,
            &creds,
            &api_symbol,
            properties,
            link_id.clone(),
            leverage,
            gate.clone(),
        ))
    })
}

/// [`foreign_perp_factory`]'s spot twin. Spot signed requests validate the timestamp, so the new
/// client reads the server-time skew once, like the mounted ones; a failed read signs with 0 offset
/// (warned), exactly as the mounted clients do.
fn foreign_spot_factory(
    env: AccountTier,
    creds: Credentials,
    link_id: Option<String>,
    gate: RateGate,
) -> ClientFactory<SpotRest> {
    Box::new(move |label| {
        let label = label.to_uppercase();
        let api_symbol = lane_symbol(&label, false)?;
        let (properties, base_asset) =
            fetch_binance_properties(env, &creds, &label).ok_or_else(|| no_grid_reason(&label))?;
        let rest = mk_spot_rest_gated(
            env,
            &creds,
            &api_symbol,
            properties,
            &base_asset,
            link_id.clone(),
            gate.clone(),
        );
        match rest.server_time_offset(now_ms()) {
            Ok(offset) => rest.signer.set_offset_ms(offset),
            Err(e) => {
                tracing::warn!(target: "vike_binance::exec", symbol = %label, code = e.code, msg = %e.msg, "server time fetch failed; signing with 0 offset")
            }
        }
        Ok(rest)
    })
}

/// Fetch `symbol`'s REAL grid + base asset, ROUTING by the `.P` suffix: perp reads the fapi
/// `/fapi/v1/exchangeInfo`, spot reads `/api/v3/exchangeInfo`, both under `env`'s hosts. `None` on any
/// network/parse failure or unknown symbol → the caller uses its fallback grid. So the adapter formats
/// orders on the correct grid for ANY symbol on the right venue.
///
/// `env` is the EFFECTIVE tier — the caller has already folded decision 0095's
/// ceiling-derived mainnet choice into it via [`resolve_env_from`] (STEP 2:
/// `BinanceVenueAdapter::start` takes it ONCE per start from `AdapterInputs::tier` and
/// threads it, never re-read here). So a mainnet start reads the mainnet grid because its caller said `Live`,
/// not because this function consulted global env behind the caller's back.
pub fn fetch_binance_properties(
    env: AccountTier,
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
pub struct BinanceExecutionClient(ExecActor, Arc<CoidSymbols>);

impl BinanceExecutionClient {
    /// Hand this client the HALT sentinel it watches: the venue's start passes
    /// `AdapterInputs::process.halt_path`, the one file `vike-tradehub-core` resolved for the whole process
    /// (`vike_bridge_core::exec_actor::ExecActor::with_halt_path` carries the contract). A client
    /// built without it watches NOTHING — `crates/vike-ops/tests/settings_secrets/bridge_inputs_gate.rs` holds that
    /// every live start calls this.
    #[must_use]
    pub fn with_halt_path(self, path: std::path::PathBuf) -> Self {
        Self(self.0.with_halt_path(path), self.1)
    }

    /// Spawn the exec thread (fetches the instrument grid, starts the user-data pump, then drains
    /// commands). `env` selects the REST/WS hosts (`Live` → mainnet, else demo — default-safe, see
    /// [`hosts_for`]). `symbol` is the CORE label — a trailing `.P` selects the USDⓈ-M perp venue,
    /// otherwise spot (e.g. `"BTCUSDT.P"` vs `"BTCUSDT"`). `fallback_properties` is used ONLY if the
    /// startup exchangeInfo fetch fails — the real grid is fetched at startup so any symbol formats
    /// correctly.
    pub fn spawn(
        env: AccountTier,
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
            false,
        )
    }

    /// Same as [`Self::spawn`], plus an opt-in `properties_rec`: when present, the REAL fetched
    /// instrument grid (never the fallback) is recorded into the PIT properties store right after the
    /// startup exchangeInfo fetch resolves (via [`crate::filters_rec::record_properties_all`], keyed
    /// by the CORE `.P`-preserving label so it matches the series). `properties_rec` is moved into
    /// the exec thread (`PropertiesRecorder` is `Send + Sync`).
    ///
    /// `on_reconcile` (reconciliation-activation Task 7) is threaded straight down to the venue's
    /// `run_resync_supervisor` call (spot or perp, whichever `symbol` routes to) — `Some` fires the
    /// recon trigger after every reconnect's event-replay settles; `None` (every non-recon
    /// caller, e.g. [`Self::spawn`]) reproduces the pre-Task-7 event-replay-only behavior
    /// byte-for-byte.
    ///
    /// `link_id` (unified cross-venue attribution, task 6) is the Binance Broker/Link id resolved
    /// ONCE by the caller (`crate::adapter`'s `BinanceVenueAdapter::start`, via `attribution_code_from`
    /// over `AdapterInputs::secrets`) and stamped onto every client-order-id string the
    /// SUBMIT-capable REST client sends the venue; `None` (unconfigured, or every non-mount caller
    /// e.g. [`Self::spawn`]) reproduces the pre-task-6 wire body byte-for-byte.
    ///
    /// `leverage` is the account leverage the PERP driver posts once at start
    /// (`POST /fapi/v1/leverage`), resolved ONCE at the start site from the operator's `[risk]`
    /// budget via [`leverage_for`] — so the venue account and the RiskGate's margin math run on the
    /// SAME number. Pass [`DEFAULT_LEVERAGE`] (what [`Self::spawn`] does) for the historical
    /// literal. Inert on a SPOT symbol: spot has no leverage endpoint and never reads it.
    ///
    /// `trade_lite_fill` is the `venue.binance.trade_lite_fill` setting, read once by the start
    /// through [`crate::perp_user_data::trade_lite_fill`]; inert on a spot symbol.
    /// [`Self::spawn`] passes `false`.
    pub fn spawn_with_recorder(
        env: AccountTier,
        creds: Credentials,
        symbol: String,
        fallback_properties: SymbolProperties,
        events: EventSender,
        properties_rec: Option<Arc<vike_data::PropertiesRecorder>>,
        on_reconcile: Option<mpsc::Sender<()>>,
        link_id: Option<String>,
        leverage: f64,
        trade_lite_fill: bool,
    ) -> Self {
        let ledger = Arc::new(DispatchLedger::default());
        // Which symbol each FOREIGN order was submitted on: written by the exec thread's router,
        // read by the confirm worker's, so a status query lands on the order's own symbol.
        let coids = Arc::new(CoidSymbols::default());
        let adopt_coids = coids.clone();
        let confirm = confirm_fn(
            env,
            &creds,
            &symbol,
            fallback_properties,
            link_id.clone(),
            ledger.clone(),
            coids.clone(),
        );
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
                trade_lite_fill,
                ledger,
                coids,
            )
        })
        .with_confirm(confirm);
        Self(actor, adopt_coids)
    }
}

impl ExecutionClient for BinanceExecutionClient {
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

/// The exec thread: resolve the grid (routed by `.P`), record it, then dispatch to the spot or perp
/// driver. ALL network I/O lives on this thread — off the single-writer core.
// Internal exec-thread plumbing: `env` (rung 5) joins the seven fields the driver already threads
// (creds, symbol, grid, events, command rx, PIT recorder, recon trigger). Bundling them would only
// move the arg list into a struct with no readability gain, so allow the count here.
#[expect(clippy::too_many_arguments)]
fn run(
    env: AccountTier,
    creds: Credentials,
    symbol: String,
    fallback_properties: SymbolProperties,
    events: EventSender,
    rx: Receiver<ExecCommand>,
    properties_rec: Option<Arc<vike_data::PropertiesRecorder>>,
    on_reconcile: Option<mpsc::Sender<()>>,
    link_id: Option<String>,
    leverage: f64,
    trade_lite_fill: bool,
    ledger: Arc<DispatchLedger>,
    coids: Arc<CoidSymbols>,
) {
    // `env` arrives EFFECTIVE: the start resolved decision 0095's ceiling-derived mainnet choice
    // once (`resolve_env_from`) and threaded the result in, so every downstream host (`hosts_for`,
    // the REST clients, the user-data WS, the in-thread grid fetch) binds to whatever network the
    // caller selected. This thread deliberately does NOT re-resolve it (STEP 2) — a spawned thread
    // re-reading global state is exactly how a start ends up signing mainnet creds against demo
    // hosts.
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
            // ⚠ THE COIN-M REFUSAL. A missing grid is reached two ways and they are not the same
            // failure. `exchangeInfo fetch failed` is transient; a COIN-M symbol is STRUCTURAL —
            // fapi does not list it and never will, so the fallback grid would be signed over
            // forever rather than for one startup. The listing is consulted HERE, on a path that
            // has already failed, so a working start pays nothing for it and a box that cannot
            // reach the COIN-M host degrades to exactly today's warn rather than to a new refusal.
            match crate::instruments::coin_m_contract(&api_symbol) {
                Ok(Some(contract)) => {
                    let reason = coin_m_exec_refusal(&symbol, contract);
                    tracing::error!(target: "vike_binance::exec", %symbol, "{reason}");
                    run_refusing(&reason, &events, &rx);
                    return;
                }
                Ok(None) => {}
                Err(e) => tracing::warn!(
                    target: "vike_binance::exec",
                    %symbol,
                    error = %e,
                    "could not read the COIN-M listing while resolving a missing grid; \
                     falling through to the fallback-properties path unchanged"
                ),
            }
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
            trade_lite_fill,
            ledger,
            coids,
        );
    } else {
        // SPOT has no set-leverage endpoint and no TRADE_LITE frame — `leverage` and
        // `trade_lite_fill` are deliberately not threaded here.
        run_spot(
            env,
            &creds,
            &symbol,
            properties,
            &base_asset,
            events,
            rx,
            on_reconcile,
            link_id,
            ledger,
            coids,
        );
    }
}

/// **The refusal a COIN-M symbol gets from the EXEC path**, on the standard
/// `crates/bridges/bybit/src/data.rs`'s `ambiguous_bare_symbol_refusal` sets: it names the exact
/// limitation, it carries its own measurement, it leaves an ACT, and it refuses to recommend the
/// accidental spelling that happens to work.
///
/// ⚠ **The DATA path reaches this venue's COIN-M book and this one does not, deliberately** — a
/// data path that can reach a book the exec path cannot is a trap only while it is silent, and this
/// is the noise. What is missing is not a host: it is everything an order is signed and sized
/// against.
fn coin_m_exec_refusal(symbol: &str, contract: crate::instruments::CoinMContract) -> String {
    format!(
        "binance: {symbol:?} is a COIN-M ({}) contract, and this venue's EXEC path cannot honour \
         one — so it is refused rather than signed on a guess. What is missing, MEASURED \
         2026-09-16: (1) NO GRID — `fapi/v1/exchangeInfo` does not list this symbol (897 rows, \
         zero `*_PERP`), so `fetch_binance_properties` resolves nothing and the caller's FALLBACK \
         grid would be what orders are formatted on; the venue's real grid is tickSize 0.1, \
         stepSize 1, minQty 1, quantityPrecision 0, and quantity is in WHOLE CONTRACTS. (2) NO \
         CONTRACT SIZE — this bridge never sets `SymbolProperties::contract_size`, so `multiplier` \
         answers 1.0 where the venue says contractSize 100 (USD per contract): every notional, \
         margin, exposure and fee number would be off by 100x. (3) WRONG MARGIN ASSET — this \
         contract settles in BTC, not a stablecoin, and no row in \
         `crates/vike-venues/src/venue_margin_support.rs` or `venue_caps.rs` can say so. (4) WRONG \
         HOST — `run_perp` would sign against `fapi.binance.com`, whose instrument list does not \
         contain this symbol. ⚠ Do NOT reach for the fallback grid to get moving: a grid is what \
         an order is SIGNED over. The HISTORICAL path IS reachable and is what this venue offers \
         for COIN-M today — `vike_binance::data::fetch_klines_range` routes {symbol:?} to \
         `dapi.binance.com` and reads the base-asset volume column \
         (`vike_binance::instruments` is the routing authority).",
        contract.venue_word()
    )
}

/// The exec thread body for a symbol this adapter has REFUSED. It drains the command channel and
/// answers every order intent terminally, because the venue-adapter contract's "no order may
/// silently vanish" is not suspended by a refusal — a refused venue must reject LOUDLY, not go
/// quiet.
///
/// ⚠ **Returning from `run` instead would also reject**, through
/// `vike_bridge_core::exec_actor::ExecActor::submit`'s dead-thread arm — but with the reason
/// `"venue exec thread unavailable"`, which names nothing an operator can act on. The whole point
/// of the refusal is the message, so the thread stays alive to carry it.
fn run_refusing(reason: &str, events: &EventSender, rx: &Receiver<ExecCommand>) {
    use vike_model::events::{OrderCancelRejected, OrderModifyRejected, OrderRejected};
    while let Ok(cmd) = rx.recv() {
        match cmd {
            ExecCommand::Submit(request) => {
                let _ = events.blocking_send(Event::OrderRejected(OrderRejected {
                    client_order_id: request.client_order_id.clone(),
                    reason: reason.to_string().into(),
                    ts: request.ts,
                }));
            }
            // A cancel is advisory and NON-terminal, exactly as the shared actor treats an
            // undeliverable one: nothing was ever resting at this venue to cancel.
            ExecCommand::Cancel { client_order_id, .. } => {
                let _ = events.blocking_send(Event::OrderCancelRejected(OrderCancelRejected {
                    client_order_id,
                    reason: reason.to_string().into(),
                    ts: now_ms(),
                }));
            }
            ExecCommand::CancelBatch { client_order_ids, .. } => {
                for client_order_id in client_order_ids {
                    let _ = events.blocking_send(Event::OrderCancelRejected(OrderCancelRejected {
                        client_order_id,
                        reason: reason.to_string().into(),
                        ts: now_ms(),
                    }));
                }
            }
            ExecCommand::Modify { order, .. } => {
                let _ = events.blocking_send(Event::OrderModifyRejected(OrderModifyRejected {
                    client_order_id: order.client_order_id.clone(),
                    reason: reason.to_string().into(),
                    ts: order.ts,
                }));
            }
            ExecCommand::Shutdown => return,
        }
    }
}

/// SPOT driver: three REST clients (submit / resync-supervisor / gap-sentinel), the shared server-
/// time skew correction, the WS-API fills pump (+ audit-A3 resync), then the command loop.
#[expect(clippy::too_many_arguments)]
fn run_spot(
    env: AccountTier,
    creds: &Credentials,
    symbol: &str,
    properties: SymbolProperties,
    base_asset: &str,
    events: EventSender,
    rx: Receiver<ExecCommand>,
    on_reconcile: Option<mpsc::Sender<()>>,
    link_id: Option<String>,
    ledger: Arc<DispatchLedger>,
    coids: Arc<CoidSymbols>,
) {
    // Only the submit-capable client carries the resolved link id; the resync/sentinel clients are
    // history-only reads (`get_all_orders`/`get_my_trades`) that never identify an order by coid.
    // ONE order-rate window for it and every foreign-symbol spot client built later.
    let gate = crate::ratelimit::spot_rest_gate();
    let rest = mk_spot_rest_gated(
        env,
        creds,
        symbol,
        properties,
        base_asset,
        link_id.clone(),
        gate.clone(),
    );
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
    // From here the command loop signs each order on the ORDER's symbol: the mounted symbol (or none)
    // goes to `rest` unchanged, any other spot symbol to a client built on first use.
    let routed = PerSymbolRest::new(
        symbol,
        rest,
        foreign_spot_factory(env, creds.clone(), link_id, gate),
        coids,
    );

    let resync_symbol = symbol.to_string();
    let sentinel_symbol = symbol.to_string();
    // Private WS-API user-data (+ audit-A3 resync): authoritative fills/cancels straight into ingest.
    let feed = spawn_binance_user_data_with_resync(
        hosts_for(env).spot_ws.to_string(),
        creds.api_key.expose().clone(),
        creds.api_secret.expose().clone(),
        symbol.to_string(),
        events.clone(),
        move || spot_history_events(&resync_rest, &resync_symbol),
        on_reconcile,
    );
    // Gap-sentinel: after a submit/cancel, replay recent history `DEFAULT_FILL_SENTINEL` later to
    // recover a fill/cancel the WS lost (esp. the first-connect subscribe race — a market order
    // fills before the pump is subscribed, and the private WS does not replay on subscribe).
    // Dedup'd by the core, so a normal WS delivery makes it a no-op.
    run_loop(
        &Dispatched { rest: routed, ledger },
        &events,
        rx,
        || spot_history_events(&sentinel_rest, &sentinel_symbol),
        DEFAULT_FILL_SENTINEL,
    );
    let _ = feed.shutdown();
}

/// PERP driver: three fapi REST clients, a one-shot set-leverage, the listenKey fills pump (+ audit-
/// A3 resync), then the command loop. The REST clients use the EXCHANGE `api_symbol`. Every
/// execution the pump and the history replay emit carries the CORE `.P` series label so it folds
/// into the right position: the stream labels each frame's own `s` + `.P` (the stream is
/// account-wide), the replay labels `series_symbol`, and `series_symbol` is the stream's fallback for
/// a frame without `s`. [`crate::family::perp_series_symbol`] is the one spelling; until 2026-10-03
/// the stream labelled the bare `s` and every perp fill was dropped from the position.
#[expect(clippy::too_many_arguments)]
fn run_perp(
    env: AccountTier,
    creds: &Credentials,
    series_symbol: &str,
    api_symbol: &str,
    properties: SymbolProperties,
    events: EventSender,
    rx: Receiver<ExecCommand>,
    on_reconcile: Option<mpsc::Sender<()>>,
    link_id: Option<String>,
    leverage: f64,
    trade_lite_fill: bool,
    ledger: Arc<DispatchLedger>,
    coids: Arc<CoidSymbols>,
) {
    // Only the submit-capable client carries the resolved link id — see `run_spot`'s identical note.
    // All three carry the SAME mount-resolved `leverage`; only `rest` ever posts it (below).
    let gate = crate::ratelimit::perp_rest_gate();
    let rest = mk_perp_rest_gated(
        env,
        creds,
        api_symbol,
        properties,
        link_id.clone(),
        leverage,
        gate.clone(),
    );
    let resync_rest = mk_perp_rest(env, creds, api_symbol, properties, None, leverage);
    let sentinel_rest = mk_perp_rest(env, creds, api_symbol, properties, None, leverage);
    // fapi change-leverage is idempotent (HTTP-200 even at target); best-effort — a failure just
    // leaves the account's current leverage in place and orders still submit. UNCHANGED handling:
    // the value is now the operator's, but a rejection still only warns and never kills the start.
    if let Err(e) = rest.set_leverage() {
        tracing::warn!(target: "vike_binance::exec", symbol = %series_symbol, leverage, code = e.code, msg = %e.msg, "set_leverage failed (continuing) — the account keeps its CURRENT leverage while the RiskGate sizes against the configured one");
    }
    // From here the command loop signs each order on the ORDER's symbol: the mounted symbol (or none)
    // goes to `rest` unchanged, any other perp symbol to a client built on first use.
    // A foreign perp's CANCEL and status take a client with no grid fetch (a symbol and a coid are all
    // they send; the grid and leverage riding on it are never read), so they do not wait for
    // `exchangeInfo`. The lane check is the factory's own (an other-lane symbol is still refused).
    let light: ClientFactory<PerpRest> = {
        let (creds, link_id, gate) = (creds.clone(), link_id.clone(), gate.clone());
        Box::new(move |label| {
            let api_symbol = lane_symbol(label, true)?;
            Ok(mk_perp_rest_gated(
                env,
                &creds,
                &api_symbol,
                properties,
                link_id.clone(),
                DEFAULT_LEVERAGE,
                gate.clone(),
            ))
        })
    };
    let routed = PerSymbolRest::new(
        series_symbol,
        rest,
        foreign_perp_factory(env, creds.clone(), link_id, leverage, gate),
        coids,
    )
    .with_light_factory(light);

    let resync_symbol = series_symbol.to_string();
    let sentinel_symbol = series_symbol.to_string();
    // Private listenKey user-data (+ audit-A3 resync): fills carry the CORE `.P` label of their own
    // `s` (see this function's doc).
    let hosts = hosts_for(env);
    let feed = spawn_binance_perp_user_data_with_resync(
        hosts.fapi_rest.to_string(),
        hosts.fapi_ws.to_string(),
        creds.api_key.expose().clone(),
        series_symbol.to_string(),
        events.clone(),
        move || perp_history_events(&resync_rest, &resync_symbol),
        on_reconcile,
        trade_lite_fill,
    );
    run_loop(
        &Dispatched { rest: routed, ledger },
        &events,
        rx,
        || perp_history_events(&sentinel_rest, &sentinel_symbol),
        DEFAULT_FILL_SENTINEL,
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

    /// Rung-5 host resolution: ONLY `Live` selects mainnet; `Demo`/`Paper` (and, via the
    /// absent-creds-is-the-live-gate rule, any uncredentialed run) stay on the exact demo hosts
    /// binance used before env-threading — so the default order path is byte-identical.
    #[test]
    fn hosts_for_live_is_mainnet_others_stay_demo() {
        let live = hosts_for(AccountTier::Live);
        assert_eq!(live.spot_rest, MAINNET_REST);
        assert_eq!(live.fapi_rest, MAINNET_FAPI_REST);
        assert_eq!(live.spot_ws, MAINNET_WS);
        assert_eq!(live.fapi_ws, MAINNET_FAPI_WS);

        for env in [AccountTier::Demo, AccountTier::Paper] {
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
    /// decision `crate::adapter`'s `BinanceVenueAdapter::start` calls once, before anything is spawned.
    #[test]
    fn perp_leverage_follows_the_operator_risk_budget() {
        // Unset — both shapes of "the operator said nothing" — keeps today's literal EXACTLY.
        assert_eq!(leverage_for(None), DEFAULT_LEVERAGE);
        assert_eq!(leverage_for(None), 2.0, "the historical hardcoded /fapi/v1/leverage value");
        let silent = vike_model::ProfileRisk { max_leverage: None, ..Default::default() };
        assert_eq!(leverage_for(Some(&silent)), 2.0);

        // Set — the venue account is configured with the operator's own number, in BOTH
        // directions (the two ways the old hardcode was dangerous).
        let ten = vike_model::ProfileRisk { max_leverage: Some(10.0), ..Default::default() };
        assert_eq!(leverage_for(Some(&ten)), 10.0);
        let one = vike_model::ProfileRisk { max_leverage: Some(1.0), ..Default::default() };
        assert_eq!(leverage_for(Some(&one)), 1.0);

        // ONE number governs both sides: what we POST is the reciprocal of the initial-margin
        // fraction the RiskGate sizes against.
        assert_eq!(ten.im_requirement(), Some(0.1));
        assert_eq!(ten.im_requirement(), Some(1.0 / leverage_for(Some(&ten))));
        // The same invariant, via the shared helper the rule itself ships.
        assert!(vike_bridge_core::leverage::agrees_with_im_requirement(&ten, DEFAULT_LEVERAGE));
    }

    /// Decision 0095: `resolve_env_from` is default-safe over the bool the CEILING resolved (no
    /// flag reads anything any more) — `false` leaves `env` untouched, `true` UPGRADES to mainnet
    /// hosts, and it is idempotent.
    #[test]
    fn resolve_env_from_upgrades_only_when_the_ceiling_selected_mainnet() {
        // false: env is returned verbatim, so `Demo` (what `crate::adapter` passes under a demo
        // ceiling) stays demo → `hosts_for` yields the exact demo hosts (byte-identical default
        // path).
        assert_eq!(resolve_env_from(AccountTier::Demo, false), AccountTier::Demo);
        assert_eq!(resolve_env_from(AccountTier::Paper, false), AccountTier::Paper);
        let demo = hosts_for(resolve_env_from(AccountTier::Demo, false));
        assert_eq!(demo.spot_rest, DEMO_REST);
        assert_eq!(demo.fapi_ws, DEMO_FAPI_WS);

        // true: any caller env upgrades to Live → mainnet hosts.
        assert_eq!(resolve_env_from(AccountTier::Demo, true), AccountTier::Live);
        assert_eq!(resolve_env_from(AccountTier::Live, true), AccountTier::Live); // idempotent
        let main = hosts_for(resolve_env_from(AccountTier::Demo, true));
        assert_eq!(main.spot_rest, MAINNET_REST);
        assert_eq!(main.fapi_rest, MAINNET_FAPI_REST);
        assert_eq!(main.spot_ws, MAINNET_WS);
        assert_eq!(main.fapi_ws, MAINNET_FAPI_WS);
    }

    /// An engine runs ONE lane: an order on the other lane is refused with a reason naming both, a
    /// same-lane order yields its upper-cased exchange symbol (the `.P` stripped on a perp).
    #[test]
    fn an_order_on_the_other_lane_is_refused_not_signed() {
        assert_eq!(lane_symbol("ETHUSDT.P", true), Ok("ETHUSDT".to_string()));
        assert_eq!(lane_symbol("ethusdt", false), Ok("ETHUSDT".to_string()));
        let why = lane_symbol("ETHUSDT", true).expect_err("a spot symbol on the perp lane");
        assert!(why.contains("perp lane") && why.contains("spot symbol"), "{why}");
        let why = lane_symbol("ETHUSDT.P", false).expect_err("a perp symbol on the spot lane");
        assert!(why.contains("spot lane") && why.contains("perp symbol"), "{why}");
    }

    /// The gap-sentinel both drivers hand `run_loop` is bridge-core's mechanism default — the five
    /// seconds this file's private `FILL_SENTINEL` held before the number moved there — so a retune
    /// of that default is a deliberate edit here too, beside the cross-pin in vike-bridge-core.
    #[test]
    fn the_drivers_gap_sentinel_is_the_five_second_default() {
        assert_eq!(DEFAULT_FILL_SENTINEL, std::time::Duration::from_secs(5));
    }

    /// **A perpetual admitted on demand is written through the bridge's OWN set-leverage verb.** The
    /// client is the order client's build for the EXCHANGE symbol (the `.P` stripped, upper-cased) on
    /// the tier's fapi host, the POST names that symbol and a whole-number leverage, a refusal keeps the
    /// venue's words, and a SPOT symbol is refused before any client is built.
    #[test]
    fn a_perpetual_admitted_on_demand_is_written_through_the_bridges_own_set_leverage() {
        use std::cell::RefCell;

        /// `(method, path, params)` of every call.
        type Sent = Vec<(String, String, Vec<(String, String)>)>;
        struct Wire {
            sent: RefCell<Sent>,
            answer: Result<serde_json::Value, vike_bridge_core::transport::VenueApiError>,
        }
        impl RestTransport for Wire {
            fn signed(
                &self,
                _base_url: &str,
                path: &str,
                method: &str,
                params: &[(&str, String)],
                _signer: &dyn Signer,
            ) -> Result<serde_json::Value, vike_bridge_core::transport::VenueApiError> {
                let params = params.iter().map(|(k, v)| ((*k).to_string(), v.clone())).collect();
                self.sent.borrow_mut().push((method.to_string(), path.to_string(), params));
                self.answer.clone()
            }
            fn public(
                &self,
                _base_url: &str,
                _path: &str,
                _params: &[(&str, String)],
            ) -> Result<serde_json::Value, vike_bridge_core::transport::VenueApiError> {
                unreachable!("a set-leverage write reads nothing public")
            }
        }
        let creds = Credentials {
            api_key: "k".to_string().into(),
            api_secret: "s".to_string().into(),
            passphrase: None,
        };

        // The client: the exchange symbol, the figure, the tier's host, the order window.
        let demo = mk_perp_rest(
            AccountTier::Demo,
            &creds,
            &split_symbol("ethusdt.p".to_uppercase().as_str()).0,
            SymbolProperties::default(),
            None,
            10.0,
        );
        assert_eq!((demo.symbol.as_str(), demo.leverage), ("ETHUSDT", 10.0));
        assert_eq!(demo.base_url, DEMO_FAPI_REST);

        let over = |answer| BinancePerpRest {
            signer: BinanceHmacSigner::new(&creds, || 0),
            transport: Wire { sent: RefCell::new(Vec::new()), answer },
            base_url: DEMO_FAPI_REST.to_string(),
            symbol: "ETHUSDT".to_string(),
            properties: SymbolProperties::default(),
            leverage: 10.0,
            link_id: None,
        };

        // A clean write: ONE POST to /fapi/v1/leverage naming the exchange symbol and a whole number.
        let rest = over(Ok(serde_json::json!({ "leverage": 10, "symbol": "ETHUSDT" })));
        assert_eq!(post_symbol_leverage(&rest), Ok(()));
        let sent = rest.transport.sent.borrow();
        assert_eq!(sent.len(), 1, "one write");
        assert_eq!(
            (sent[0].0.as_str(), sent[0].1.as_str()),
            ("POST", crate::perp::PATH_SET_LEVERAGE)
        );
        assert!(sent[0].2.contains(&("symbol".to_string(), "ETHUSDT".to_string())));
        assert!(sent[0].2.contains(&("leverage".to_string(), "10".to_string())));
        drop(sent);

        // A refusal is the venue's words and the symbol.
        let rest = over(Err(vike_bridge_core::transport::VenueApiError {
            code: -4028,
            msg: "Leverage 125 is not valid".to_string(),
        }));
        let why = post_symbol_leverage(&rest).unwrap_err();
        assert!(why.contains("ETHUSDT") && why.contains("Leverage 125 is not valid"), "{why}");

        // A SPOT symbol has no leverage: refused in words, before any client exists.
        let why =
            write_symbol_leverage(&creds, AccountTier::Demo, "ETHUSDT", 10.0, None).unwrap_err();
        assert!(why.contains("SPOT") && why.contains(".P"), "{why}");
    }
}
