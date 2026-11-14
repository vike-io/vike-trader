//! Bybit live exec half — `ExecutionClient` over the V5 linear-perp REST + the private user-data WS.
//!
//! The client IS the shared [`ExecutionClientActor`], returned by [`spawn_with_recorder`] with this venue's hooks (no wrapper
//! of its own): `submit`/`cancel` enqueue onto ONE dedicated OS thread
//! that drives [`VenueRestBybit`] (blocking REST off the single-writer core). `submit_order` emits
//! `[OrderSubmitted, OrderAccepted|OrderRejected]` synchronously; the authoritative async fills/cancels
//! return on the private WS via [`crate::execution::user_data::spawn_bybit_perp_user_data_with_resync`] straight into the core ingest.
//!
//! LIVE GATE: absent credentials never reach here — the start only spawns this when
//! `vike_secrets::credentials::api_credentials` yields `Some` for the started account (the
//! codebase's absent-creds-is-the-live-gate rule). An account with no keys does not run.
//!
//! Scope: `submit`/`cancel` (the DOM's click-to-place / cancel surface) and a native `modify`
//! (`ExecutionClientActorCommand::Modify` → `rest.modify_order` in the shared `run_loop`;
//! `crates/bridges/bybit/src/lib.rs`'s `capabilities_test` ties `supports_modify` to it). Batch is NOT
//! wired at the client seam: it falls through to the [`vike_execution::ExecutionClient`] fan-out default
//! (`supports_native_batch` is false) — a native batch is a follow-up (`VenueRestBybit` already has
//! `submit_batch`, but this client forwards no batch lane to it).

pub mod event_mapper;
pub mod funding;
pub mod rest;
pub mod resync;
pub mod user_data;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::Duration;
use vike_secrets::credentials::Credentials;

use crate::signer::BybitV5Signer;
use vike_bridge_core::confirm::{DispatchLedger, VenueRestDispatch, confirm_closure};
use vike_bridge_core::exec_actor::submit_fence::{SubmitFence, VenueRestOrderSubmitFence};
use vike_bridge_core::exec_actor::{
    ConfirmFn, DEFAULT_FILL_SENTINEL, ExecutionClientActor, ExecutionClientActorCommand,
    forward_poll, run_loop,
};
use vike_bridge_core::leverage_read::{LeverageWriteError, write_demo_only};
use vike_bridge_core::per_symbol::{ClientFactory, NEGATIVE_TTL, OrderClientIdSymbols};
use vike_bridge_core::ratelimit::RateGate;
use vike_bridge_core::reconcile_admitted::ReconcileAdmittedSymbols;
use vike_bridge_core::rest::VenueRestRefusing;
use vike_bridge_core::signer::VenueClock;
use vike_bridge_core::transport::{ErrorKind, VenueApiError};
use vike_bridge_core::venue_adapter::await_properties;
use vike_bridge_core::websocket_session::user::{RESYNC_HISTORY_LIMIT, sleep_unless_stopped};
use vike_execution::{CoreGone, EventSender};
use vike_model::SymbolProperties;
use vike_model::events::Event;

use crate::execution::funding::BybitFundingPoller;
use crate::execution::rest::per_symbol::VenueRestPerSymbolBybit;
use crate::execution::rest::{
    BybitUrls, PATH_INSTRUMENTS, VenueRestBybit, parse_bybit_perp_instruments,
};
use crate::execution::user_data::spawn_bybit_perp_user_data_with_resync;
use crate::history::Category;
use crate::transport::client::rest_transport_on;
use vike_bridge_core::transport::json::{RestTransportJson, RestTransportJsonUreq};

/// FALLBACK cross-margin leverage the exec thread sets once at start — the literal this arm posted
/// to `/v5/position/set-leverage` unconditionally before the operator's budget was threaded in
/// (demo default; matches the smoke tests). Used ONLY when the operator expressed no leverage
/// intent, so a start with no `[risk] max_leverage` is byte-identical to every start before
/// [`leverage_for`] existed. NEVER read on the submit path — `VenueRestBybit::leverage` is consulted
/// by `set_leverage` and nothing else, which is why the read-only clients below (funding poller,
/// confirm re-query) can carry it inertly.
const DEFAULT_LEVERAGE: f64 = 2.0;
/// Funding-settlement poll cadence (REST `/v5/account/transaction-log`; funding is NOT on the
/// private WS — see `crate::execution::funding`'s module doc for the sign trap that rules that channel out).
/// Settlements post at most every 8h, so 60s keeps live equity current without hammering the
/// endpoint.
const FUNDING_POLL_INTERVAL: Duration = Duration::from_secs(60);

/// The account leverage this arm POSTs at startup, resolved from the operator's `[risk]` budget.
///
/// Pure, and `pub` on purpose — the SAME shape as the start's ceiling (decision 0095):
/// `crate::adapter`'s `VenueAdapterBybit::start` is handed the `ProfileRisk` and calls this ONCE per
/// start, then threads the resolved `f64` into [`spawn_with_recorder`]
/// (docs/decisions/0096 moved that call out of `vike_tradehub_core::make_engine`). Nothing inside the
/// adapter reaches for the profile, so the number the venue account is configured with and the
/// number the RiskGate sizes against (`ProfileRisk::initial_margin_requirement`, `initial_margin = 1.0 / max_leverage`)
/// can never disagree — that disagreement was the bug this exists to close.
///
/// Unset (`None`, or a profile that never mentions `max_leverage`) ⇒ [`DEFAULT_LEVERAGE`],
/// byte-identical to before. See `vike_bridge_core::leverage` for the full rule.
pub fn leverage_for(profile: Option<&vike_model::ProfileRisk>) -> f64 {
    vike_bridge_core::leverage::resolve_startup_leverage(profile, DEFAULT_LEVERAGE)
}

/// Fetch + map recent order/execution history to events (the audit-A3 replay body). Shared by the
/// pump's reconnect-resync AND the `run_loop` fill-sentinel; both feed the same dedup'd core ingest.
pub fn bybit_history_events<T: RestTransportJson>(
    rest: &VenueRestBybit<T>,
    symbol: &str,
) -> Vec<Event> {
    bybit_history_events_of(rest, symbol, &[])
}

/// [`bybit_history_events`] for the mounted `symbol` AND each of `foreign` (the engine's other symbols: the
/// admitted ones for the reconnect-resync, the ones a command touched for the gap-sentinel; decision 0174's
/// reopener). The mounted symbol is read FIRST, with the requests it always made. A foreign symbol is read
/// with the SAME two requests on its own name (bybit's engine spelling is the bare symbol, which is what the
/// rows carry), a repeat, an empty one and the mounted symbol are skipped, and the first answer that says
/// "slow down" ends the walk, so a reconnect of fifty accounts at once cannot turn into a ban: whatever is
/// left unread is reconcile's, as it was before this walk existed. A failed read costs its own rows only.
pub fn bybit_history_events_of<T: RestTransportJson>(
    rest: &VenueRestBybit<T>,
    symbol: &str,
    foreign: &[String],
) -> Vec<Event> {
    let limited = std::cell::Cell::new(false);
    let read = |symbol_name: &str| -> Vec<Event> {
        let rows = |r: Result<serde_json::Value, VenueApiError>| {
            r.unwrap_or_else(|e| {
                // Cloudflare answers a rate-limited IP with a 403 (`recon_client`'s reading of the same).
                if e.kind() == ErrorKind::RateLimited || e.code == 403 {
                    limited.set(true);
                }
                serde_json::json!([])
            })
        };
        let orders = rows(rest.get_order_history_for(symbol_name, RESYNC_HISTORY_LIMIT));
        let execs = rows(rest.get_execution_history_for(symbol_name, RESYNC_HISTORY_LIMIT));
        crate::execution::resync::map_bybit_history(&orders, &execs, "bybit", symbol_name)
    };
    let mut events = read(symbol);
    let mut seen: Vec<&str> = vec![symbol];
    for symbol_name in foreign {
        if limited.get() {
            tracing::warn!(
                target: "vike_bybit",
                "history replay: the venue said slow down; the admitted symbols after the last one read are left to the reconcile pass"
            );
            break;
        }
        if symbol_name.is_empty() || seen.iter().any(|s| s.eq_ignore_ascii_case(symbol_name)) {
            continue;
        }
        seen.push(symbol_name);
        events.extend(read(symbol_name));
    }
    events
}

