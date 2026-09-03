//! Aster live exec half — `ExecutionClient` over the signed REST, routing SPOT vs USDⓈ-M PERP by a
//! trailing `.P` on the symbol. A near-verbatim port of `binance::exec` — NOTHING here is
//! reimplemented; it is the wiring seam that assembles the existing Aster parts.
//!
//! Two structural differences from the binance template (see `spot.rs`/`perp.rs`):
//!   * base URLs are resolved from [`urls::urls_for`]`(env)` (testnet vs mainnet), threaded through
//!     `spawn`/`run`/`mk_*_rest`/`fetch_aster_properties` — NOT hardcoded DEMO consts;
//!   * requests are signed by the v3 EIP-712 [`AsterSigner`](crate::signing::AsterSigner), not
//!     Binance HMAC (the `nonce` is µs, minted inside the signer).
//!
//! * SPOT (`BTCUSDT`): [`AsterSpotRest`](crate::spot::AsterSpotRest) over `/api/v3`.
//! * PERP (`BTCUSDT.P`): [`AsterPerpRest`](crate::perp::AsterPerpRest) over `/fapi/v3` (native
//!   modify/batch), plus a one-shot set-leverage at start.
//!
//! Built on the shared [`ExecActor`] scaffold: `submit`/`cancel`/`modify` enqueue onto ONE dedicated
//! OS thread that drives the venue REST (blocking, off the single-writer core). `submit_order` emits
//! `[OrderSubmitted, OrderAccepted|OrderRejected]` synchronously into the core ingest.
//!
//! `.P` ROUTING (the SAME idiom the market feeds use): the trailing `.P` is stripped to the EXCHANGE
//! symbol (`BTCUSDT`) that drives the venue REST endpoints, while the ORIGINAL `.P`-suffixed symbol
//! stays the CORE/series label the fills carry (so a perp's position/order key never collides with
//! its spot twin). A non-`.P` symbol is byte-identical to spot-only behavior.
//!
//! LIVE GATE: absent credentials never reach here — the composition root only spawns this when
//! `load_aster_credentials` yields `Some`. With no `ASTER_*` keys the venue stays paper.
//!
//! SCOPE (Phase 4-6 + audit A3). The REST submit/cancel/modify path is wired end-to-end here; the
//! listenKey user-data FILLS pump (spot + perp, Phase 5 / Task 9) streams authoritative fills/cancels
//! back via `run_spot`/`run_perp`; per-order rate gates (`crate::ratelimit`, Phase 6 / Task 12) sit on
//! every REST transport; and the PIT `SymbolProperties` recorder (`crate::filters_rec`, Phase 6 /
//! Task 12) is wired into `run`'s startup fetch. Audit A3 (history gap-replay) is now wired: on a WS
//! reconnect the resync supervisor replays recent REST order/trade history
//! (`spot_history_events`/`perp_history_events` → [`crate::history`]) so a fill/cancel/expire/reject
//! that landed during the reconnect gap is recovered (the core dedups the overlap by `trade_id` +
//! FSM). The same replay drives the `run_loop` gap-sentinel (a fill the first-connect subscribe race
//! lost). This is the port of binance's `spot_history_events`/`perp_history_events` wiring.

use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::time::Duration;

use vike_bridge_core::credentials::Credentials;
use vike_bridge_core::exec_actor::{run_loop, ExecActor, ExecCommand};
use vike_bridge_core::transport::{RestTransport, UreqTransport};
use vike_bridge_core::Environment;
use vike_exec::{EventSender, ExecutionClient};
use vike_model::events::Event;
use vike_model::{now_ns, OrderRequest, SymbolProperties};

use vike_binance::family::exec_loop::split_symbol;

use crate::history::{map_aster_history, map_aster_perp_history};
use crate::perp::{parse_aster_perp_instruments, AsterPerpRest};
use crate::signing::AsterSigner;
use crate::spot::{parse_symbol_properties, AsterSpotRest};
use crate::urls;

