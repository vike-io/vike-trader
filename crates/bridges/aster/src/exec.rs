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
//! `load_aster_credentials` yields `Some`. An account holding no `USER` + `PRIVATE_KEY` does not run.
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

use std::sync::Arc;
use std::sync::mpsc::Receiver;

use vike_bridge_core::exec_actor::{
    ConfirmFn, DEFAULT_FILL_SENTINEL, ExecActor, ExecCommand, run_loop,
};
use vike_bridge_core::ratelimit::RateGate;
use vike_bridge_core::transport::{RestTransport, UreqTransport};
use vike_exec::{EventSender, ExecutionClient};
use vike_model::AccountTier;
use vike_model::events::Event;
use vike_model::{OrderRequest, SymbolProperties, now_ns};
use vike_secrets::credentials::Credentials;

use vike_binance::family::exec_loop::split_symbol;
use vike_binance::family::order_map::parse_symbol_properties;
use vike_binance::per_symbol::{ClientFactory, CoidSymbols, PerSymbolRest};
use vike_bridge_core::confirm::{DispatchLedger, Dispatched, confirm_closure};

use crate::history::{map_aster_history, map_aster_perp_history};
use crate::perp::{AsterPerpRest, parse_aster_perp_instruments};
use crate::signing::AsterSigner;
use crate::spot::AsterSpotRest;
use crate::urls;

/// Rows of recent order/trade history the audit-A3 resync replays after each WS reconnect.
const RESYNC_HISTORY_LIMIT: u32 = 50;
/// FALLBACK cross-margin leverage the perp exec thread sets once at start — the literal this arm
/// posted to `/fapi/v3/leverage` unconditionally before the operator's budget was threaded in
/// (testnet default; matches the smoke). Used ONLY when the operator expressed no leverage intent,
/// so a start with no `[risk] max_leverage` is byte-identical to every start before
/// [`leverage_for`] existed. ⚠ This venue starts against MAINNET in practice (only a `live`-tier
/// aster account holds credentials), which is exactly why the unset path must not move. NEVER read on
/// the submit path — `AsterPerpRest::leverage` is consulted by `set_leverage` and nothing else.
const DEFAULT_LEVERAGE: f64 = 2.0;