// Wall-clock helpers: the shared `vike_model::{now_ms, now_ns}`. A signer stamps the start's `VenueClock`
// instead (owner ruling B-4, `rest_transport_on`).
use vike_model::{now_ms, now_ns};

/// Fetch `symbol`'s REAL tick/step/min-qty/min-notional grid from Bybit `instruments-info` (linear
/// category). `None` on any network/parse failure or unknown symbol: the caller has no grid, and there is
/// no fallback (owner, 2026-11-13). So the adapter formats orders on the correct grid for ANY symbol.
///
/// `rest_base` is the account's signed REST base the start resolved ONCE ([`BybitUrls::rest`],
/// decision 0095: `crate::adapter`'s `VenueAdapterBybit::start` looks it up at the tier
/// `vike-tradehub-core` resolves from the account row) — never re-read here, so the grid host can
/// never disagree with the exec host.
pub fn fetch_bybit_properties(
    creds: &Credentials,
    symbol: &str,
    rest_base: &str,
) -> Option<SymbolProperties> {
    fetch_bybit_properties_with_cap(creds, symbol, rest_base).map(|(properties, _cap)| properties)
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
    rest_base: &str,
) -> Option<(SymbolProperties, Option<f64>)> {
    fetch_bybit_instrument(creds, symbol, rest_base, &VenueClock::default())
        .ok()
        .map(|i| (i.properties, i.max_leverage))
}

/// The WHOLE `instruments-info` row for `symbol` — the grid, the leverage ceiling, and the venue's
/// own `contractType`. `Err` names what failed: the read (the venue's code and words) or a list with no
/// row for `symbol`.
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
///
/// `clock` is the start's signing clock (owner ruling B-4, [`rest_transport_on`]): the start's grid pre-fetch is
/// its first signed call, so it reads the venue's time for every later one.
pub fn fetch_bybit_instrument(
    creds: &Credentials,
    symbol: &str,
    rest_base: &str,
    clock: &VenueClock,
) -> Result<crate::execution::rest::BybitInstrument, String> {
    // Grid host is the exec path's own (`rest_base`), so a mainnet start sizes/prices on the mainnet
    // grid and a demo one on the demo grid.
    let info = rest_transport_on(clock)
        .signed(
            rest_base,
            PATH_INSTRUMENTS,
            "GET",
            &[("category", serde_json::json!("linear")), ("symbol", serde_json::json!(symbol))],
            &BybitV5Signer::new(creds, clock.ms()),
        )
        .map_err(|e| format!("{PATH_INSTRUMENTS} could not be read: code {} {}", e.code, e.msg))?;
    parse_bybit_perp_instruments(&info)
        .get(symbol)
        .cloned()
        .ok_or_else(|| format!("{PATH_INSTRUMENTS} lists no row for {symbol}"))
}

/// The venue's own `contractType` for the MOUNTED symbol: `None` when the exec plane signs that contract,
/// `Some(message)` when it does not.
///
/// The function keeps its name because the older records cite it. What it refuses has shrunk twice:
/// a DATED linear future is the linear wire (decision 0231), and a coin-margined perpetual or dated future
/// (`InversePerpetual`, `InverseFutures`) is MOUNTED since the bybit inverse amendment of decision 0258: its
/// clients sign in `inverse` ([`mount_money`] names the book and the settle coin), the adapter reports the
/// row's inverse contract value (one contract is one US dollar), the reconcile client reports the settle
/// coin's wallet and the funding poll reads the inverse log in that coin. What is left is a word this plane
/// signs no book for: an option, and any word the venue adds.
///
/// ⚠ `instruments-info?category=linear&symbol=<inverse>` ANSWERS with the real coin-settled row (MEASURED
/// 2026-09-16), so the row's word, never the request's, names the contract.
///
/// Pure: the caller supplies the word the venue said. An ABSENT `contractType` is NOT refused —
/// that is the shape of a row the venue changed or a fetch that failed, and refusing every order on
/// a missing field would take a working linear start off the venue for a parse miss. The fetch
/// failing already falls back to the caller's default grid, exactly as before (an INVERSE name with no
/// row is [`mount_money`]'s refusal).
#[must_use]
pub fn non_linear_perpetual_refusal(symbol: &str, contract_type: Option<&str>) -> Option<String> {
    match contract_type {
        None
        | Some("LinearPerpetual" | "LinearFutures" | "InversePerpetual" | "InverseFutures") => None,
        Some(other) => Some(format!(
            "bybit exec REFUSES {symbol:?}: the venue reports contractType {other:?}, which is not a \
             book this adapter signs (it signs linear and coin-margined perpetuals and dated futures, \
             and spot pairs as `PAIR.S`; an option needs a payoff and short-margin model the engine \
             does not have), so nothing is being sent. The market-data side of this instrument \
             charts and backfills."
        )),
    }
}

/// **What the MOUNTED symbol's money is**, from its own `instruments-info` row (decisions 0231 and 0258):
/// `None` for a linear contract, `Some((contract value, settle coin))` for a coin-margined one. One inverse
/// contract is ONE US dollar: the row carries no size field, and 8 contracts were 8 USD on the demo host
/// (decision 0231's capture), the unit the gatewayhub's symbol facts state too. The engine keeps a coin-margined
/// mount's books in that coin (decision 0258), so the coin is the row's `settleCoin`, never guessed.
///
/// # Errors
/// A coin-margined mount (the row's word says so, or the row says nothing and the kept list's row does,
/// `crate::history::Category::of_kept`) whose row could not be read or names no settle coin: the contract's
/// grid, kind and coin are the row's, and a guess (a guessed grid, a linear value) would size every order
/// wrong. A linear name with no row is `Ok(None)`: the start reports no grid, and the exec thread refuses its
/// orders until the row loads (no fallback grid, owner 2026-11-13).
pub fn mount_money(
    symbol: &str,
    row: Option<&crate::execution::rest::BybitInstrument>,
) -> Result<Option<(vike_model::ContractValue, String)>, String> {
    let inverse = match row.and_then(|r| r.contract_type.as_deref()) {
        Some(word) => {
            crate::instrument::class::category_of_contract_type(word) == Some(Category::Inverse)
        }
        None => Category::of_kept(symbol) == Some(Category::Inverse),
    };
    if !inverse {
        return Ok(None);
    }
    let contract = row.and_then(|r| r.contract.as_ref());
    let value = contract.and_then(|c| vike_model::ContractValue::of(c, 1.0));
    match (value, contract.and_then(vike_model::Contract::settle_asset)) {
        (Some(value), Some(coin)) if !coin.is_empty() => Ok(Some((value, coin.to_string()))),
        _ => Err(format!(
            "bybit: cannot mount {symbol:?}: it is coin-margined, and its instrument row {}, so its grid, \
             value and settle coin are unknown (a linear grid would size every order wrong); \
             nothing is sent",
            if row.is_some() {
                "names no coin-margined contract and settle coin"
            } else {
                "could not be read"
            }
        )),
    }
}