/// Rows of recent order/trade history the audit-A3 resync replays after each WS reconnect.
const RESYNC_HISTORY_LIMIT: u32 = 50;
/// Gap-sentinel: after a submit/cancel, arm a resync this long later to recover a fill/cancel the WS
/// lost (esp. the first-connect subscribe race). Dedup'd by the core, so a normal WS delivery makes
/// it a no-op. The resync closure replays recent REST history (`spot_history_events`/
/// `perp_history_events`).
const FILL_SENTINEL: Duration = Duration::from_secs(5);
/// FALLBACK cross-margin leverage the perp exec thread sets once at start — the literal this arm
/// posted to `/fapi/v3/leverage` unconditionally before the operator's budget was threaded in
/// (testnet default; matches the smoke). Used ONLY when the operator expressed no leverage intent,
/// so a mount with no `[risk] max_leverage` is byte-identical to every mount before
/// [`leverage_for`] existed. ⚠ This venue mounts against MAINNET in practice (only `ASTER_LIVE_*`
/// credentials are configured), which is exactly why the unset path must not move. NEVER read on
/// the submit path — `AsterPerpRest::leverage` is consulted by `set_leverage` and nothing else.
const DEFAULT_LEVERAGE: f64 = 2.0;

/// The account leverage this arm POSTs at startup, resolved from the operator's `[risk]` budget.
///
/// Pure, and `pub` on purpose — the SAME shape binance uses: `vike_mount::make_engine` owns the
/// `ProfileRisk` and calls this ONCE per mount, then threads the resolved `f64` into
/// [`AsterExecutionClient::spawn_with_recorder`]. Nothing inside the adapter reaches for the
/// profile, so the number the venue account is configured with and the number the RiskGate sizes
/// against (`ProfileRisk::im_requirement`, `im = 1.0 / max_leverage`) can never disagree — that
/// disagreement was the bug this exists to close.
///
/// Unset (`None`, or a profile that never mentions `max_leverage`) ⇒ [`DEFAULT_LEVERAGE`],
/// byte-identical to before. See `vike_bridge_core::leverage` for the full rule.
///
/// ⚠ **NOT clamped to a venue ceiling, unlike bybit/okx** — `vike_bridge_core::leverage::
/// clamp_to_venue_cap` exists and this arm deliberately does not call it, for the same reason
/// binance does not (Aster's perp API is a Binance-fapi fork, so it inherits the gap verbatim).
/// VERIFIED LIVE 2026-08-05: `GET /fapi/v3/exchangeInfo` (735 KB, every symbol) contains ZERO
/// case-insensitive `leverage` matches, and `/fapi/v3/leverageBracket` answers `{"code":-1102,
/// "msg":"Mandatory parameter 'nonce' was not sent…"}` — i.e. a WHOLE NEW EIP-712-SIGNED request
/// on the mount path, not a field ride-along, and on a venue that mounts against MAINNET in
/// practice. Its brackets are risk-tiered like binance's, so a single flat cap does not exist to
/// be read. A too-high request is still rejected by the VENUE, which `run_perp`'s `set_leverage`
/// warn already surfaces.
pub fn leverage_for(profile: Option<&vike_exec::ProfileRisk>) -> f64 {
    vike_bridge_core::leverage::resolve_startup_leverage(profile, DEFAULT_LEVERAGE)
}

/// The concrete REST clients this exec half drives: blocking `ureq` + the v3 EIP-712 `AsterSigner`.
type SpotRest = AsterSpotRest<AsterSigner, UreqTransport>;
type PerpRest = AsterPerpRest<AsterSigner, UreqTransport>;

// AsterSigner's µs nonce comes from `vike_model::now_us` (Aster's `nonce` is µs, ±10 s of server
// time; consolidated helper — saturating, no clock-skew panic).
// TODO(testnet): wire AsterSigner::set_offset_us if the smoke shows nonce drift.

/// One fresh SPOT REST client for `symbol` (its own transport + signer, so it can live on its own
/// thread), with the base URL resolved from `env`. `base_asset` only matters for `connect()`
/// reconcile (unused on submit/cancel here), so an empty fallback is harmless.
fn mk_spot_rest(
    env: Environment,
    creds: &Credentials,
    symbol: &str,
    properties: SymbolProperties,
    base_asset: &str,
    builder: Option<(String, String)>,
) -> SpotRest {
    AsterSpotRest {
        signer: AsterSigner::new(creds, vike_model::now_us),
        transport: UreqTransport::new("aster").with_rate_gate(crate::ratelimit::spot_rest_gate()),
        base_url: urls::urls_for(env).sapi_rest.to_string(),
        symbol: symbol.to_string(),
        properties,
        base_asset: base_asset.to_string(),
        builder,
    }
}