/// The account leverage this arm POSTs at startup, resolved from the operator's `[risk]` budget.
///
/// Pure, and `pub` on purpose — the SAME shape binance uses: `AsterVenueAdapter::start` calls this
/// ONCE per start with the `ProfileRisk` `vike-tradehub-core` hands it, then threads the resolved `f64` into
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
/// on the start path, not a field ride-along, and on a venue that starts against MAINNET in
/// practice. Its brackets are risk-tiered like binance's, so a single flat cap does not exist to
/// be read. A too-high request is still rejected by the VENUE, which `run_perp`'s `set_leverage`
/// warn already surfaces.
pub fn leverage_for(profile: Option<&vike_model::ProfileRisk>) -> f64 {
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
    env: AccountTier,
    creds: &Credentials,
    symbol: &str,
    properties: SymbolProperties,
    base_asset: &str,
    builder: Option<(String, String)>,
) -> SpotRest {
    mk_spot_rest_gated(
        env,
        creds,
        symbol,
        properties,
        base_asset,
        builder,
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
    builder: Option<(String, String)>,
    gate: RateGate,
) -> SpotRest {
    AsterSpotRest {
        signer: AsterSigner::new(creds, vike_model::now_us),
        transport: UreqTransport::new("aster").with_rate_gate(gate),
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
    env: AccountTier,
    creds: &Credentials,
    api_symbol: &str,
    properties: SymbolProperties,
    builder: Option<(String, String)>,
    leverage: f64,
) -> PerpRest {
    mk_perp_rest_gated(
        env,
        creds,
        api_symbol,
        properties,
        builder,
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
    builder: Option<(String, String)>,
    leverage: f64,
    gate: RateGate,
) -> PerpRest {
    AsterPerpRest {
        signer: AsterSigner::new(creds, vike_model::now_us),
        transport: UreqTransport::new("aster").with_rate_gate(gate),
        base_url: urls::urls_for(env).fapi_rest.to_string(),
        symbol: api_symbol.to_string(),
        properties,
        leverage,
        builder,
    }
}

/// The venue's active order-status re-query for [`ExecActor::with_confirm`]: the core's
/// `Command::ConfirmOrder` lands here on a short-lived worker thread (blocking REST, never on the
/// core fold), and runs the SAME by-coid lookup the ambiguous-submit arm uses
/// (`vike_bridge_core::confirm`): found -> `OrderAccepted` (+ the terminal when the venue
/// already closed it), -2013 -> the "venue confirms absent" reject ONLY once the exec thread has
/// finished this coid's submit (`ledger`), a FAILED query -> nothing. ONE REST client serves every
/// confirm (one TLS pool), bounded and backed off by `confirm_closure`. Aster stamps no coid
/// prefix, so the bare id is the lookup key; the grid is the fallback one and the builder pair is
/// `None` (a status GET reads neither). Aster signs a nonce, not a timestamp: no server-time read.
///
/// The one client is a `PerSymbolRest`: a coid the exec thread recorded as a FOREIGN order (`coids`)
/// is asked about on its own symbol, any other on the mounted one. The foreign-symbol clients here
/// read no grid (a status GET never formats a price), so building one costs no network.
fn confirm_fn(
    env: AccountTier,
    creds: &Credentials,
    symbol: &str,
    fallback_properties: SymbolProperties,
    ledger: Arc<DispatchLedger>,
    coids: Arc<CoidSymbols>,
) -> ConfirmFn {
    let (api_symbol, is_perp) = split_symbol(symbol);
    let creds_owned = creds.clone();
    if is_perp {
        let rest =
            mk_perp_rest(env, creds, &api_symbol, fallback_properties, None, DEFAULT_LEVERAGE);
        let make: ClientFactory<PerpRest> = Box::new(move |label| {
            let api_symbol = lane_symbol(label, true)?;
            Ok(mk_perp_rest(
                env,
                &creds_owned,
                &api_symbol,
                fallback_properties,
                None,
                DEFAULT_LEVERAGE,
            ))
        });
        confirm_closure(PerSymbolRest::new(symbol, rest, make, coids), ledger, |_| true)
    } else {
        let rest = mk_spot_rest(env, creds, symbol, fallback_properties, "", None);
        let make: ClientFactory<SpotRest> = Box::new(move |label| {
            let api_symbol = lane_symbol(label, false)?;
            Ok(mk_spot_rest(env, &creds_owned, &api_symbol, fallback_properties, "", None))
        });
        confirm_closure(PerSymbolRest::new(symbol, rest, make, coids), ledger, |_| true)
    }
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
        "aster: cannot sign {label:?}: this account's engine runs the {this} lane and {label:?} \
         is a {other} symbol; the {other} lane has no user-data stream here, so its fills would \
         never arrive. The order was not sent"
    ))
}

/// Why an order on `label` is refused when its grid cannot be read: never a guessed or BTC-shaped
/// fallback grid.
fn no_grid_reason(label: &str) -> String {
    format!(
        "aster: cannot sign {label:?}: exchangeInfo has no usable grid for it (unknown symbol, or \
         the venue was unreachable); the order was not sent"
    )
}

/// The `PerSymbolRest` factory for a PERP engine: a signing client for another perp of the SAME
/// account (same keys, host, builder pair, leverage value and order-rate window as the mounted
/// client) on that symbol's own fetched grid. The grid fetch is one blocking public `exchangeInfo`
/// GET on the exec thread, bounded by the transport's timeout. `leverage` is carried but NOT posted
/// for the new symbol: the venue keeps its current leverage there (phase 1f is not done here).
fn foreign_perp_factory(
    env: AccountTier,
    creds: Credentials,
    builder: Option<(String, String)>,
    leverage: f64,
    gate: RateGate,
) -> ClientFactory<PerpRest> {
    Box::new(move |label| {
        let label = label.to_uppercase();
        let api_symbol = lane_symbol(&label, true)?;
        let (properties, _) =
            fetch_aster_properties(env, &creds, &label).ok_or_else(|| no_grid_reason(&label))?;
        // ⚠ NO `set_leverage` here (see binance's `foreign_perp_factory`): it would also run after a
        // restart and move an existing position's leverage.
        Ok(mk_perp_rest_gated(
            env,
            &creds,
            &api_symbol,
            properties,
            builder.clone(),
            leverage,
            gate.clone(),
        ))
    })
}

/// [`foreign_perp_factory`]'s spot twin (aster signs a nonce, not a timestamp: no server-time read).
fn foreign_spot_factory(
    env: AccountTier,
    creds: Credentials,
    builder: Option<(String, String)>,
    gate: RateGate,
) -> ClientFactory<SpotRest> {
    Box::new(move |label| {
        let label = label.to_uppercase();
        let api_symbol = lane_symbol(&label, false)?;
        let (properties, base_asset) =
            fetch_aster_properties(env, &creds, &label).ok_or_else(|| no_grid_reason(&label))?;
        Ok(mk_spot_rest_gated(
            env,
            &creds,
            &api_symbol,
            properties,
            &base_asset,
            builder.clone(),
            gate.clone(),
        ))
    })
}

/// Fetch `symbol`'s REAL grid + base asset, ROUTING by the `.P` suffix: perp reads the fapi
/// `/fapi/v3/exchangeInfo`, spot reads `/api/v3/exchangeInfo`, both under `env`'s hosts. `None` on any
/// network/parse failure or unknown symbol → the caller uses its fallback grid. So the adapter formats
/// orders on the correct grid for ANY symbol on the right venue.
pub fn fetch_aster_properties(
    env: AccountTier,
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
pub struct AsterExecutionClient(ExecActor, Arc<CoidSymbols>);

impl AsterExecutionClient {
    /// Hand this client the HALT sentinel it watches: the venue's start passes
    /// `AdapterInputs::process.halt_path`, the one file `vike-tradehub-core` resolved for the whole process
    /// (`vike_bridge_core::exec_actor::ExecActor::with_halt_path` carries the contract). A client
    /// built without it watches NOTHING — `crates/vike-ops/tests/settings_secrets/bridge_inputs_gate.rs` holds that
    /// every live start calls this.
    #[must_use]
    pub fn with_halt_path(self, path: std::path::PathBuf) -> Self {
        Self(self.0.with_halt_path(path), self.1)
    }

    /// Spawn the exec thread (fetches the instrument grid on `env`'s hosts, then drains commands).
    /// `symbol` is the CORE label — a trailing `.P` selects the USDⓈ-M perp venue, otherwise spot
    /// (e.g. `"BTCUSDT.P"` vs `"BTCUSDT"`). `fallback_properties` is used ONLY if the startup
    /// exchangeInfo fetch fails — the real grid is fetched at startup so any symbol formats correctly.
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
    /// (`POST /fapi/v3/leverage`), resolved ONCE at the start site from the operator's `[risk]`
    /// budget via [`leverage_for`] — so the venue account and the RiskGate's margin math run on the
    /// SAME number. Pass [`DEFAULT_LEVERAGE`] (what [`Self::spawn`] does) for the historical
    /// literal. Inert on a SPOT symbol: spot has no leverage endpoint and never reads it.
    pub fn spawn_with_recorder(
        env: AccountTier,
        creds: Credentials,
        symbol: String,
        fallback_properties: SymbolProperties,
        events: EventSender,
        properties_rec: Option<Arc<vike_data::PropertiesRecorder>>,
        builder: Option<(String, String)>,
        leverage: f64,
    ) -> Self {
        let ledger = Arc::new(DispatchLedger::default());
        // Which symbol each FOREIGN order was submitted on: written by the exec thread's router, read by
        // the confirm worker's, so a status query lands on the order's own symbol.
        let coids = Arc::new(CoidSymbols::default());
        let adopt_coids = coids.clone();
        let confirm =
            confirm_fn(env, &creds, &symbol, fallback_properties, ledger.clone(), coids.clone());
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
                ledger,
                coids,
            )
        })
        .with_confirm(confirm);
        Self(actor, adopt_coids)
    }
}