/// The book a MOUNTED symbol's clients that start before its grid row is read sign in (the confirm re-query, the
/// funding poll): `inverse` for a symbol whose KEPT row is coin-margined, `linear` for every other, as every one of
/// them did before an inverse mount existed. A mounted symbol has a kept row: the account's start refuses one with
/// none (`crate::adapter`). [`mount_money`] answers from the grid row for the clients built after it. The
/// private-socket decoder keeps the rows of this book (`crate::execution::user_data::perp_frame_decoder`).
pub(crate) fn named_mount_book(symbol: &str) -> Category {
    match Category::of_kept(symbol) {
        Some(Category::Inverse) => Category::Inverse,
        _ => Category::Linear,
    }
}

/// **One settle coin per inverse engine** (decision 0258 item 7, bybit's own twin of deribit's
/// `MoneyModel::admits`): on an engine MOUNTED on a coin-margined contract that keeps its books in `coin`, an
/// order on another symbol is signed only when that symbol is a coin-margined contract of the SAME coin. A
/// linear contract or a spot pair (`contract` not inverse) is the other money model (decision 0233 item 5), and
/// a contract of another coin is another venue wallet; both are refused in words before anything is sent.
/// `None` (no row could be read) is left to [`foreign_grid`], which refuses it in its own words.
fn coin_fence(
    symbol: &str,
    contract: Option<&vike_model::Contract>,
    mount: &str,
    coin: &str,
) -> Result<(), String> {
    let Some(contract) = contract else { return Ok(()) };
    if contract.margining() != Some(vike_model::Margining::Inverse) {
        return Err(format!(
            "bybit: {symbol:?} is not a coin-margined contract, but this engine is mounted on the coin-margined \
             {mount:?} and keeps its books in {coin}: an engine counts money one way, so the order was not \
             sent. Use an account mounted on a linear contract"
        ));
    }
    match contract.settle_asset() {
        Some(theirs) if theirs.eq_ignore_ascii_case(coin) => Ok(()),
        theirs => Err(format!(
            "bybit: {symbol:?} settles in {} but this engine, mounted on {mount:?}, keeps its books in {coin}, \
             and one engine holds one settle coin (the venue keeps one wallet per coin), so the order was not \
             sent. Use an account mounted on a contract of that coin",
            theirs.unwrap_or("an unnamed coin")
        )),
    }
}

/// One funding-poll iteration: pull new settlements and forward them to the core ingest. `Err`
/// only when the core is gone (the ingest receiver dropped) — the caller's loop then exits
/// instead of polling forever into a void, mirroring every other pump thread's self-exit.
///
/// A fetch failure is retried next cadence and warned ONCE per consecutive-failure streak
/// (`fail_streak` counts it; a success resets it) — a 60s background thread, never the hot fold,
/// so one warn per streak is both visible and bounded.
fn poll_funding_once<T: RestTransportJson>(
    poller: &mut BybitFundingPoller<'_, T>,
    events: &EventSender,
    fail_streak: &mut u32,
) -> Result<(), CoreGone> {
    if let Some(e) = forward_poll(poller.poll(), events, fail_streak)? {
        tracing::warn!(
            target: "vike_bybit",
            error = %e,
            "funding poll fetch failed; retrying each cadence (warns once per streak)"
        );
    }
    Ok(())
}