/// One fresh PERP (fapi) REST client for the EXCHANGE `api_symbol` (its own transport + signer), with
/// the base URL resolved from `env`.
///
/// `leverage` is the mount-resolved account leverage ([`leverage_for`]) — the ONLY consumer is
/// `AsterPerpRest::set_leverage`, which `run_perp` fires once at start.
fn mk_perp_rest(
    env: Environment,
    creds: &Credentials,
    api_symbol: &str,
    properties: SymbolProperties,
    builder: Option<(String, String)>,
    leverage: f64,
) -> PerpRest {
    AsterPerpRest {
        signer: AsterSigner::new(creds, vike_model::now_us),
        transport: UreqTransport::new("aster").with_rate_gate(crate::ratelimit::perp_rest_gate()),
        base_url: urls::urls_for(env).fapi_rest.to_string(),
        symbol: api_symbol.to_string(),
        properties,
        leverage,
        builder,
    }
}

/// Fetch `symbol`'s REAL grid + base asset, ROUTING by the `.P` suffix: perp reads the fapi
/// `/fapi/v3/exchangeInfo`, spot reads `/api/v3/exchangeInfo`, both under `env`'s hosts. `None` on any
/// network/parse failure or unknown symbol → the caller uses its fallback grid. So the adapter formats
/// orders on the correct grid for ANY symbol on the right venue.
pub fn fetch_aster_properties(
    env: Environment,
    creds: &Credentials,
    symbol: &str,
) -> Option<(SymbolProperties, String)> {
    let (api_symbol, is_perp) = split_symbol(symbol);
    let _ = creds; // exchangeInfo is a PUBLIC endpoint (no signing) — creds kept for signature parity
    if is_perp {
        // One-shot startup fetch; gated for symmetry with mk_perp_rest (the submit-path transport
        // this grid formats orders for) rather than left ungated.
        let transport =
            UreqTransport::new("aster").with_rate_gate(crate::ratelimit::perp_rest_gate());
        let info = transport
            .public(
                urls::urls_for(env).fapi_rest,
                urls::PERP_PATH_EXCHANGE_INFO,
                &[("symbol", api_symbol.clone())],
            )
            .ok()?;
        let inst = parse_aster_perp_instruments(&info).get(&api_symbol.to_uppercase())?.clone();
        Some((inst.properties, inst.base_asset))
    } else {
        // One-shot startup fetch; gated for symmetry with mk_spot_rest (the submit-path transport
        // this grid formats orders for) rather than left ungated.
        let transport =
            UreqTransport::new("aster").with_rate_gate(crate::ratelimit::spot_rest_gate());
        let info = transport
            .public(
                urls::urls_for(env).sapi_rest,
                urls::SPOT_PATH_EXCHANGE_INFO,
                &[("symbol", api_symbol.clone())],
            )
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
/// The `signed_requery` transport is short-timeout, so a resync can't stall; a failed fetch degrades
/// to an empty array (no replay this round) rather than propagating.
fn spot_history_events(rest: &SpotRest, symbol: &str) -> Vec<Event> {
    let orders =
        rest.get_all_orders(RESYNC_HISTORY_LIMIT).unwrap_or_else(|_| serde_json::json!([]));
    let trades = rest.get_my_trades(RESYNC_HISTORY_LIMIT).unwrap_or_else(|_| serde_json::json!([]));
    map_aster_history(&orders, &trades, "aster", symbol)
}

/// Perp twin of [`spot_history_events`] (fapi `allOrders` + `userTrades` → `map_aster_perp_history`).
fn perp_history_events(rest: &PerpRest, symbol: &str) -> Vec<Event> {
    let orders =
        rest.get_all_orders(RESYNC_HISTORY_LIMIT).unwrap_or_else(|_| serde_json::json!([]));
    let trades =
        rest.get_user_trades(RESYNC_HISTORY_LIMIT).unwrap_or_else(|_| serde_json::json!([]));
    map_aster_perp_history(&orders, &trades, "aster", symbol)
}

/// Live Aster exec client (spot AND perp, chosen by the symbol's `.P` suffix). `submit`/`cancel`/
/// `modify` enqueue onto the REST thread (non-blocking); every venue event returns through the core
/// ingest. Dropping it (or `detach`) stops the REST thread — deterministic teardown via the owned
/// [`ExecActor`].
pub struct AsterExecutionClient(ExecActor);

impl AsterExecutionClient {
    /// Spawn the exec thread (fetches the instrument grid on `env`'s hosts, then drains commands).
    /// `symbol` is the CORE label — a trailing `.P` selects the USDⓈ-M perp venue, otherwise spot
    /// (e.g. `"BTCUSDT.P"` vs `"BTCUSDT"`). `fallback_properties` is used ONLY if the startup
    /// exchangeInfo fetch fails — the real grid is fetched at startup so any symbol formats correctly.
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
            DEFAULT_LEVERAGE,
        )
    }

    /// Same as [`Self::spawn`], plus an opt-in `properties_rec`: when present, the REAL fetched
    /// instrument grid (never the fallback) is recorded into the PIT properties store right after
    /// the startup exchangeInfo fetch resolves (via [`crate::filters_rec::record_properties_all`],
    /// keyed by the CORE `.P`-preserving label so it matches the series). `properties_rec` is moved
    /// into the exec thread (`PropertiesRecorder` is `Send + Sync`).
    ///
    /// `builder` — unified cross-venue attribution (task 8): the Aster Code `(builder address,
    /// feeRate)` pair, resolved by the caller from `attribution_code_from(vars, "aster")` +
    /// `ASTER_BUILDER_FEE_RATE`. `None` (unconfigured, the default) reproduces every order shape
    /// from before this parameter existed — see `perp.rs`/`spot.rs`'s `build_order_params`.
    ///
    /// `leverage` is the account leverage the PERP driver posts once at start
    /// (`POST /fapi/v3/leverage`), resolved ONCE at the mount site from the operator's `[risk]`
    /// budget via [`leverage_for`] — so the venue account and the RiskGate's margin math run on the
    /// SAME number. Pass [`DEFAULT_LEVERAGE`] (what [`Self::spawn`] does) for the historical
    /// literal. Inert on a SPOT symbol: spot has no leverage endpoint and never reads it.
    #[allow(clippy::too_many_arguments)]
    pub fn spawn_with_recorder(
        env: Environment,
        creds: Credentials,
        symbol: String,
        fallback_properties: SymbolProperties,
        events: EventSender,
        properties_rec: Option<Arc<vike_data::PropertiesRecorder>>,
        builder: Option<(String, String)>,
        leverage: f64,
    ) -> Self {
        let actor = ExecActor::spawn("aster-exec", events.clone(), move |rx| {
            run(
                env,
                creds,
                symbol,
                fallback_properties,
                events,
                rx,
                properties_rec,
                builder,
                leverage,
            )
        });
        Self(actor)
    }
}