impl ExecutionClient for AsterExecutionClient {
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

/// The exec thread: resolve the grid (routed by `.P`), then dispatch to the spot or perp driver. ALL
/// network I/O lives on this thread — off the single-writer core.
#[expect(clippy::too_many_arguments)]
fn run(
    env: AccountTier,
    creds: Credentials,
    symbol: String,
    fallback_properties: SymbolProperties,
    events: EventSender,
    rx: Receiver<ExecCommand>,
    properties_rec: Option<Arc<vike_data::PropertiesRecorder>>,
    builder: Option<(String, String)>,
    leverage: f64,
    ledger: Arc<DispatchLedger>,
    coids: Arc<CoidSymbols>,
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
        run_perp(
            env,
            &creds,
            &symbol,
            &api_symbol,
            properties,
            events,
            rx,
            builder,
            leverage,
            ledger,
            coids,
        );
    } else {
        // SPOT has no set-leverage endpoint — `leverage` is deliberately not threaded here.
        run_spot(env, &creds, &symbol, properties, &base_asset, events, rx, builder, ledger, coids);
    }
}

/// SPOT driver: three REST clients (submit / resync-supervisor / gap-sentinel), the listenKey fills
/// pump (+ audit-A3 resync), then the command loop. Mirrors binance's `run_spot`.
///
/// The listenKey user-data pump (Task 9) streams authoritative fills/cancels straight into ingest;
/// on a reconnect the resync supervisor replays recent REST order/trade history
/// (`spot_history_events`) so a terminal that landed during the WS gap is recovered (the core dedups
/// the overlap). The same replay drives the `run_loop` gap-sentinel.
fn run_spot(
    env: AccountTier,
    creds: &Credentials,
    symbol: &str,
    properties: SymbolProperties,
    base_asset: &str,
    events: EventSender,
    rx: Receiver<ExecCommand>,
    builder: Option<(String, String)>,
    ledger: Arc<DispatchLedger>,
    coids: Arc<CoidSymbols>,
) {
    // Only the submit-path client needs the builder pair — resync_rest/sentinel_rest only ever GET
    // history (they never call `build_order_params`), so they stay `None`.
    // ONE order-rate window for it and every foreign-symbol spot client built later.
    let gate = crate::ratelimit::spot_rest_gate();
    let rest = mk_spot_rest_gated(
        env,
        creds,
        symbol,
        properties,
        base_asset,
        builder.clone(),
        gate.clone(),
    );
    let resync_rest = mk_spot_rest(env, creds, symbol, properties, base_asset, None);
    let sentinel_rest = mk_spot_rest(env, creds, symbol, properties, base_asset, None);
    // From here the command loop signs each order on the ORDER's symbol: the mounted symbol (or none)
    // goes to `rest` unchanged, any other spot symbol to a client built on first use.
    let routed = PerSymbolRest::new(
        symbol,
        rest,
        foreign_spot_factory(env, creds.clone(), builder, gate),
        coids,
    );

    let resync_symbol = symbol.to_string();
    let sentinel_symbol = symbol.to_string();
    // Task 9 + audit A3: authoritative fills stream back via the listenKey user-data pump, and the
    // resync supervisor replays recent history on each reconnect (held for the driver's lifetime;
    // deterministic teardown when the command loop returns). `None` on_reconcile: the exec driver
    // wires event-replay only (no recon trigger).
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
        &Dispatched { rest: routed, ledger },
        &events,
        rx,
        || spot_history_events(&sentinel_rest, &sentinel_symbol),
        DEFAULT_FILL_SENTINEL,
    );
    let _ = feed.shutdown();
}