/// Spawn the funding-settlement background poll thread (attached via
/// [`vike_bridge_core::exec_actor::ExecutionClientActor::with_background`]): its OWN REST client (never
/// shares the submit/resync/sentinel clients on the command thread), polling
/// [`FUNDING_POLL_INTERVAL`] until `stop` fires or the core goes away. `properties` is unused by
/// the funding GET (no order formatting), so a default is fine.
///
/// The poller is floored at THIS spawn instant (`now_ms()`): settlements that posted before the
/// start are already inside the authoritative venue balance the account seeds from, so emitting
/// them would double-count — see `crate::execution::funding`'s module doc (the restart law).
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
    endpoints: BybitUrls,
    clock: VenueClock,
) -> (Arc<AtomicBool>, thread::JoinHandle<()>) {
    let stop = Arc::new(AtomicBool::new(false));
    let stop_thread = stop.clone();
    let join = thread::Builder::new()
        .name("bybit-funding-poll".to_string())
        .spawn(move || {
            let rest = VenueRestBybit {
                signer: BybitV5Signer::new(&creds, clock.ms()),
                transport: rest_transport_on(&clock),
                // The start's already-resolved hosts (decision 0095: `AdapterInputs::tier`, as
                // `VenueAdapterBybit::start` reads it), captured at spawn — this thread never
                // re-resolves them, so it can never poll a different network than the exec thread
                // trades on. The account's OWN funding log is a signed `trade` read at its type.
                base_url: endpoints.rest,
                mainnet: endpoints.mainnet,
                symbol: symbol.clone(),
                properties: SymbolProperties::default(),
                // INERT here: this client only GETs `/v5/account/transaction-log` and never calls
                // `set_leverage` (the field's sole reader), so the fallback is correct by
                // construction — the operator's value rides the exec thread's client below.
                leverage: DEFAULT_LEVERAGE,
                // The poller reads the account-wide settlement log of the MOUNT's book: linear, or for a
                // coin-margined mount the inverse log in its settle coin (`crate::execution::funding`; funding is not a
                // spot or a dated-contract cashflow).
                category: named_mount_book(&symbol),
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

/// The live Bybit exec client is the shared [`ExecutionClientActor`] itself, with this venue's hooks ([`over`]): `submit`/`cancel`
/// enqueue onto the REST thread (non-blocking) and every venue event returns through the core ingest. Dropping it
/// (or `detach`) stops the REST thread AND the user-data pump: the actor's own deterministic teardown. The HALT
/// sentinel it watches is handed in by [`ExecutionClientActor::with_halt_path`] (`AdapterInputs::process.halt_path`;
/// `crates/vike-ops/tests/settings_secrets/bridge_inputs_gate.rs` holds that every live start calls it).
///
/// Spawn the exec thread (which fetches the instrument grid, sets leverage, starts the user-data
/// pump, then drains commands). `symbol` is the V5 linear instrument (e.g. `"BTCUSDT"`); its real grid is
/// fetched at startup so any symbol formats correctly, and until it loads every order is refused in words
/// and the fetch asked again (`vike_bridge_core::venue_adapter::await_properties`): there is no fallback
/// grid (owner, 2026-11-13).
///
/// `endpoints` are the hosts the start resolved ONCE from the account's rows at its tier
/// (decision 0095: the tier chooses the network; [`crate::execution::rest::endpoints`]); a smoke/test caller
/// passes the demo ones explicitly, which no environment variable can silently upgrade.
pub fn spawn(
    creds: Credentials,
    symbol: String,
    events: EventSender,
    endpoints: BybitUrls,
) -> ExecutionClientActor {
    spawn_with_recorder(
        creds,
        symbol,
        events,
        None,
        None,
        None,
        endpoints,
        DEFAULT_LEVERAGE,
        false,
        Arc::default(),
        VenueClock::default(),
    )
}

/// Same as [`spawn`], plus an opt-in `properties_rec`: when present, the REAL fetched
/// instrument grid is recorded into the PIT properties store right after the
/// startup `instruments-info` fetch resolves. `properties_rec` is moved into the exec thread
/// (`PropertiesRecorder` is `Send + Sync`).
///
/// `on_reconcile` (reconciliation-activation Task 7) is threaded straight down to the venue's
/// `run_user_resync` call — `Some` fires the recon trigger after every reconnect's
/// event-replay settles; `None` (every non-recon caller, e.g. [`spawn`]) reproduces the
/// pre-Task-7 event-replay-only behavior byte-for-byte.
///
/// `broker_id` (unified cross-venue attribution, task 5) is the FD-broker code resolved ONCE at
/// the start site (`crate::adapter`'s `VenueAdapterBybit::start`, via `attribution_code_from` over
/// the credential map); threaded down to the ONE `VenueRestBybit`
/// signer actually used for `submit_order` (`run`'s `rest`), so every order-create request carries
/// `X-Referer: <id>`. `None` (every non-recon caller, e.g. [`spawn`]) is byte-identical.
///
/// `endpoints` (decision 0095: the tier chooses the network) are likewise resolved ONCE at the
/// start site and threaded down to EVERY host this client dials — the confirm re-query client,
/// the funding poller and the exec thread's three REST clients + user-data WS. No thread below
/// re-reads a row, so one start can never end up signing mainnet credentials against demo hosts.
///
/// `leverage` is the account leverage the exec thread posts once at start
/// (`POST /v5/position/set-leverage`, buy == sell), resolved ONCE at the start site from the
/// operator's `[risk]` budget via [`leverage_for`] — so the venue account and the RiskGate's
/// margin math run on the SAME number. Pass [`DEFAULT_LEVERAGE`] (what [`spawn`] does)
/// for the historical literal.
///
/// `fast_execution` is the `venue.bybit.fast_exec` setting, read once by the start through
/// [`crate::execution::user_data::fast_execution`] and handed to the private-socket pump. [`spawn`]
/// passes `false`.
///
/// `admitted` is the engine's admitted-symbol list: `crate::adapter` passes the SAME `Arc` it hands the
/// reconcile client, which is the one told each admission, and the reconnect-resync replays the history
/// of each symbol on it ([`bybit_history_events_of`]). [`spawn`] passes an empty list, the
/// mounted-only replay.
///
/// `clock` is the start's ONE signing clock (owner ruling B-4, [`rest_transport_on`]): every client this spawns
/// (the confirm, the funding poll, the exec thread's own and the foreign-symbol ones) and the private socket's
/// login sign on it. `crate::adapter` passes the clock its grid pre-fetch and reconcile client sign on; [`spawn`]
/// a fresh one.
#[expect(clippy::too_many_arguments)]
pub fn spawn_with_recorder(
    creds: Credentials,
    symbol: String,
    events: EventSender,
    properties_rec: Option<Arc<vike_data::PropertiesRecorder>>,
    on_reconcile: Option<mpsc::Sender<()>>,
    broker_id: Option<String>,
    endpoints: BybitUrls,
    leverage: f64,
    fast_execution: bool,
    admitted: Arc<ReconcileAdmittedSymbols>,
    clock: VenueClock,
) -> ExecutionClientActor {
    // Active-confirm re-query (audit ex1 residual), in the ONE shared form of
    // `vike_bridge_core::confirm` (the form decision 0171 describes): the ExecutionClientActor runs the closure on its OWN worker
    // thread when the core issues `Command::ConfirmOrder`, so a wedged adapter is prodded with NO
    // network on the core fold. ONE REST client serves every confirm (bounded, backed off per order client id),
    // over the exact ambiguous-submit re-query. `ledger` is stamped by `VenueRestDispatch` around the
    // command loop's client in `run`: "absent" rejects only an order client id whose submit has FINISHED, and a
    // failed query says nothing. The status GET is keyed on orderLinkId only, so it carries no grid.
    // The GET also names the order's SYMBOL: the one client is a `VenueRestPerSymbolBybit`, so an order client id the
    // command loop recorded as a foreign order (`order_client_ids`, shared with the exec thread's router)
    // is asked about on ITS symbol, any other order client id on the mounted one. A foreign-symbol client
    // here reads no grid (a status GET never formats a price), so building one costs no network.
    let ledger = Arc::new(DispatchLedger::default());
    let order_client_ids = Arc::new(OrderClientIdSymbols::default());
    let adopt_order_client_ids = order_client_ids.clone();
    // ⚠ The status GET names the order's BOOK too (decision 0231): a re-query in the wrong category reads
    // an empty list, and "absent on both endpoints" is a terminal `OrderRejected` of a live order. So the
    // book is the NAME's ([`light_category`]) and a name that cannot be read is a client that cannot be
    // built (the confirm then says nothing, as for any failed query), never a default.
    let (confirm_base, confirm_mainnet) = (endpoints.rest.clone(), endpoints.mainnet);
    let confirm_clock = clock.clone();
    let confirm_rest =
        move |creds: &Credentials, symbol: String, category: Category| VenueRestBybit {
            signer: BybitV5Signer::new(creds, confirm_clock.ms()),
            transport: rest_transport_on(&confirm_clock),
            // The start's resolved host, captured — never re-read on this worker.
            base_url: confirm_base.clone(),
            mainnet: confirm_mainnet,
            symbol: crate::instrument::class::split_spot(&symbol).0.to_string(),
            // A status re-query formats no price and no size.
            properties: SymbolProperties::default(),
            // INERT here: a status re-query never calls `set_leverage` (the field's sole
            // reader), so the default is correct by construction.
            leverage: DEFAULT_LEVERAGE,
            category,
        };
    let (foreign_credentials, foreign_rest) = (creds.clone(), confirm_rest.clone());
    let listings = endpoints.listings.clone();
    let make: ClientFactory<VenueRestBybit<RestTransportJsonUreq>> = Box::new(move |label| {
        let label = label.to_uppercase();
        let category = light_category(&listings, &label)?;
        Ok(foreign_rest(&foreign_credentials, label, category))
    });
    let confirm: ConfirmFn = confirm_closure(
        VenueRestPerSymbolBybit::new(
            symbol.clone(),
            confirm_rest(&creds, symbol.clone(), named_mount_book(&symbol)),
            make,
            order_client_ids.clone(),
        ),
        ledger.clone(),
        |_| true,
    );
    // Funding-settlement poll: perp-only by construction (this crate's ONLY exec client is the
    // linear-perp one — spot is deferred, see the crate doc), so spawning it here is inherently
    // scoped to perp starts.
    let (funding_stop, funding_join) = spawn_funding_poll(
        creds.clone(),
        symbol.clone(),
        events.clone(),
        endpoints.clone(),
        clock.clone(),
    );
    let actor = ExecutionClientActor::spawn("bybit-exec", events.clone(), move |rx| {
        run(
            creds,
            symbol,
            events,
            rx,
            properties_rec,
            on_reconcile,
            broker_id,
            endpoints,
            leverage,
            fast_execution,
            ledger,
            order_client_ids,
            admitted,
            clock,
        )
    })
    .with_confirm(confirm)
    .with_background(funding_stop, funding_join);
    over(actor, adopt_order_client_ids)
}

/// The client over its command `actor`: the shared actor itself, declaring what its `run_loop` reads beyond the
/// default, the trigger amend ([`ExecutionClientActor::with_trigger_amend`]; without it every `modify_trigger` is refused in
/// the trait's words before the channel), and teaching the router the symbol each restored order rests on (the core
/// announces them at boot and after a gatewayhub restart; [`ExecutionClientActor::with_adopt_restored`]): a cancel of an order
/// this process never submitted would otherwise go to the MOUNTED symbol, where the venue answers "unknown order"
/// and the adapter reports a LIVE order cancelled.
fn over(
    actor: ExecutionClientActor,
    order_client_ids: Arc<OrderClientIdSymbols>,
) -> ExecutionClientActor {
    actor
        .with_trigger_amend()
        .with_adopt_restored(Arc::new(move |orders| order_client_ids.adopt(orders)))
}

/// The exec thread's SUBMIT client — the one [`VenueRestBybit`] that places, cancels, amends and
/// sets leverage (`run_loop`'s `rest`), and so the ONE bybit transport on the exec side that rides
/// the order-rate gate ([`crate::transport::ratelimit::rest_rate_gate`], the `BYBIT.orders` row: 18 per 1 s,
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
///   (`crate::reconcile::recon_client`). They are not merged on purpose: a reconcile burst
///   queued ahead of a cancel in one FIFO window is the order-path delay the exempt `signed_requery`
///   split exists to prevent.
/// - What this does NOT do is aggregate across starts: the gate is per start (per symbol), so two
///   starts on one account each have their own 18/s against the venue's one per-UID 20/s.
///
/// Unified cross-venue attribution, task 5: this is the ONE `VenueRestBybit` used for
/// `submit_order`/`cancel_order`/`modify_order`/`set_leverage`, so `broker_id` (the resolved
/// FD-broker code) rides its SIGNER and every order-related REST request from the exec thread
/// stamps `X-Referer`. `None` (unconfigured) is byte-identical to before this field existed.
/// `leverage` is the mount-resolved operator leverage ([`leverage_for`], clamped) — this is the ONE
/// client that posts it (`set_leverage` right after this call); unset profile ⇒ [`DEFAULT_LEVERAGE`],
/// unchanged. `category` is the MOUNT's book ([`mount_money`]: `inverse` for a coin-margined mount, else
/// `linear`).
fn order_rest(
    creds: &Credentials,
    endpoints: &BybitUrls,
    symbol: &str,
    properties: SymbolProperties,
    leverage: f64,
    broker_id: Option<String>,
    category: Category,
    clock: &VenueClock,
) -> VenueRestBybit<RestTransportJsonUreq> {
    order_rest_gated(
        creds,
        endpoints,
        symbol,
        properties,
        leverage,
        broker_id,
        category,
        crate::transport::ratelimit::rest_rate_gate(),
        clock,
    )
}

/// The book a client built WITHOUT a grid fetch signs in (a cancel, a status query, a replay): its KEPT row's
/// (`crate::history::Category::of_kept`), else the venue's own listings ([`crate::instrument::listing::perp_book_for`],
/// three keyless reads once per process at `listings`, [`BybitUrls::listings`]), else an `Err` in words. ⚠ NEVER a
/// default to `linear`: a cancel sent in the wrong book answers `110001` ("order not exists"), which the router
/// swallows as "already gone", so a LIVE order would be reported cancelled while it rests.
fn light_category(listings: &str, symbol: &str) -> Result<Category, String> {
    use crate::instrument::listing::PerpListing;
    if let Some(category) = Category::of_kept(symbol) {
        return Ok(category);
    }
    match crate::instrument::listing::perp_book_for(listings, symbol) {
        Ok(PerpListing::Linear) => Ok(Category::Linear),
        Ok(PerpListing::Inverse) => Ok(Category::Inverse),
        Ok(other) => Err(format!(
            "bybit: cannot tell which book {symbol:?} trades in (the venue lists it as {other:?}); nothing was sent"
        )),
        Err(why) => Err(format!(
            "bybit: cannot tell which book {symbol:?} trades in ({why}); nothing was sent"
        )),
    }
}

/// [`order_rest`] on a GIVEN order-rate window. The mounted client and every foreign-symbol client of
/// one exec thread are built on clones of ONE gate: the venue's 20/s budget is per UID, so N symbols
/// must share the 18/s window, not each take their own. `symbol` is the ENGINE's (a spot pair is
/// `DOGEUSDT.S`); the client holds the venue's name of it, and `category` is its one book. `clock` is the one it signs
/// on ([`rest_transport_on`]).
fn order_rest_gated(
    creds: &Credentials,
    endpoints: &BybitUrls,
    symbol: &str,
    properties: SymbolProperties,
    leverage: f64,
    broker_id: Option<String>,
    category: Category,
    gate: RateGate,
    clock: &VenueClock,
) -> VenueRestBybit<RestTransportJsonUreq> {
    VenueRestBybit {
        signer: BybitV5Signer::new(creds, clock.ms()).with_broker_id(broker_id),
        transport: rest_transport_on(clock).with_rate_gate(gate),
        base_url: endpoints.rest.clone(),
        mainnet: endpoints.mainnet,
        symbol: crate::instrument::class::split_spot(symbol).0.to_string(),
        properties,
        leverage,
        category,
    }
}

/// **The one write of the account's leverage for a symbol the engine was NOT mounted on** (the
/// gatewayhub lane's per-symbol hand-over, `VenueAdapter::set_symbol_leverage`): [`VenueRestBybit::set_leverage`]
/// itself, the very verb the exec thread posts at its start (110043 "not modified" swallowed there),
/// on a client of its own because the exec thread's foreign-symbol clients post none on purpose
/// (`foreign_rest_factory`). `leverage` is written as given: the lane's probe refused a figure above
/// the symbol's maximum, so nothing is clamped here. A SPOT pair (`PAIR.S`) holds no leverage and is refused, as
/// binance and okx refuse theirs (owner ruling L-D2; the exec thread's own start still skips one in silence). `Err`
/// is the venue's own words, and says whether the outcome is unknown ([`LeverageWriteError`]).
///
/// ⚠ **DEMO only, guarded HERE**: a `mainnet` account ([`BybitUrls::mainnet`], its type) is refused before any
/// client exists (owner ruling 2026-10-26: no exchange write on a live account; `vike_secrets::LEVERAGE_VENUES`
/// opens bybit at `demo` alone today, and opening `live` in that table must not by itself open a per-symbol
/// mainnet write).
pub fn write_symbol_leverage(
    creds: &Credentials,
    endpoints: &BybitUrls,
    symbol: &str,
    leverage: f64,
    broker_id: Option<String>,
) -> Result<(), LeverageWriteError> {
    write_demo_only(crate::execution::rest::VENUE, endpoints.mainnet, symbol)?;
    if crate::instrument::class::split_spot(&symbol.to_uppercase()).1 {
        return Err(LeverageWriteError::Refused(format!(
            "bybit: {symbol} is a spot pair, which holds no leverage; nothing was sent"
        )));
    }
    post_symbol_leverage(&symbol_leverage_rest(creds, endpoints, symbol, leverage, broker_id))?;
    tracing::info!(target: "vike_bybit", %symbol, leverage, "set-leverage POSTED for a symbol admitted on demand (the gatewayhub reads it back)");
    Ok(())
}

/// The client [`write_symbol_leverage`] posts through: the order client's own build ([`order_rest`], on
/// the order-rate window and with the broker code), pointed at the admitted symbol.
fn symbol_leverage_rest(
    creds: &Credentials,
    endpoints: &BybitUrls,
    symbol: &str,
    leverage: f64,
    broker_id: Option<String>,
) -> VenueRestBybit<RestTransportJsonUreq> {
    let symbol = symbol.to_uppercase();
    // A leverage write or a mode read is an ACCOUNT setting of the symbol, never an order: a name the kept list
    // carries no row of falls to `linear`, where the venue answers a symbol of another book with its own parameter
    // error (nothing is traded either way).
    let category = Category::of_kept(&symbol).unwrap_or(Category::Linear);
    // A one-shot client: it reads the venue's clock for itself (owner ruling B-4).
    order_rest_gated(
        creds,
        endpoints,
        &symbol,
        SymbolProperties::default(),
        leverage,
        broker_id,
        category,
        crate::transport::ratelimit::rest_rate_gate(),
        &VenueClock::default(),
    )
}

/// **The account's modes put right for a symbol BEFORE the gatewayhub's read-only leverage probe**
/// (`VenueAdapter::heal_modes`): [`crate::account::modes::Modes::ensure`] itself, the rule `run_loop` applies to the
/// mounted symbol at the start (the symbol's position mode in one-way, an isolated-margin account in cross;
/// read first, written only on a mismatch and only while the target is flat and empty; DEMO account only, a
/// mainnet account is never read or written), on a one-shot client of its own. The probe then reads the mode
/// the engine models instead of the one the start was about to fix. `Err` is the words of a symbol the
/// switch is blocked on (what holds it); the lane goes on to the probe, which refuses the figure under its
/// own reason.
pub fn heal_symbol_modes(
    creds: &Credentials,
    endpoints: &BybitUrls,
    symbol: &str,
    broker_id: Option<String>,
) -> Result<(), String> {
    // The ENGINE's spelling goes to `ensure` (a spot pair's `.S` is how it knows there is no mode to read).
    let engine_symbol = symbol.to_uppercase();
    let VenueRestBybit { signer, transport, base_url, mainnet, .. } =
        symbol_leverage_rest(creds, endpoints, symbol, DEFAULT_LEVERAGE, broker_id);
    crate::account::modes::Modes::new(signer, transport, base_url, mainnet).ensure(&engine_symbol)
}

/// [`write_symbol_leverage`]'s post and its error words, over any transport. A timeout, a 5xx or an
/// unreadable answer (`transport::error_codes::submit_outcome_unknown`) is [`LeverageWriteError::Ambiguous`]: the POST may
/// have landed, so the lane reads the venue back instead of reporting a refusal.
fn post_symbol_leverage<T: RestTransportJson>(
    rest: &VenueRestBybit<T>,
) -> Result<(), LeverageWriteError> {
    rest.set_leverage().map_err(|e| {
        let ambiguous = crate::transport::error_codes::submit_outcome_unknown(e.code);
        LeverageWriteError::of_set_leverage(&rest.symbol, ambiguous, e)
    })
}

/// The grid an order on a symbol OTHER than the mounted one may be signed on, from the venue's own
/// `instruments-info` row for it: `Err(reason)` (shown to the operator in the terminal
/// `OrderRejected`) when the fetch failed or the symbol is unknown (`None`: never a guessed or
/// BTC-shaped fallback grid), or when the venue's word is not a book this plane signs
/// ([`non_linear_perpetual_refusal`], the same refusal the mounted symbol gets).
fn foreign_grid(
    symbol: &str,
    row: Option<crate::execution::rest::BybitInstrument>,
) -> Result<(SymbolProperties, Category), String> {
    let Some(row) = row else {
        return Err(format!(
            "bybit: cannot sign {symbol:?}: no derivative instrument grid for it \
             (unknown symbol, or instruments-info was unreachable); the order was not sent"
        ));
    };
    // A row that names no contract type is not shown to be any book: for a FOREIGN symbol (no
    // mounted-start default to keep working) that is a refusal, consistent with a failed fetch.
    let Some(word) = row.contract_type.as_deref() else {
        return Err(format!(
            "bybit: cannot sign {symbol:?}: instruments-info did not say its contract type, so its \
             book is not known; the order was not sent"
        ));
    };
    // The venue's own word names the book (decision 0231): a linear perpetual or a dated linear future trades
    // in `linear`, a coin-margined perpetual or dated future in `inverse`. Any other word is a book this plane
    // does not sign.
    match crate::instrument::class::category_of_contract_type(word) {
        Some(book) => Ok((row.properties, book)),
        None => Err(non_linear_perpetual_refusal(symbol, Some(word)).unwrap_or_else(|| {
            format!("bybit: cannot sign {symbol:?}: contract type {word:?} is not a book this adapter signs")
        })),
    }
}

/// The grid of a SPOT pair from its `instruments-info` row (`category=spot`), or why it cannot be traded: no row
/// (unknown pair, unreachable list) or a pair the venue does not list as `Trading`. The grid is the spot row's own
/// (`basePrecision` as the quantity step, `minOrderAmt` as the minimum notional: `catalog::spot_properties`).
fn spot_grid(symbol: &str, row: Option<&serde_json::Value>) -> Result<SymbolProperties, String> {
    let Some(row) = row else {
        return Err(format!(
            "bybit: cannot sign the spot pair {symbol:?}: no spot instrument grid for it (unknown pair, or \
             instruments-info was unreachable); the order was not sent"
        ));
    };
    let status = row.get("status").and_then(serde_json::Value::as_str).unwrap_or("");
    if status != "Trading" {
        return Err(format!(
            "bybit: the spot pair {symbol:?} is not trading (status {status:?}); the order was not sent"
        ));
    }
    Ok(crate::catalog::spot_properties(row))
}

/// The venue's `instruments-info` row of one SPOT pair (`category=spot`), through the same signed GET the
/// derivative grid fetch uses; `None` on a failure or an unknown pair.
fn fetch_bybit_spot_row(
    creds: &Credentials,
    symbol: &str,
    rest_base: &str,
    clock: &VenueClock,
) -> Option<serde_json::Value> {
    let info = rest_transport_on(clock)
        .signed(
            rest_base,
            PATH_INSTRUMENTS,
            "GET",
            &[("category", serde_json::json!("spot")), ("symbol", serde_json::json!(symbol))],
            &BybitV5Signer::new(creds, clock.ms()),
        )
        .ok()?;
    info.get("result")?.get("list")?.as_array()?.first().cloned()
}

/// The [`VenueRestPerSymbolBybit`] factory: a signing client for another symbol of the SAME account (same
/// keys, host, leverage value, broker id and order-rate window as the mounted client), on that
/// symbol's own fetched grid. The grid fetch is one blocking `instruments-info` GET on the exec
/// thread, bounded by the transport's timeout. `leverage` is carried but NOT posted for the new
/// symbol BY THIS CLIENT: the venue keeps that symbol's current leverage unless the gatewayhub lane
/// writes it through [`write_symbol_leverage`] (above), which it does only after a probe that refuses
/// an open position held at another leverage.
///
/// `inverse_mount` is `(mounted symbol, settle coin)` of an engine mounted on a coin-margined contract
/// ([`mount_money`]); such an engine signs only coin-margined contracts of its own coin ([`coin_fence`]).
/// `None` (a linear mount) changes nothing.
fn foreign_rest_factory(
    creds: Credentials,
    endpoints: BybitUrls,
    leverage: f64,
    broker_id: Option<String>,
    gate: RateGate,
    inverse_mount: Option<(String, String)>,
    clock: VenueClock,
) -> ClientFactory<VenueRestBybit<RestTransportJsonUreq>> {
    Box::new(move |symbol| {
        // instruments-info rows are keyed by the upper-case symbol
        let symbol = symbol.to_uppercase();
        let fence = |contract: Option<&vike_model::Contract>| match &inverse_mount {
            Some((mount, coin)) => coin_fence(&symbol, contract, mount, coin),
            None => Ok(()),
        };
        // A SPOT pair (`DOGEUSDT.S`) is read in the spot book; every other name is a derivative whose row's
        // `contractType` says its book (decision 0231).
        let (properties, category) = match crate::instrument::class::split_spot(&symbol) {
            (pair, true) => {
                fence(Some(&vike_model::Contract::CryptoSpot))?;
                let row = fetch_bybit_spot_row(&creds, pair, &endpoints.rest, &clock);
                (spot_grid(pair, row.as_ref())?, Category::Spot)
            }
            (_, false) => {
                let row = fetch_bybit_instrument(&creds, &symbol, &endpoints.rest, &clock).ok();
                fence(row.as_ref().and_then(|r| r.contract.as_ref()))?;
                foreign_grid(&symbol, row)?
            }
        };
        // ⚠ NO `set_leverage` here: this factory also runs after a restart, when the account may hold
        // a position on this symbol at a leverage set on the venue; posting the mounted symbol's
        // configured value would move its liquidation price.
        Ok(order_rest_gated(
            &creds,
            &endpoints,
            &symbol,
            properties,
            leverage,
            broker_id.clone(),
            category,
            gate.clone(),
            &clock,
        ))
    })
}

/// **A client for a CANCEL or a status query of any symbol**, built with no network and no grid (a symbol and
/// an order client id are all they send; the grid and leverage are never read): the command loop's light factory, and the
/// router's whole factory while the mounted row has not loaded ([`run`]'s wait). Its rate window is `gate`.
fn light_factory(
    creds: &Credentials,
    endpoints: &BybitUrls,
    broker_id: Option<String>,
    gate: RateGate,
    clock: &VenueClock,
) -> ClientFactory<VenueRestBybit<RestTransportJsonUreq>> {
    let (creds, endpoints, clock) = (creds.clone(), endpoints.clone(), clock.clone());
    Box::new(move |symbol| {
        let symbol = symbol.to_uppercase();
        // The book is the NAME's, else the venue's listings, else no client at all ([`light_category`]):
        // a cancel in the wrong book would be swallowed as "already gone" while the order rests.
        let category = light_category(&endpoints.listings, &symbol)?;
        Ok(order_rest_gated(
            &creds,
            &endpoints,
            &symbol,
            // A cancel and a status query format no price and no size.
            SymbolProperties::default(),
            DEFAULT_LEVERAGE,
            broker_id.clone(),
            category,
            gate.clone(),
            &clock,
        ))
    })
}

/// The exec thread: build the REST client, put the mounted symbol in the account modes the adapter orders
/// in ([`crate::account::modes`]), set leverage, start the private-WS pump, then drain commands until `Shutdown`. ALL network I/O (leverage, submit/cancel, WS) lives HERE — off the
/// single-writer core thread. On `Shutdown` the pump is stopped + joined before returning.
#[expect(clippy::too_many_arguments)]
fn run(
    creds: Credentials,
    symbol: String,
    events: EventSender,
    rx: Receiver<ExecutionClientActorCommand>,
    properties_rec: Option<Arc<vike_data::PropertiesRecorder>>,
    on_reconcile: Option<mpsc::Sender<()>>,
    broker_id: Option<String>,
    endpoints: BybitUrls,
    leverage: f64,
    fast_execution: bool,
    ledger: Arc<DispatchLedger>,
    order_client_ids: Arc<OrderClientIdSymbols>,
    admitted: Arc<ReconcileAdmittedSymbols>,
    clock: VenueClock,
) {
    // Fetch the venue's REAL instrument grid for `symbol` (off the core thread): until it loads every
    // command is refused in words and the fetch asked again, so orders format on the right tick/step or
    // not at all (no fallback grid, owner 2026-11-13). Recorded into the PIT properties store below.
    // The SAME response also carries `leverageFilter.maxLeverage` — this symbol's published
    // account-leverage ceiling. Captured here (free: one request, two fields) and applied to the
    // startup `set_leverage` below. A row without one yields `None`, i.e. no cap and no clamp.
    // Meanwhile a cancel is SENT (it needs no grid) through the command loop's client built with none: the
    // mounted symbol in the book its NAME says, as the confirm's, and any other through the light factory.
    let grid_less = || {
        let book = named_mount_book(&symbol);
        let mounted = order_rest(
            &creds,
            &endpoints,
            &symbol,
            SymbolProperties::default(),
            DEFAULT_LEVERAGE,
            broker_id.clone(),
            book,
            &clock,
        );
        let gate = crate::transport::ratelimit::rest_rate_gate();
        let make = light_factory(&creds, &endpoints, broker_id.clone(), gate, &clock);
        VenueRestPerSymbolBybit::new(symbol.clone(), mounted, make, Arc::clone(&order_client_ids))
    };
    let Some(row) =
        await_properties("bybit", &symbol, &events, &rx, NEGATIVE_TTL, grid_less, || {
            fetch_bybit_instrument(&creds, &symbol, &endpoints.rest, &clock)
        })
    else {
        return;
    };
    // ⚠ **THE EXEC REFUSAL, on the venue's own word and before anything is built.** The fetch above
    // is the SAME request this arm always made; its `contractType` names what the mount is. A contract
    // this plane signs no book for (an option), and a coin-margined mount whose row names no contract or
    // settle coin, stop here: no REST client, no `set_leverage` POST, no private WS, no signed request of any kind —
    // just a loop that rejects every command with the reason.
    let money = match non_linear_perpetual_refusal(&symbol, row.contract_type.as_deref()) {
        Some(why) => Err(why),
        None => mount_money(&symbol, Some(&row)),
    };
    let inverse_mount = match money {
        Ok(money) => money.map(|(_, coin)| (symbol.clone(), coin)),
        Err(why) => {
            tracing::error!(target: "vike_bybit", %symbol, "{why}");
            run_loop(&VenueRestRefusing { why }, &events, rx, Vec::new, DEFAULT_FILL_SENTINEL);
            return;
        }
    };
    // Every client of the mounted symbol signs in its ONE book (decision 0231): `inverse` for a coin-margined
    // mount (decision 0258's bybit amendment), `linear` for every other, as before.
    let book = if inverse_mount.is_some() { Category::Inverse } else { Category::Linear };
    let (properties, venue_leverage_cap) = (row.properties, row.max_leverage);
    vike_data::PropertiesRecorder::record_opt(
        &properties_rec,
        "bybit",
        &symbol,
        properties,
        now_ns(),
    );
    // Ceiling: the start resolved the operator's REQUEST, this is the venue's own limit for this
    // symbol. `min(request, cap)` when a cap is known; UNKNOWN (no `leverageFilter`)
    // ⇒ the requested value rides through untouched, byte-identical to before this
    // clamp existed. `vike_bridge_core::leverage`'s module doc is the authority, including the
    // residual it leaves open (the RiskGate still sizes against the un-clamped request).
    let leverage = vike_bridge_core::leverage::clamp_to_venue_cap(
        leverage,
        venue_leverage_cap,
        crate::execution::rest::VENUE,
        &symbol,
    );
    // The host pair for this start's PRIVATE exec + user-data WS. Decision 0095: the tier alone
    // decides (`AdapterInputs::tier`, as `VenueAdapterBybit::start` reads it), resolved ONCE at the
    // start and threaded in — this thread never re-resolves it. All REST clients here + the WS pump
    // bind to it.
    let rest = order_rest(
        &creds,
        &endpoints,
        &symbol,
        properties,
        leverage,
        broker_id.clone(),
        book,
        &clock,
    );
    // ONE order-rate window for the mounted client and every foreign-symbol client built later: the
    // foreign ones are handed the mounted client's own gate. (`order_rest` always gates; the
    // fallback is unreachable, there only so a missing gate cannot panic an exec thread.)
    let gate = rest
        .transport
        .rate_gate()
        .cloned()
        .unwrap_or_else(crate::transport::ratelimit::rest_rate_gate);
    // The account-mode rule (`crate::account::modes`), DEMO account only (a mainnet account is never read or written for
    // a mode, owner ruling 2026-10-26): the mounted symbol is put in one-way position mode (and an
    // isolated-margin account in cross) BEFORE leverage is posted and before the first order, and every
    // symbol admitted later passes the same check at its first order (`with_mode_check` below). A symbol
    // that cannot be put right is refused in words at its orders; here it is only logged, because the
    // start must still stand (orders on other symbols, cancels, HALT).
    let modes = Arc::new(crate::account::modes::Modes::new(
        BybitV5Signer::new(&creds, clock.ms()),
        rest_transport_on(&clock).with_rate_gate(gate.clone()),
        endpoints.rest.clone(),
        endpoints.mainnet,
    ));
    if let Err(why) = modes.ensure(&symbol) {
        tracing::error!(target: "vike_bybit", %symbol, "{why}");
    }
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
    // A foreign order's CANCEL and status take a client with no grid fetch (a symbol and an order client id are all
    // they send; the grid and leverage are never read), so they do not wait for `instruments-info`.
    let light = light_factory(&creds, &endpoints, broker_id.clone(), gate.clone(), &clock);
    // The gap-sentinel below reads the foreign symbols the router was asked about (`OrderClientIdSymbols::touch`).
    let sentinel_order_client_ids = Arc::clone(&order_client_ids);
    let routed = VenueRestPerSymbolBybit::new(
        symbol.clone(),
        rest,
        foreign_rest_factory(
            creds.clone(),
            endpoints.clone(),
            leverage,
            broker_id,
            gate,
            inverse_mount,
            clock.clone(),
        ),
        order_client_ids,
    )
    .with_light_factory(light)
    .with_mode_check(Box::new(move |symbol| modes.ensure(symbol)));
    // Audit A3: a SEPARATE REST client for the resync supervisor — the submit client above lives on
    // THIS thread and can't be shared with the pump's supervisor thread. After every private-WS
    // reconnect the supervisor replays recent order/execution history through this client, so a
    // fill/cancel/reject that landed during the reconnect gap is recovered (the core dedups the
    // overlap) instead of leaving a phantom-live order.
    let resync_rest = VenueRestBybit {
        signer: BybitV5Signer::new(&creds, clock.ms()),
        transport: rest_transport_on(&clock),
        base_url: endpoints.rest.clone(),
        mainnet: endpoints.mainnet,
        symbol: symbol.clone(),
        properties,
        // Same mount-resolved value as the submit client; history-only, so it never posts it.
        leverage,
        category: book,
    };
    // A THIRD client for the run_loop fill-sentinel (off the pump thread, so its history calls never
    // contend with the pump's own resync).
    let sentinel_rest = VenueRestBybit {
        signer: BybitV5Signer::new(&creds, clock.ms()),
        transport: rest_transport_on(&clock),
        base_url: endpoints.rest.clone(),
        mainnet: endpoints.mainnet,
        symbol: symbol.clone(),
        properties,
        // Same mount-resolved value as the submit client; history-only, so it never posts it.
        leverage,
        category: book,
    };
    let resync_symbol = symbol.clone();
    let sentinel_symbol = symbol.clone();
    // The fence both threads below share: the pump's fills for an order wait behind that order's own
    // `OrderSubmitted`/`OrderAccepted` (`vike_bridge_core::exec_actor::submit_fence`'s module doc has the
    // race: the `execution` topic can beat both the `order` topic's `New` and the REST answer).
    let fence = Arc::new(SubmitFence::new(events.clone()));
    // Private user-data WS (+ resync): authoritative fills/cancels push into the core ingest through the fence.
    let feed = spawn_bybit_perp_user_data_with_resync(
        endpoints.ws,
        creds.api_key.expose().clone(),
        creds.api_secret.expose().clone(),
        symbol,
        events.clone(),
        Arc::clone(&fence),
        // After a reconnect: the mounted symbol, then EVERY symbol the engine admitted (`admitted` is the
        // list the reconcile client is told, read at the reconnect so a symbol admitted since the start
        // is on it).
        move || {
            let foreign = admitted.symbols();
            let events = bybit_history_events_of(&resync_rest, &resync_symbol, &foreign);
            if !foreign.is_empty() {
                tracing::info!(
                    target: "vike_bybit", symbols = foreign.len(), events = events.len(),
                    "reconnect resync: replayed the history of the admitted symbols after the mounted one"
                );
            }
            events
        },
        on_reconcile,
        fast_execution,
        clock,
    );
    // The gap-sentinel replays recent history `DEFAULT_FILL_SENTINEL` after the last command, to
    // recover a fill/cancel the WS lost (the first-connect subscribe race); the core dedups the
    // overlap, so a normal WS delivery makes it a no-op. A venue-free mechanism default.
    // The ledger stamps every dispatched submit, so the active confirm trusts "absent" only for one.
    run_loop(
        &VenueRestOrderSubmitFence::new(&VenueRestDispatch { rest: routed, ledger }, &fence),
        &events,
        rx,
        // Five seconds after the last command: the mounted symbol, then the foreign symbols the commands
        // since the last look named (not every admitted symbol: a sentinel per command over sixteen
        // symbols would be a burst of requests for nothing).
        || {
            bybit_history_events_of(
                &sentinel_rest,
                &sentinel_symbol,
                &sentinel_order_client_ids.take_touched(),
            )
        },
        DEFAULT_FILL_SENTINEL,
    );
    // Shutdown reached: stop + join the pump + resync supervisor (deterministic teardown).
    let _ = feed.shutdown();
}

#[path = "execution_tests.rs"]
#[cfg(test)]
mod execution_tests;