impl ExecutionClient for AsterExecutionClient {
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

/// The exec thread: resolve the grid (routed by `.P`), then dispatch to the spot or perp driver. ALL
/// network I/O lives on this thread — off the single-writer core.
#[allow(clippy::too_many_arguments)]
fn run(
    env: Environment,
    creds: Credentials,
    symbol: String,
    fallback_properties: SymbolProperties,
    events: EventSender,
    rx: Receiver<ExecCommand>,
    properties_rec: Option<Arc<vike_data::PropertiesRecorder>>,
    builder: Option<(String, String)>,
    leverage: f64,
) {
    let (api_symbol, is_perp) = split_symbol(&symbol);
    // Fetch the venue's REAL instrument grid for `symbol` (off the core thread); fall back to the
    // caller's default only if the fetch fails, so orders format on the right tick/step.
    let (properties, base_asset) = match fetch_aster_properties(env, &creds, &symbol) {
        Some((real, base)) => {
            crate::filters_rec::record_properties_all(
                &properties_rec,
                [(symbol.to_uppercase(), real)],
                now_ns(),
            );
            (real, base)
        }
        None => {
            tracing::warn!(target: "vike_aster::exec", %symbol, is_perp, "exchangeInfo fetch failed; using fallback properties");
            (fallback_properties, String::new())
        }
    };
    if is_perp {
        run_perp(env, &creds, &symbol, &api_symbol, properties, events, rx, builder, leverage);
    } else {
        // SPOT has no set-leverage endpoint — `leverage` is deliberately not threaded here.
        run_spot(env, &creds, &symbol, properties, &base_asset, events, rx, builder);
    }
}

/// SPOT driver: three REST clients (submit / resync-supervisor / gap-sentinel), the listenKey fills
/// pump (+ audit-A3 resync), then the command loop. Mirrors binance's `run_spot`.
///
/// The listenKey user-data pump (Task 9) streams authoritative fills/cancels straight into ingest;
/// on a reconnect the resync supervisor replays recent REST order/trade history
/// (`spot_history_events`) so a terminal that landed during the WS gap is recovered (the core dedups
/// the overlap). The same replay drives the `run_loop` gap-sentinel.
#[allow(clippy::too_many_arguments)]
fn run_spot(
    env: Environment,
    creds: &Credentials,
    symbol: &str,
    properties: SymbolProperties,
    base_asset: &str,
    events: EventSender,
    rx: Receiver<ExecCommand>,
    builder: Option<(String, String)>,
) {
    // Only the submit-path client needs the builder pair — resync_rest/sentinel_rest only ever GET
    // history (they never call `build_order_params`), so they stay `None`.
    let rest = mk_spot_rest(env, creds, symbol, properties, base_asset, builder);
    let resync_rest = mk_spot_rest(env, creds, symbol, properties, base_asset, None);
    let sentinel_rest = mk_spot_rest(env, creds, symbol, properties, base_asset, None);

    let resync_symbol = symbol.to_string();
    let sentinel_symbol = symbol.to_string();
    // Task 9 + audit A3: authoritative fills stream back via the listenKey user-data pump, and the
    // resync supervisor replays recent history on each reconnect (held for the driver's lifetime;
    // deterministic teardown when the command loop returns). `None` on_reconcile: the exec driver
    // wires event-replay only (no reconcile poke).
    let feed = crate::user_data::spawn_aster_user_data_with_resync(
        env,
        creds.clone(),
        urls::urls_for(env).sapi_ws.to_string(),
        symbol.to_string(),
        events.clone(),
        move || spot_history_events(&resync_rest, &resync_symbol),
        None,
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

/// PERP driver: three fapi REST clients (submit / resync-supervisor / gap-sentinel), a one-shot
/// set-leverage, the listenKey fills pump (+ audit-A3 resync), then the command loop. The REST
/// clients use the EXCHANGE `api_symbol`; the fills pump + history use the CORE `series_symbol` (the
/// `.P` label) so a fill lacking the frame's `s` folds into the right position (never colliding with
/// its spot twin) — mirroring binance's `series_symbol` pump wiring.
///
/// On a reconnect the resync supervisor replays recent fapi order/trade history
/// (`perp_history_events`) so a terminal that landed during the WS gap is recovered (the core dedups
/// the overlap). The same replay drives the `run_loop` gap-sentinel.
#[allow(clippy::too_many_arguments)]
fn run_perp(
    env: Environment,
    creds: &Credentials,
    series_symbol: &str,
    api_symbol: &str,
    properties: SymbolProperties,
    events: EventSender,
    rx: Receiver<ExecCommand>,
    builder: Option<(String, String)>,
    leverage: f64,
) {
    // Only the submit-path client needs the builder pair — resync_rest/sentinel_rest only ever GET
    // history (they never call `build_order_params`), so they stay `None`.
    // All three carry the SAME mount-resolved `leverage`; only `rest` ever posts it (below).
    let rest = mk_perp_rest(env, creds, api_symbol, properties, builder, leverage);
    let resync_rest = mk_perp_rest(env, creds, api_symbol, properties, None, leverage);
    let sentinel_rest = mk_perp_rest(env, creds, api_symbol, properties, None, leverage);
    // fapi change-leverage is idempotent (HTTP-200 even at target); best-effort — a failure just
    // leaves the account's current leverage in place and orders still submit. UNCHANGED handling:
    // the value is now the operator's, but a rejection still only warns and never kills the mount.
    if let Err(e) = rest.set_leverage() {
        tracing::warn!(target: "vike_aster::exec", symbol = %series_symbol, leverage, code = e.code, msg = %e.msg, "set_leverage failed (continuing) — the account keeps its CURRENT leverage while the RiskGate sizes against the configured one");
    }

    let resync_symbol = series_symbol.to_string();
    let sentinel_symbol = series_symbol.to_string();
    // Task 9 + audit A3: authoritative fills stream back via the listenKey user-data pump (fills carry
    // the CORE `.P` series label so they fold into the perp position), and the resync supervisor
    // replays recent history on each reconnect. Held for the driver's lifetime. `None` on_reconcile:
    // the exec driver wires event-replay only (no reconcile poke).
    let feed = crate::perp_user_data::spawn_aster_perp_user_data_with_resync(
        env,
        creds.clone(),
        urls::urls_for(env).fapi_ws.to_string(),
        series_symbol.to_string(),
        events.clone(),
        move || perp_history_events(&resync_rest, &resync_symbol),
        None,
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
    //! Aster-specific exec tests: `.P` SPOT/PERP routing + testnet host selection, asserted with NO
    //! network. The generic command→event wiring tests (a mock `VenueRest` proving `run_loop` routes
    //! Submit/Cancel/Modify and fires the gap-sentinel) live WITH the shared `run_loop` in
    //! `vike_bridge_core::exec_actor::run_loop_tests` — aster drives that SAME `run_loop`, so they are
    //! not re-hosted here (the pattern binance/bybit/okx/deribit already follow after the MockRest
    //! consolidation). The aster native-modify reality tie (`caps_for("aster").supports_modify`) lives
    //! in the lib `caps_test`.
    use super::*;

    /// `.P` routing (NO network): the trailing `.P` selects the perp venue + strips to the exchange
    /// symbol; a bare symbol stays spot. This is the exact decision `run` makes before it dispatches
    /// to `run_perp` / `run_spot`. The hosts the drivers pin are resolved from `urls::urls_for` (not
    /// hardcoded consts), so a `Demo` run provably reaches the Aster testnet REST/WS endpoints.
    #[test]
    fn dot_p_routes_to_fapi_hosts_and_strips_symbol() {
        // perp: `.P` stripped, perp flag set
        assert_eq!(split_symbol("BTCUSDT.P"), ("BTCUSDT".to_string(), true));
        // spot: unchanged, non-perp
        assert_eq!(split_symbol("BTCUSDT"), ("BTCUSDT".to_string(), false));
        // the testnet hosts the drivers pin (the endpoints `run_perp`/`run_spot` actually use)
        let u = urls::urls_for(vike_bridge_core::Environment::Demo);
        assert_eq!(u.fapi_rest, "https://fapi.asterdex-testnet.com");
        assert_eq!(u.sapi_rest, "https://sapi.asterdex-testnet.com");
        assert_eq!(u.fapi_ws, "wss://fstream.asterdex-testnet.com");
        assert_eq!(u.sapi_ws, "wss://sstream.asterdex-testnet.com");
    }

    /// The leverage this arm POSTs to `/fapi/v3/leverage` is the OPERATOR's `[risk] max_leverage`,
    /// and an unset profile still yields the historical literal (`2.0`). ⚠ On THIS venue that
    /// unset-is-unchanged property guards a MAINNET account (only `ASTER_LIVE_*` credentials
    /// exist), so it is the one assertion here that protects real money. NO network:
    /// `leverage_for` is the pure decision `make_engine` calls once, before anything is spawned.
    #[test]
    fn perp_leverage_follows_the_operator_risk_budget() {
        // Unset — both shapes of "the operator said nothing" — keeps today's literal EXACTLY.
        assert_eq!(leverage_for(None), DEFAULT_LEVERAGE);
        assert_eq!(leverage_for(None), 2.0, "the historical hardcoded /fapi/v3/leverage value");
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
}