/// PERP driver: three fapi REST clients (submit / resync-supervisor / gap-sentinel), a one-shot
/// set-leverage, the listenKey fills pump (+ audit-A3 resync), then the command loop. The REST
/// clients use the EXCHANGE `api_symbol`. Every execution the pump and the history replay emit is
/// labelled with the CORE `.P` series label, so it folds into the perp position and never collides
/// with its spot twin: the stream labels each frame's own `s` + `.P` (an account-wide stream, so a
/// foreign symbol keeps its own label), the replay labels `series_symbol`, and `series_symbol` is
/// the stream's fallback for a frame without `s` — mirroring binance's `series_symbol` wiring.
/// `vike_binance::family::perp_series_symbol` is the one spelling.
///
/// On a reconnect the resync supervisor replays recent fapi order/trade history
/// (`perp_history_events`) so a terminal that landed during the WS gap is recovered (the core dedups
/// the overlap). The same replay drives the `run_loop` gap-sentinel.
#[expect(clippy::too_many_arguments)]
fn run_perp(
    env: AccountTier,
    creds: &Credentials,
    series_symbol: &str,
    api_symbol: &str,
    properties: SymbolProperties,
    events: EventSender,
    rx: Receiver<ExecCommand>,
    builder: Option<(String, String)>,
    leverage: f64,
    ledger: Arc<DispatchLedger>,
    coids: Arc<CoidSymbols>,
) {
    // Only the submit-path client needs the builder pair — resync_rest/sentinel_rest only ever GET
    // history (they never call `build_order_params`), so they stay `None`.
    // All three carry the SAME mount-resolved `leverage`; only `rest` ever posts it (below).
    let gate = crate::ratelimit::perp_rest_gate();
    let rest = mk_perp_rest_gated(
        env,
        creds,
        api_symbol,
        properties,
        builder.clone(),
        leverage,
        gate.clone(),
    );
    let resync_rest = mk_perp_rest(env, creds, api_symbol, properties, None, leverage);
    let sentinel_rest = mk_perp_rest(env, creds, api_symbol, properties, None, leverage);
    // fapi change-leverage is idempotent (HTTP-200 even at target); best-effort — a failure just
    // leaves the account's current leverage in place and orders still submit. UNCHANGED handling:
    // the value is now the operator's, but a rejection still only warns and never kills the start.
    if let Err(e) = rest.set_leverage() {
        tracing::warn!(target: "vike_aster::exec", symbol = %series_symbol, leverage, code = e.code, msg = %e.msg, "set_leverage failed (continuing) — the account keeps its CURRENT leverage while the RiskGate sizes against the configured one");
    }
    // From here the command loop signs each order on the ORDER's symbol: the mounted symbol (or none)
    // goes to `rest` unchanged, any other perp symbol to a client built on first use.
    let routed = PerSymbolRest::new(
        series_symbol,
        rest,
        foreign_perp_factory(env, creds.clone(), builder, leverage, gate),
        coids,
    );

    let resync_symbol = series_symbol.to_string();
    let sentinel_symbol = series_symbol.to_string();
    // Task 9 + audit A3: authoritative fills stream back via the listenKey user-data pump (fills carry
    // the CORE `.P` series label of their own `s` so they fold into the perp position — see this
    // function's doc), and the resync supervisor
    // replays recent history on each reconnect. Held for the driver's lifetime. `None` on_reconcile:
    // the exec driver wires event-replay only (no recon trigger).
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
        let u = urls::urls_for(vike_model::AccountTier::Demo);
        assert_eq!(u.fapi_rest, "https://fapi.asterdex-testnet.com");
        assert_eq!(u.sapi_rest, "https://sapi.asterdex-testnet.com");
        assert_eq!(u.fapi_ws, "wss://fstream.asterdex-testnet.com");
        assert_eq!(u.sapi_ws, "wss://sstream.asterdex-testnet.com");
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

    /// The leverage this arm POSTs to `/fapi/v3/leverage` is the OPERATOR's `[risk] max_leverage`,
    /// and an unset profile still yields the historical literal (`2.0`). ⚠ On THIS venue that
    /// unset-is-unchanged property guards a MAINNET account (only a `live`-tier aster account
    /// holds credentials), so it is the one assertion here that protects real money. NO network:
    /// `leverage_for` is the pure decision `AsterVenueAdapter::start` calls once, before anything is
    /// spawned.
    #[test]
    fn perp_leverage_follows_the_operator_risk_budget() {
        // Unset — both shapes of "the operator said nothing" — keeps today's literal EXACTLY.
        assert_eq!(leverage_for(None), DEFAULT_LEVERAGE);
        assert_eq!(leverage_for(None), 2.0, "the historical hardcoded /fapi/v3/leverage value");
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

    /// The gap-sentinel both drivers hand `run_loop` is bridge-core's mechanism default — the five
    /// seconds this file's private `FILL_SENTINEL` held before the number moved there — so a retune
    /// of that default is a deliberate edit here too, beside the cross-pin in vike-bridge-core.
    #[test]
    fn the_drivers_gap_sentinel_is_the_five_second_default() {
        assert_eq!(DEFAULT_FILL_SENTINEL, std::time::Duration::from_secs(5));
    }
}
