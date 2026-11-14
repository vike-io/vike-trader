//! OKX live exec half — `ExecutionClient` over the V5 SWAP-perp REST + the private user-data WS.
//!
//! The client IS the shared [`ExecutionClientActor`], returned by [`spawn_with_recorder`] with this venue's hooks (no wrapper
//! of its own): `submit`/`cancel` enqueue onto ONE dedicated OS thread
//! that drives [`VenueRestOkx`] (blocking REST off the single-writer core). `submit_order` emits
//! `[OrderSubmitted, OrderAccepted|OrderRejected]` synchronously (routing `order_type="stop"` to the
//! algo endpoint); the authoritative async fills/cancels return on the private WS via
//! [`crate::execution::user_data::spawn_okx_perp_user_data_with_resync`] straight into the core ingest.
//!
//! OKX-specifics vs Bybit: the perp is priced in CONTRACTS (`ct_val` base per contract — carried in
//! both the REST client and the WS pump), demo trading is the same REST host + the
//! `x-simulated-trading: 1` header (`rest_transport(true)`), and the signer needs the account
//! passphrase. Every address is the start's [`OkxNet`], resolved once from the account's `venue_url`
//! rows at its type; nothing here names a host.
//!
//! ONE ACCOUNT, MANY INSTRUMENTS: the command loop's client is a [`VenueRestPerSymbol`] (see
//! `vike_bridge_core::per_symbol`'s and [`crate::execution::rest::per_symbol`]'s module docs): an order, cancel, amend or status query is signed on the
//! ORDER's own `instId` with that instrument's grid and `ctVal`, and the private WS converts a fill
//! with the `ctVal` of the instrument it happened on. Only SWAP instruments are signable; the
//! mounted instrument's path is unchanged.
//!
//! LIVE GATE: absent credentials never reach here — the start only spawns this when
//! `vike_secrets::credentials::api_credentials` yields `Some` for the started account
//! (absent-creds-is-the-live-gate). An account with no complete key trio does not run.
//!
//! v1 scope: `submit`/`cancel` only (the DOM surface). `modify`/batch fall through to the
//! [`vike_execution::ExecutionClient`] defaults — a native amend/batch is a follow-up.

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

use crate::signer::OkxV5Signer;
use vike_bridge_core::confirm::{DispatchLedger, VenueRestDispatch, confirm_closure};
use vike_bridge_core::exec_actor::submit_fence::{SubmitFence, VenueRestOrderSubmitFence};
use vike_bridge_core::exec_actor::{
    ConfirmFn, DEFAULT_FILL_SENTINEL, ExecutionClientActor, ExecutionClientActorCommand,
    forward_poll, run_loop,
};
use vike_bridge_core::leverage_read::{LeverageWriteError, write_demo_only};
use vike_bridge_core::per_symbol::{
    ClientFactory, NEGATIVE_TTL, OrderClientIdSymbols, VenueRestPerSymbol,
};
use vike_bridge_core::reconcile_admitted::ReconcileAdmittedSymbols;
use vike_bridge_core::rest::VenueRestRefusing;
use vike_bridge_core::signer::VenueClock;
use vike_bridge_core::transport::json::RestTransportJson;
use vike_bridge_core::transport::{ErrorKind, VenueApiError};
use vike_bridge_core::venue_adapter::await_properties;
use vike_bridge_core::websocket_session::user::{RESYNC_HISTORY_LIMIT, sleep_unless_stopped};
use vike_execution::{CoreGone, EventSender};
use vike_model::SymbolProperties;
use vike_model::events::Event;
use vike_secrets::credentials::Credentials;

use crate::execution::funding::OkxFundingPoller;
use crate::execution::rest::per_symbol::{ContractSizes, refused_no_client};
use crate::execution::rest::{
    OkxInstrument, OkxNet, PATH_INSTRUMENTS, VenueRestOkx, parse_okx_perp_instruments, simulated,
};
use crate::execution::user_data::{
    spawn_okx_algo_user_data_with_resync, spawn_okx_perp_user_data_with_resync,
};
use crate::instrument::InstrumentType;
use crate::transport::client::rest_transport_with_timeout;
use crate::transport::client::unwrap_okx;
use crate::transport::client::{rest_transport, rest_transport_on};
use vike_bridge_core::transport::json::RestTransportJsonUreq;

/// FALLBACK cross-margin leverage the exec thread sets once at start — the literal this arm posted
/// to `/api/v5/account/set-leverage` unconditionally before the operator's budget was threaded in
/// (demo default; matches the smoke tests). Used ONLY when the operator expressed no leverage
/// intent, so a start with no `[risk] max_leverage` is byte-identical to every start before
/// [`leverage_for`] existed. NEVER read on the submit path — `VenueRestOkx::leverage` is consulted
/// by `set_leverage` and nothing else, which is why the read-only clients below (funding poller,
/// confirm re-query) can carry it inertly.
const DEFAULT_LEVERAGE: f64 = 2.0;
/// Funding-bill poll cadence (REST `/api/v5/account/bills`; funding is NOT on any OKX WS channel —
/// see `crate::execution::funding`'s module doc). Settlements post at most every 8h, so 60s keeps live equity
/// current without hammering the endpoint.
const FUNDING_POLL_INTERVAL: Duration = Duration::from_secs(60);

/// The account leverage this arm POSTs at startup, resolved from the operator's `[risk]` budget.
///
/// Pure, and `pub` on purpose — the SAME shape as the start's ceiling (decision 0095):
/// `crate::adapter`'s `VenueAdapterOkx::start`, handed the `ProfileRisk` in its request, calls this
/// ONCE per start, then threads the resolved `f64` into
/// [`spawn_with_recorder`]. Nothing inside the adapter
/// reaches for the profile, so the number the venue account is configured with and the number the
/// RiskGate sizes against (`ProfileRisk::initial_margin_requirement`, `initial_margin = 1.0 / max_leverage`) can never
/// disagree — that disagreement was the bug this exists to close.
///
/// Unset (`None`, or a profile that never mentions `max_leverage`) ⇒ [`DEFAULT_LEVERAGE`],
/// byte-identical to before. See `vike_bridge_core::leverage` for the full rule.
pub fn leverage_for(profile: Option<&vike_model::ProfileRisk>) -> f64 {
    vike_bridge_core::leverage::resolve_startup_leverage(profile, DEFAULT_LEVERAGE)
}

/// How far apart two foreign instruments' reads of one replay start: OKX meters `fills-history` at
/// ten requests per two seconds per account, and a replay over the engine's sixteen extra instruments is
/// sixteen of them. The wait sits BETWEEN two foreign instruments (the first one follows the mounted
/// instrument's reads at once), and a replay is a background thread's.
const FOREIGN_READ_PACE: Duration = Duration::from_millis(250);

/// Fetch + map recent order/fill history to events (the audit-A3 replay body). Shared by the pump's
/// reconnect-resync AND the `run_loop` fill-sentinel; both feed the same dedup'd core ingest. It reads
/// the mounted `symbol` AND each of `foreign` (the engine's other instruments: the admitted ones for
/// the reconnect-resync, the ones a command touched for the gap-sentinel; decision 0174's reopener). The mounted instrument is read FIRST, with the requests it always made. A foreign
/// instrument is read with the SAME two requests on its own `instId`, its fills sized by ITS OWN contract
/// value (`contract_of`: a contract of another instrument is another amount of the base asset, so an instrument
/// whose size cannot be read is skipped with a warning, never sized with a stand-in) and carrying the
/// scoped trade id the user-data stream spells for it (`INSTID:tradeId`,
/// [`crate::execution::resync::scope_fill_ids`]), so a fill the stream already folded is a KNOWN id. A repeat, an
/// empty one and the mounted instrument are skipped, reads are `pace` apart, and the first answer that
/// says "slow down" ends the walk: whatever is left unread is reconcile's, as it was before this walk.
/// A failed read costs its own rows only.
pub fn okx_history_events_of<T: RestTransportJson>(
    rest: &VenueRestOkx<T>,
    symbol: &str,
    ct_val: f64,
    foreign: &[String],
    contract_of: &dyn Fn(&str) -> Option<f64>,
    pace: Duration,
) -> Vec<Event> {
    let limited = std::cell::Cell::new(false);
    let rows = |r: Result<serde_json::Value, VenueApiError>| {
        r.unwrap_or_else(|e| {
            if e.kind() == ErrorKind::RateLimited {
                limited.set(true);
            }
            serde_json::json!([])
        })
    };
    let mut events = {
        let orders = rows(rest.get_orders_history(RESYNC_HISTORY_LIMIT));
        let fills = rows(rest.get_fills_history(RESYNC_HISTORY_LIMIT));
        crate::execution::resync::map_okx_history(&orders, &fills, "okx", symbol, ct_val)
    };
    let mut seen: Vec<&str> = vec![symbol];
    let mut first = true;
    for instrument_value in foreign {
        if limited.get() {
            tracing::warn!(
                target: "vike_okx",
                "history replay: the venue said slow down; the admitted instruments after the last one read are left to the reconcile pass"
            );
            break;
        }
        if instrument_value.is_empty()
            || seen.iter().any(|s| s.eq_ignore_ascii_case(instrument_value))
        {
            continue;
        }
        seen.push(instrument_value);
        let Some(contract) = contract_of(instrument_value) else {
            tracing::warn!(
                target: "vike_okx", instrument = %instrument_value,
                "history replay: the contract value of this admitted instrument cannot be read; its \
                 history is left to the reconcile pass"
            );
            continue;
        };
        if !std::mem::replace(&mut first, false) {
            std::thread::sleep(pace);
        }
        let orders = rows(rest.get_orders_history_for(instrument_value, RESYNC_HISTORY_LIMIT));
        let fills = rows(rest.get_fills_history_for(instrument_value, RESYNC_HISTORY_LIMIT));
        let fills = crate::execution::resync::scope_fill_ids(&fills, instrument_value);
        events.extend(crate::execution::resync::map_okx_history(
            &orders,
            &fills,
            "okx",
            instrument_value,
            contract,
        ));
    }
    events
}

/// The ALGO socket's reconnect-resync body: the newest stops `canceled` or `order_failed` (the venue
/// wants one `state` per read) mapped to their terminal events, so a stop that ended while that socket
/// was down does not stay `ACCEPTED`. `effective` stops are not read: their child order's fills replay
/// through [`okx_history_events_of`] under the stop's own id.
///
/// The mounted instrument AND each of `foreign` (the admitted instruments): a stop on an admitted
/// instrument that ended while the ALGO socket was down is replayed too. Same walk as
/// [`okx_history_events_of`]: the mounted instrument's two reads first and unchanged,
/// each foreign instrument's two reads on its own `instId`, `pace` apart, ended by the first rate-limit
/// answer.
pub fn okx_algo_history_events_of<T: RestTransportJson>(
    rest: &VenueRestOkx<T>,
    foreign: &[String],
    pace: Duration,
) -> Vec<Event> {
    let limited = std::cell::Cell::new(false);
    let read = |instrument: &str| -> Vec<Event> {
        ["canceled", "order_failed"]
            .into_iter()
            .flat_map(|state| {
                let rows = rest
                    .get_algo_history_for(instrument, state, RESYNC_HISTORY_LIMIT)
                    .unwrap_or_else(|e| {
                        if e.kind() == ErrorKind::RateLimited {
                            limited.set(true);
                        }
                        serde_json::json!([])
                    });
                crate::execution::resync::map_okx_algo_history(&rows)
            })
            .collect()
    };
    let mut events = read(&rest.symbol);
    let mut seen: Vec<&str> = vec![rest.symbol.as_str()];
    let mut first = true;
    for instrument in foreign {
        if limited.get() {
            tracing::warn!(
                target: "vike_okx",
                "history replay: the venue said slow down; the admitted instruments after the last one read are left to the reconcile pass"
            );
            break;
        }
        if instrument.is_empty() || seen.iter().any(|s| s.eq_ignore_ascii_case(instrument)) {
            continue;
        }
        seen.push(instrument);
        if !std::mem::replace(&mut first, false) {
            std::thread::sleep(pace);
        }
        events.extend(read(instrument));
    }
    events
}

// Wall-clock helpers: the shared `vike_model::{now_ms, now_ns}`. A signer stamps the start's `VenueClock`
// instead (owner ruling B-4, `crate::transport::client::rest_transport_on`).
use vike_model::{now_ms, now_ns};

/// Fetch `symbol`'s REAL tick/lot grid + contract value (`ct_val`) from OKX public `instruments`
/// (SWAP). `None` on any network/parse failure or unknown symbol: there is no fallback (owner,
/// 2026-11-13). Lets the adapter size + price any SWAP inst correctly. Public (unsigned).
///
/// `net` is the account's addresses and type the start threaded in (decision 0095:
/// `crate::adapter`'s `VenueAdapterOkx::start` resolves it ONCE, from the account's `AdapterInputs::tier`)
/// — never re-read here, so the grid universe can never disagree with the exec path's.
pub fn fetch_okx_instrument(symbol: &str, net: &OkxNet) -> Option<(SymbolProperties, f64)> {
    fetch_okx_instrument_row(symbol, net).ok().map(|i| (i.properties, i.ct_val))
}

/// [`fetch_okx_instrument`]'s WHOLE row for `symbol`: the grid, `ctVal`, the venue's published
/// account-leverage CEILING (the instrument's `lever` field; `None` when the instrument carried no
/// usable one, OKX omits it on SPOT/OPTION) and the row's `ctType` word — the SAME single
/// `public/instruments` request, since all of them ride the same row. `Err` names what failed: the read
/// or a list with no SWAP row for `symbol`.
///
/// This exists rather than widening [`fetch_okx_instrument`]'s tuple so every existing caller
/// (`crate::adapter`'s `RiskLimits` + `ct_val` pre-fetch, the live smokes) is untouched, and so
/// neither the cap nor the contract type costs a second round-trip anywhere. Consumed by [`run`]:
/// the cap via `vike_bridge_core::leverage::clamp_to_venue_cap`, the type by
/// [`inverse_swap_refusal`].
pub fn fetch_okx_instrument_row(symbol: &str, net: &OkxNet) -> Result<OkxInstrument, String> {
    // Grid host is shared, but the demo-vs-mainnet instrument universe is selected by the
    // `x-simulated-trading` header — follow the same switch the exec path uses so a mainnet start
    // sizes/prices on the mainnet grid; demo keeps the header (byte-identical).
    fetch_row_on(&rest_transport(simulated(net.mainnet)), &net.rest, symbol)
}

/// How long a foreign instrument's row read may hold the thread it runs on. That thread is the exec
/// thread (the [`VenueRestPerSymbol`] factory: nothing queued behind it is placed or cancelled meanwhile)
/// or the user-data pump (a fill of an instrument with no client): the order agent's 30 s is far
/// too long to stall either.
const FOREIGN_FETCH_TIMEOUT: Duration = Duration::from_secs(5);

/// [`fetch_okx_instrument_row`] for an instrument the exec thread was NOT started on, on a transport
/// that gives up after [`FOREIGN_FETCH_TIMEOUT`]. The mounted instrument's start-up fetch keeps the
/// ordinary agent.
fn fetch_foreign_row(symbol: &str, net: &OkxNet) -> Option<OkxInstrument> {
    let transport = rest_transport_with_timeout(simulated(net.mainnet), FOREIGN_FETCH_TIMEOUT);
    // The class is the instId's own (a spot pair, a dated future or a swap): the row is asked for under
    // that `instType`, and `foreign_rest` holds the row's own word against it. An id of no known shape
    // asks for a SWAP, as every id did.
    fetch_row_of(&transport, &net.rest, symbol, InstrumentType::of_inst_id_or_swap(symbol)).ok()
}

/// The one `public/instruments` SWAP read of `symbol` on `transport`; `Err` on any network/parse
/// failure or an id the venue does not list as a SWAP. The MOUNTED instrument's read: this adapter's engine
/// is mounted on a SWAP, and a spot id there is never signed (its orders are refused while no row loads).
fn fetch_row_on<T: RestTransportJson>(
    transport: &T,
    base: &str,
    symbol: &str,
) -> Result<OkxInstrument, String> {
    fetch_row_of(transport, base, symbol, InstrumentType::Swap)
}

/// [`fetch_row_on`] under the `instType` of `class`.
fn fetch_row_of<T: RestTransportJson>(
    transport: &T,
    base: &str,
    symbol: &str,
    class: InstrumentType,
) -> Result<OkxInstrument, String> {
    let unread = |e: VenueApiError| {
        format!("{PATH_INSTRUMENTS} could not be read: code {} {}", e.code, e.msg)
    };
    let info = transport
        .public(
            base,
            PATH_INSTRUMENTS,
            &[("instType", class.inst_type().into()), ("instId", symbol.into())],
        )
        .map_err(unread)?;
    let data = unwrap_okx(info).map(|d| serde_json::json!({ "data": d })).map_err(unread)?;
    parse_okx_perp_instruments(&data).get(symbol).cloned().ok_or_else(|| {
        format!("{PATH_INSTRUMENTS} lists no {} row for {symbol}", class.inst_type())
    })
}

/// **The exec REFUSAL for a coin-margined (inverse) swap**, on the venue's own word (`ctType`)
/// and before anything is built (decision 0170, plan S0.1).
///
/// Since decision 0258's okx amendment it is the LINEAR engine's refusal: an engine MOUNTED on a
/// coin-margined contract is [`mount_money`]'s (its quantity is the contract count and its ledger is the
/// coin), and what such an engine may sign besides its mount is `coin_fence`'s. A linear mount still
/// refuses an inverse instrument here (its foreign clients, its fill sizing, its reconcile client), for the
/// reason below.
///
/// An inverse swap (`BTC-USD-SWAP`) counts a contract in USD — `ctVal` is 100 and `ctValCcy` is
/// `USD` — and this adapter's whole order path reads a quantity as BASE and divides it by `ctVal` in
/// [`VenueRestOkx::to_contracts`]: a 0.5 BTC order would become 0.005 contracts, floor to zero, and
/// the grid `properties_in_base` hands the gate is 100x too coarse. Nothing refused it; the order
/// formatted on a real grid, for the right instrument, in the wrong unit. A twin of bybit's
/// `non_linear_perpetual_refusal`, and for the same reason it is a refusal and not a fix: the
/// decision supplies the vocabulary (`ContractValue`), not the permission to trade a new class of
/// contract.
///
/// Pure: the caller supplies the row the fetch returned. **A row that could not be fetched is
/// REFUSED too (fail closed)**: with no row, `ctType` is unknown, and a `BTC-USD-SWAP` engine would be
/// sized in the wrong unit. An ABSENT `ctType` on a row that WAS fetched is NOT refused: that is a row the
/// venue changed, and refusing on a missing field would take a working linear start off the venue for a
/// parse miss. The exec thread asks only once a row loaded: until then it refuses every order in words and
/// fetches again (`run`).
#[must_use]
pub fn inverse_swap_refusal(symbol: &str, row: Option<&OkxInstrument>) -> Option<String> {
    let Some(row) = row else {
        return Some(format!(
            "okx exec REFUSES {symbol:?}: the instrument row could not be fetched, so its ctType is \
             unknown and an inverse swap (a contract counted in USD) would be sized in the wrong \
             unit — nothing is being sent until the row can be read"
        ));
    };
    match row.contract_type.as_deref() {
        None | Some("linear") => None,
        Some(other) => Some(format!(
            "okx exec REFUSES {symbol:?}: the venue reports ctType {other:?}, a contract counted in \
             the QUOTE currency (`ctVal` is USD on an inverse swap) and settled in the COIN (MEASURED on \
             the demo account, 2026-10-30: a BTC-USD-SWAP round trip paid its fee in BTC and moved the \
             account's BTC balance while its USDT balance did not move at all). This engine's ledger \
             and the balance the reconcile pass seeds it from are USDT, so the PnL of such a contract \
             would never reach the engine's equity — nothing is being sent. The market-data side of \
             this instrument works. Trading it needs an engine whose ledger is that coin (decision \
             0170's inverse engine, mounted on the inverse contract), not an admission into a USDT one."
        )),
    }
}

/// **What the MOUNTED instrument's money is**, from its own `public/instruments` row (decisions 0170 and
/// 0258): `None` for a linear contract, `Some((contract value, settle coin))` for a coin-margined one. One
/// inverse contract is worth `ctVal x ctMult` US dollars (100 on `BTC-USD-SWAP`): the engine's quantity is
/// that CONTRACT count, passed to the wire as is (the contracts→quantity factor is 1.0, as a spot pair's),
/// and the engine keeps its books in the row's `settleCcy`, never a guessed coin.
///
/// # Errors
/// A coin-margined mount (the row's `ctType` says so, or the row says nothing and the name's shape does,
/// `vike_catalog::is_inverse_contract`) whose row could not be read, names no settle coin or no usable
/// contract value: its grid, kind and coin are the row's, and a guess (a guessed grid, a linear value)
/// would size every order wrong. A linear name with no row is `Ok(None)` here: the start reports no grid,
/// and the exec thread refuses its orders until the row loads. A LINEAR row whose
/// `ctMult` is not 1: the linear path sizes a contract by `ctVal` alone, so it does not mount.
pub fn mount_money(
    symbol: &str,
    row: Option<&OkxInstrument>,
) -> Result<Option<(vike_model::ContractValue, String)>, String> {
    let inverse = match row.and_then(|r| r.contract_type.as_deref()) {
        Some(word) => word == "inverse",
        None => vike_catalog::is_inverse_contract(crate::execution::rest::VENUE, symbol),
    };
    if !inverse {
        // A linear mount is sized by `ctVal` alone (`to_contracts`, `properties_in_base`, the reconcile
        // factor), so a contract worth `ctVal x ctMult` would trade the multiplier's times the size it says.
        // Refused, as `foreign_rest` refuses one besides the mount; no listed USDT swap states one today.
        if let Some(row) = row.filter(|r| r.multiplied()) {
            return Err(format!(
                "okx exec REFUSES {symbol:?}: its contract multiplier ctMult is {} and this adapter sizes \
                 a linear contract by ctVal alone, so every order would be the multiplier's times its size; \
                 nothing is being sent",
                row.ct_mult
            ));
        }
        return Ok(None);
    }
    let Some(row) = row else {
        return Err(format!(
            "okx exec REFUSES {symbol:?}: it is a coin-margined contract and its instrument row could not \
             be fetched, so its contract value and settle coin are unknown; nothing is being sent until \
             the row can be read"
        ));
    };
    let coin = row.settle_ccy.clone().ok_or_else(|| {
        format!("okx exec REFUSES {symbol:?}: its coin-margined row names no settle currency (settleCcy)")
    })?;
    // an absent `ctMult` (0.0) is the usual 1
    let mult = if row.ct_mult.is_finite() && row.ct_mult > 0.0 { row.ct_mult } else { 1.0 };
    let unit = row.ct_val * mult;
    if !(unit.is_finite() && unit > 0.0) {
        return Err(format!(
            "okx exec REFUSES {symbol:?}: its coin-margined row has no usable contract value (ctVal {})",
            row.ct_val
        ));
    }
    Ok(Some((vike_model::ContractValue::inverse(unit), coin)))
}

/// **One settle coin per engine, at the bridge** (decision 0258, item 7): an engine MOUNTED on a coin-margined
/// contract keeps its books in `coin`, so an instrument it signs or sizes besides the mounted one must be a
/// coin-margined contract settled in that coin (`BTC-USD-261225` on a `BTC-USD-SWAP` engine). Another coin's
/// contract, a linear contract and a spot pair are refused in words. The door refuses the same
/// (`settle_coin_refusal`, `contract_kind_refusal` in the tradehub); this holds it for an order the door never
/// saw. A linear mount never calls it: its inverse refusal is [`inverse_swap_refusal`].
pub(crate) fn coin_fence(instrument: &str, row: &OkxInstrument, coin: &str) -> Result<(), String> {
    let settles = row.settle_ccy.as_deref().unwrap_or("no stated currency");
    if row.contract_type.as_deref() == Some("inverse") && settles.eq_ignore_ascii_case(coin) {
        return Ok(());
    }
    Err(format!(
        "okx: cannot sign {instrument:?} on this engine: it is mounted on a coin-margined contract and keeps its \
         books in {coin}, and {instrument} is {} settled in {settles}; one engine holds one settle coin of one \
         kind (decision 0258). Trade it from an account mounted on it; the order was not sent",
        match row.contract_type.as_deref() {
            Some(word) => format!("a {word} contract"),
            None => "not a contract".to_string(),
        }
    ))
}

/// One funding-poll iteration: pull new bills and forward them to the core ingest. `Err` only when
/// the core is gone (the ingest receiver dropped) — the caller's loop then exits instead of polling
/// forever into a void, mirroring every other pump thread's self-exit.
///
/// A fetch failure is retried next cadence and warned ONCE per consecutive-failure streak
/// (`fail_streak` counts it; a success resets it) — a 60s background thread, never the hot fold,
/// so one warn per streak is both visible and bounded.
fn poll_funding_once<T: RestTransportJson>(
    poller: &mut OkxFundingPoller<'_, T>,
    events: &EventSender,
    fail_streak: &mut u32,
) -> Result<(), CoreGone> {
    if let Some(e) = forward_poll(poller.poll(), events, fail_streak)? {
        tracing::warn!(
            target: "vike_okx",
            error = %e,
            "funding poll fetch failed; retrying each cadence (warns once per streak)"
        );
    }
    Ok(())
}

/// Spawn the funding-bill background poll thread (attached via
/// [`vike_bridge_core::exec_actor::ExecutionClientActor::with_background`]): its OWN REST client (never
/// shares the submit/resync/sentinel clients on the command thread), polling
/// [`FUNDING_POLL_INTERVAL`] until `stop` fires or the core goes away. `properties`/`ct_val` are
/// unused by the bills GET (a symbol-scoped read, no order formatting), so defaults are fine.
///
/// The poller is floored at THIS spawn instant (`now_ms()`): bills that posted before the start
/// are already inside the authoritative venue balance the account seeds from, so emitting them
/// would double-count — see `crate::execution::funding`'s module doc (the restart law).
///
/// `admitted` is the engine's admitted-instrument list, the SAME `Arc` the reconcile client is told
/// each admission through (`crate::adapter`'s `VenueAdapterOkx::start`): empty, the query names the
/// mounted `instId` only; non-empty, it reads the account's SWAP bills once and folds the mounted and
/// the admitted instruments (`crate::execution::funding`'s module doc).
///
/// CROSS-MOUNT HAZARD (for a future multi-mount-per-venue-account world): the bills query is
/// `instId`-scoped (or, with admitted instruments, narrowed to this engine's instruments), so
/// distinct symbols on one account do NOT collide — but TWO starts of the SAME instId sharing an
/// account would each emit that symbol's bills → duplication through the non-idempotent
/// `apply_funding`. Whoever adds multi-mount must keep this lane one-poller-per-(account, instId).
fn spawn_funding_poll(
    creds: Credentials,
    symbol: String,
    events: EventSender,
    net: &OkxNet,
    admitted: Arc<ReconcileAdmittedSymbols>,
    clock: VenueClock,
) -> (Arc<AtomicBool>, thread::JoinHandle<()>) {
    let stop = Arc::new(AtomicBool::new(false));
    let stop_thread = stop.clone();
    // The account's OWN funding bills are a signed read of its own book: the `trade` REST base at its
    // type, the exec thread's (never a `data` row, which a demo account would read on mainnet).
    let (sim, base_url) = (simulated(net.mainnet), net.rest.clone());
    let join = thread::Builder::new()
        .name("okx-funding-poll".to_string())
        .spawn(move || {
            let rest = VenueRestOkx {
                signer: OkxV5Signer::new(&creds, clock.ms()),
                // The start's already-resolved type (decision 0095: the account's
                // `AdapterInputs::tier`, as `crate::adapter` resolves it), captured at spawn: live
                // drops the `x-simulated-trading` header; demo keeps it. This thread never
                // re-resolves it, so it can never poll a different network than the exec thread
                // trades on.
                transport: rest_transport_on(sim, &clock),
                base_url,
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
            let mut poller =
                OkxFundingPoller::new(&rest, &symbol, now_ms()).with_admitted(admitted);
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

/// The live OKX exec client is the shared [`ExecutionClientActor`] itself, with this venue's hooks ([`over`]): `submit`/`cancel`
/// enqueue onto the REST thread (non-blocking) and every venue event returns through the core ingest. Dropping it
/// (or `detach`) stops the REST thread AND both user-data pumps: the actor's own deterministic teardown. The HALT
/// sentinel it watches is handed in by [`ExecutionClientActor::with_halt_path`] (`AdapterInputs::process.halt_path`;
/// `crates/vike-ops/tests/settings_secrets/bridge_inputs_gate.rs` holds that every live start calls it).
///
/// Spawn the exec thread (fetches the instrument grid, sets leverage, starts the user-data pump,
/// then drains commands). `symbol` is the SWAP instId (e.g. `"BTC-USDT-SWAP"`); its real grid + contract
/// value are fetched at startup, and until they load every order is refused in words and the fetch asked
/// again (`vike_bridge_core::venue_adapter::await_properties`): no fallback grid and no guessed `ctVal`
/// (owner, 2026-11-13).
///
/// `net` is the account's addresses and type, resolved once ([`OkxNet::resolve`], decision 0095:
/// the start's tier chooses the network); a demo `net` keeps the `x-simulated-trading` header and
/// the demo sockets — what a smoke/test caller hands it (`crate::test_support::demo_net`), which no
/// environment variable can silently upgrade.
pub fn spawn(
    creds: Credentials,
    symbol: String,
    events: EventSender,
    net: OkxNet,
) -> ExecutionClientActor {
    spawn_with_recorder(
        creds,
        symbol,
        events,
        None,
        None,
        None,
        net,
        DEFAULT_LEVERAGE,
        Arc::default(),
        VenueClock::default(),
    )
}

/// Same as [`spawn`], plus an opt-in `properties_rec`: when present, the REAL fetched
/// instrument grid is recorded into the PIT properties store right after the
/// startup `instruments` fetch resolves. `properties_rec` is moved into the exec thread
/// (`PropertiesRecorder` is `Send + Sync`).
///
/// `on_reconcile` (reconciliation-activation Task 7) is threaded straight down to the venue's
/// `run_user_resync` call — `Some` fires the recon trigger after every reconnect's
/// event-replay settles; `None` (every non-recon caller, e.g. [`spawn`]) reproduces the
/// pre-Task-7 event-replay-only behavior byte-for-byte.
///
/// `broker_code` (unified cross-venue attribution, task 4) is the OKX FD-broker code resolved
/// ONCE by the caller (`crate::adapter`'s `VenueAdapterOkx::start`, via `attribution_code_from`)
/// and stamped as the wire `tag` on every order this start submits; `None` (unconfigured, or
/// every non-mount caller e.g. [`spawn`]) reproduces the pre-task-4 wire body
/// byte-for-byte (no `tag` key at all).
///
/// `net` (decision 0095: the start's tier chooses the network) is likewise resolved ONCE at the
/// start site ([`OkxNet::resolve`], the account's `venue_url` rows at its type) and threaded down to
/// EVERY network selection this client makes — the confirm re-query client, the funding poller,
/// and the exec thread's REST clients + both private socket URLs. No thread below re-reads
/// anything, so one start can never end up signing mainnet credentials against the
/// simulated-trading header. A demo `net` is byte-identical to before this switch existed.
///
/// `leverage` is the account leverage the exec thread posts once at start
/// (`POST /api/v5/account/set-leverage`, `mgnMode: "cross"`), resolved ONCE at the start site
/// from the operator's `[risk]` budget via [`leverage_for`] — so the venue account and the
/// RiskGate's margin math run on the SAME number. Pass [`DEFAULT_LEVERAGE`] (what
/// [`spawn`] does) for the historical literal.
///
/// `admitted` is the engine's admitted-instrument list the funding poll narrows its account-wide
/// bills read to (`crate::execution::funding`'s module doc). `crate::adapter` passes the SAME `Arc` it hands the
/// reconcile client, which is the one told each admission; [`spawn`] passes an empty list, the
/// mounted-only poll.
///
/// `clock` is the start's ONE signing clock (owner ruling B-4, `crate::transport::client::rest_transport_on`): every
/// client this spawns (the confirm, the funding poll, the exec thread's own and the foreign-instrument ones) and
/// both private sockets' logins sign on it. `crate::adapter` passes the clock its reconcile client signs on;
/// [`spawn`] a fresh one.
pub fn spawn_with_recorder(
    creds: Credentials,
    symbol: String,
    events: EventSender,
    properties_rec: Option<Arc<vike_data::PropertiesRecorder>>,
    on_reconcile: Option<mpsc::Sender<()>>,
    broker_code: Option<String>,
    net: OkxNet,
    leverage: f64,
    admitted: Arc<ReconcileAdmittedSymbols>,
    clock: VenueClock,
) -> ExecutionClientActor {
    // Active-confirm re-query (audit ex1 residual), in the ONE shared form of
    // `vike_bridge_core::confirm` (the form decision 0171 describes): the ExecutionClientActor runs the closure on its OWN worker
    // thread when the core issues `Command::ConfirmOrder`, so a wedged adapter is prodded with NO
    // network on the core fold. ONE REST client serves every confirm (bounded, backed off per order client id),
    // over the exact ambiguous-submit re-query. `ledger` is stamped by `VenueRestDispatch` around the
    // command loop's client in `run`: "absent" rejects only an order client id whose submit has FINISHED, and a
    // failed query says nothing. The status GET is keyed on clOrdId only, so it carries no grid and
    // no contract size (the funding poll's inert values; the GET reads neither). The GET also names the
    // order's INSTRUMENT: an order client id the command loop recorded as a foreign order (`order_client_ids`, shared with
    // the exec thread's router) is asked about on ITS instId (a `VenueRestPerSymbol`), any other order client id on
    // the mounted one, as before.
    let ledger = Arc::new(DispatchLedger::default());
    let order_client_ids = Arc::new(OrderClientIdSymbols::default());
    let adopt_order_client_ids = order_client_ids.clone();
    let confirm: ConfirmFn = {
        // The start's resolved type and REST base, captured — never re-read on this worker. Live
        // drops the sim header; demo keeps it, byte-identical.
        let (sim, base_url) = (simulated(net.mainnet), net.rest.clone());
        let status_rest = |symbol: String| VenueRestOkx {
            signer: OkxV5Signer::new(&creds, clock.ms()),
            transport: rest_transport_on(sim, &clock),
            base_url: base_url.clone(),
            symbol,
            properties: SymbolProperties::default(),
            ct_val: 0.0,
            // INERT here: a status re-query never calls `set_leverage` (the field's sole
            // reader), so the default is correct by construction.
            leverage: DEFAULT_LEVERAGE,
            broker_code: None, // a status re-query never builds an order body
        };
        let mounted = status_rest(symbol.clone());
        let make: ClientFactory<VenueRestOkx<RestTransportJsonUreq>> = {
            let (creds, clock) = (creds.clone(), clock.clone());
            Box::new(move |foreign| {
                Ok(VenueRestOkx {
                    signer: OkxV5Signer::new(&creds, clock.ms()),
                    transport: rest_transport_on(sim, &clock),
                    base_url: base_url.clone(),
                    symbol: foreign.to_uppercase(),
                    properties: SymbolProperties::default(),
                    ct_val: 0.0,
                    leverage: DEFAULT_LEVERAGE,
                    broker_code: None,
                })
            })
        };
        confirm_closure(
            VenueRestPerSymbol::new(
                symbol.clone(),
                mounted,
                make,
                order_client_ids.clone(),
                refused_no_client,
            ),
            ledger.clone(),
            |_| true,
        )
    };
    // Funding-bill poll: perp-only by construction (this crate's ONLY exec client is the
    // SWAP-perp one — spot has no exec client here), so spawning it here is inherently scoped
    // to perp starts.
    let (funding_stop, funding_join) = spawn_funding_poll(
        creds.clone(),
        symbol.clone(),
        events.clone(),
        &net,
        Arc::clone(&admitted),
        clock.clone(),
    );
    let actor = ExecutionClientActor::spawn("okx-exec", events.clone(), move |rx| {
        run(
            creds,
            symbol,
            events,
            rx,
            properties_rec,
            on_reconcile,
            broker_code,
            net,
            leverage,
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

/// The client over its command `actor`: the shared actor itself, with this venue's hooks. It declares the trigger
/// amend ([`ExecutionClientActor::with_trigger_amend`]: the shared `run_loop` reads a Modify's trigger and
/// `VenueRestOkx::modify_trigger` sends it, `amend-algos`, through every wrapper between, `VenueRestOrderSubmitFence`, `VenueRestDispatch`,
/// `VenueRestPerSymbol`), and it teaches the router which instrument each restored order rests on (the core announces
/// them at boot and after a gatewayhub restart; [`ExecutionClientActor::with_adopt_restored`]): a cancel of an order this
/// process never submitted would otherwise go to the MOUNTED instrument, where the venue answers `51401`/`51402`;
/// the adapter's cancel then asks the algo endpoint too and reads its "unknown" as gone, so it reports a LIVE order
/// cancelled. The hook never touches the network (the fold calls it).
fn over(
    actor: ExecutionClientActor,
    order_client_ids: Arc<OrderClientIdSymbols>,
) -> ExecutionClientActor {
    actor
        .with_trigger_amend()
        .with_adopt_restored(Arc::new(move |orders| order_client_ids.adopt(orders)))
}

/// The exec thread's SUBMIT client — the one [`VenueRestOkx`] that places, cancels, amends and sets
/// leverage (`run_loop`'s `rest`), and so the ONE okx transport on the exec side that rides the
/// order-rate gate ([`crate::transport::ratelimit::rest_rate_gate`], the `OKX.orders` row: 50 per 2 s, ~17%
/// under the venue's 60/2s place-order budget per UID and instrument).
///
/// ⚠ **A gate that is not built here is not on the wire.** Every smoke attaches the gate to its own
/// transport, so the gate was green everywhere a test looked and absent from the one place orders
/// leave the process: `crate::transport::ratelimit`'s per-transport-is-per-instrument argument holds only if
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
///   (`crate::reconcile::recon_client`). They are not merged on purpose: a reconcile burst
///   queued ahead of a cancel in one FIFO window is the order-path delay the exempt `signed_requery`
///   split exists to prevent.
/// - One window per start is the table's model (N symbols, N windows): the per-instrument bucket is
///   what the venue meters, and the 1000/2s sub-account ceiling above it is what N windows must
///   stay under (`crate::transport::ratelimit`'s module doc has the arithmetic).
///
/// `broker_code` is the resolved FD-broker code: this is the ONE client that ever builds an order
/// body (`submit_order`, on `run_loop`), so submitted orders stamp `tag`. `leverage` is the
/// mount-resolved operator leverage ([`leverage_for`], clamped) — this is the ONE client that posts
/// it (`set_leverage` right after this call); unset profile ⇒ [`DEFAULT_LEVERAGE`], unchanged.
/// `net` is the start's addresses and type: its REST base, and the `x-simulated-trading` header by
/// its type ([`simulated`]). `clock` is the one it signs on (`crate::transport::client::rest_transport_on`).
fn order_rest(
    creds: &Credentials,
    symbol: &str,
    properties: SymbolProperties,
    ct_val: f64,
    leverage: f64,
    broker_code: Option<String>,
    net: &OkxNet,
    clock: &VenueClock,
) -> VenueRestOkx<RestTransportJsonUreq> {
    VenueRestOkx {
        signer: OkxV5Signer::new(creds, clock.ms()),
        transport: rest_transport_on(simulated(net.mainnet), clock)
            .with_rate_gate(crate::transport::ratelimit::rest_rate_gate()),
        base_url: net.rest.clone(),
        symbol: symbol.to_string(),
        properties,
        ct_val,
        leverage,
        broker_code,
    }
}

/// **The one write of the account's leverage for an instrument the engine was NOT mounted on** (the
/// gatewayhub lane's per-symbol hand-over, `VenueAdapter::set_symbol_leverage`): [`VenueRestOkx::set_leverage`]
/// itself (`mgnMode: cross`, idempotent on the venue), the verb the exec thread posts at its start, on
/// a client of its own because its foreign-instrument clients post none on purpose. `leverage` is
/// written as given: the lane's probe refused a figure above the instrument's maximum. `net` is the
/// account's addresses and type: its `x-simulated-trading` switch is the type's ([`simulated`]). `Err` is
/// the venue's own words, and says whether the outcome is unknown ([`LeverageWriteError`]).
///
/// ⚠ **DEMO only, guarded HERE**: a real-network account (`net.mainnet`) is refused before any client exists
/// (owner ruling 2026-10-26: no exchange write on a live account; `vike_secrets::LEVERAGE_VENUES` opens okx
/// at `demo` alone today, and opening `live` in that table must not by itself open a per-symbol mainnet
/// write).
pub fn write_symbol_leverage(
    creds: &Credentials,
    net: &OkxNet,
    symbol: &str,
    leverage: f64,
) -> Result<(), LeverageWriteError> {
    write_demo_only(crate::execution::rest::VENUE, net.mainnet, symbol)?;
    if InstrumentType::of_inst_id(&symbol.to_uppercase()) == Some(InstrumentType::Spot) {
        return Err(LeverageWriteError::Refused(format!(
            "okx: {symbol} is a spot pair, which holds no leverage; nothing was sent"
        )));
    }
    post_symbol_leverage(&symbol_leverage_rest(creds, net, symbol, leverage))?;
    tracing::info!(target: "vike_okx", %symbol, leverage, "set-leverage POSTED for an instrument admitted on demand (the gatewayhub reads it back)");
    Ok(())
}

/// The client [`write_symbol_leverage`] posts through: the order client's own build ([`order_rest`], on
/// the order window), pointed at the admitted instrument. `set_leverage` reads neither the grid nor
/// the contract size nor the broker code, so those stay empty.
fn symbol_leverage_rest(
    creds: &Credentials,
    net: &OkxNet,
    symbol: &str,
    leverage: f64,
) -> VenueRestOkx<RestTransportJsonUreq> {
    // A one-shot client: it reads the venue's clock for itself (owner ruling B-4).
    let clock = VenueClock::default();
    let symbol = symbol.to_uppercase();
    order_rest(creds, &symbol, SymbolProperties::default(), 0.0, leverage, None, net, &clock)
}

/// **The account's position mode put right BEFORE the gatewayhub's read-only leverage probe**
/// (`VenueAdapter::heal_modes`): [`crate::account::modes::settle_modes`] itself, the probe `run` makes at the
/// start (a flat long/short account is switched to net mode; read first, DEMO network only: a transport
/// without the simulated-trading header is left alone with no request), on a client of its own. The mode is
/// the account's, so `symbol` only names the log line. `Err` is the venue's words for a switch it refused
/// or a blocker it found (what holds it); the lane goes on to the probe, which refuses the figure under its
/// own reason.
pub fn heal_symbol_modes(creds: &Credentials, net: &OkxNet, symbol: &str) -> Result<(), String> {
    let rest = symbol_leverage_rest(creds, net, symbol, DEFAULT_LEVERAGE);
    let report = crate::account::modes::settle_modes(&rest.transport, &rest.signer, &rest.base_url);
    crate::account::modes::announce(&report, &rest.symbol);
    match report.position {
        vike_bridge_core::venue_adapter::ModeHealOutcome::Refused(why) => Err(why),
        _ => Ok(()),
    }
}

/// [`write_symbol_leverage`]'s post and its error words, over any transport. A timeout, a 5xx or an
/// unreadable answer (`transport::error_codes::submit_outcome_unknown`) is [`LeverageWriteError::Ambiguous`]: the POST may
/// have landed, so the lane reads the venue back instead of reporting a refusal.
fn post_symbol_leverage<T: RestTransportJson>(
    rest: &VenueRestOkx<T>,
) -> Result<(), LeverageWriteError> {
    rest.set_leverage().map_err(|e| {
        let ambiguous = crate::transport::error_codes::submit_outcome_unknown(e.code);
        LeverageWriteError::of_set_leverage(&rest.symbol, ambiguous, e)
    })
}

/// The first half of [`class_row_refusal`], asked BEFORE the inverse refusal so that an option's or a mislabelled
/// row is told by what it IS: the row's own `instType` must be the class its instId's shape names.
fn class_type_refusal(instrument: &str, row: &OkxInstrument) -> Result<(), String> {
    let class = InstrumentType::of_inst_id_or_swap(instrument);
    match row.inst_type.as_deref() {
        Some(word) if word != class.inst_type() => Err(format!(
            "okx: cannot sign {instrument:?}: its name reads as a {} instrument and the venue's row says {word}; \
             the order was not sent",
            class.inst_type()
        )),
        None if class != InstrumentType::Swap => Err(format!(
            "okx: cannot sign {instrument:?}: the venue's row states no instType; the order was not sent"
        )),
        _ => Ok(()),
    }
}

/// **The row held against the class its instId's shape names** (decision 0232), before a client is built on
/// it: the row's own `instType` must be that class's (a shape that lies would sign a SPOT id in contracts), a
/// SPOT pair or a dated future must be `live` on this network, a SPOT pair must be quoted in the ledger's
/// money `ledger` (USDT on a linear mount; a pair quoted in another asset would be valued at par), and a dated
/// future must be settled in `ledger` and not yet delivered. A SWAP row passes unchanged (its USDT fence is the
/// door's and the facts', as it was). `Err` is the sentence the terminal `OrderRejected` carries.
fn class_row_refusal(instrument: &str, row: &OkxInstrument, ledger: &str) -> Result<(), String> {
    class_type_refusal(instrument, row)?;
    let class = InstrumentType::of_inst_id_or_swap(instrument);
    if class == InstrumentType::Swap {
        return Ok(());
    }
    if let Some(state) = row.state.as_deref().filter(|s| *s != "live") {
        return Err(format!(
            "okx: cannot sign {instrument:?}: the venue lists it as {state}, not live; the order was not sent"
        ));
    }
    let usdt =
        |word: &Option<String>| word.as_deref().is_some_and(|w| w.eq_ignore_ascii_case(ledger));
    match class {
        InstrumentType::Spot if !usdt(&row.quote_currency) => Err(format!(
            "okx: cannot sign {instrument:?}: it is quoted in {}, and the engine's ledger is {ledger}, so only a \
             {ledger}-quoted pair is traded; the order was not sent",
            row.quote_currency.as_deref().unwrap_or("no currency")
        )),
        InstrumentType::Futures if !usdt(&row.settle_ccy) => Err(format!(
            "okx: cannot sign {instrument:?}: it settles in {}, and only a {ledger}-settled future is traded \
             (the engine's ledger is {ledger}); the order was not sent",
            row.settle_ccy.as_deref().unwrap_or("no currency")
        )),
        InstrumentType::Futures => {
            crate::instrument::expiry_refusal(instrument, vike_model::now_ms(), true)
                .map_or(Ok(()), Err)
        }
        _ => Ok(()),
    }
}

/// The signing client for an instrument OTHER than the mounted one, from the `public/instruments`
/// row fetched for it under the instId's own `instType` (a SWAP, a dated USDT future or a USDT-quoted spot
/// pair: [`class_row_refusal`]), or `Err(reason)` (shown to the operator in the terminal `OrderRejected`)
/// when it may not be signed: no row (the fetch failed, the id is unknown or is not live on this network;
/// an option is none of the three), an inverse contract ([`inverse_swap_refusal`], the same refusal the
/// mounted instrument gets), or a row whose `ctVal`/lot/tick is not a positive number (`to_contracts`
/// would size it 0).
/// Never a guessed or BTC-shaped fallback. On success the instrument's `ctVal` is registered in
/// `sizes` BEFORE any order is sent, so its fills come back converted with it.
///
/// The client is built by [`order_rest`], so it carries a rate window of its OWN: the venue meters
/// placing 60 per 2 s per instrument ([`crate::transport::ratelimit`]'s module doc). Leverage is carried but NOT
/// posted for the new instrument BY THIS CLIENT: the account keeps that instrument's current leverage
/// unless the gatewayhub lane writes it through [`write_symbol_leverage`] (above), which it does only
/// after a probe that refuses an open position held at another leverage.
///
/// `coin` is the settle coin of an engine MOUNTED on a coin-margined contract ([`mount_money`]), `None` on a
/// linear mount (everything above, unchanged). Such an engine signs only a coin-margined contract of its own
/// coin ([`coin_fence`]), in CONTRACTS passed through: its factor is 1.0, as the mounted contract's is.
fn foreign_rest(
    creds: &Credentials,
    instrument: &str,
    row: Option<OkxInstrument>,
    sizes: &ContractSizes,
    leverage: f64,
    broker_code: Option<String>,
    net: &OkxNet,
    coin: Option<&str>,
    clock: &VenueClock,
) -> Result<VenueRestOkx<RestTransportJsonUreq>, String> {
    let Some(row) = row else {
        return Err(match InstrumentType::of_inst_id(instrument) {
            Some(class @ (InstrumentType::Spot | InstrumentType::Futures)) => format!(
                "okx: cannot sign {instrument:?}: public/instruments lists no {} row for it (unknown, not \
                 live on this network, or the list was unreachable); the order was not sent",
                class.inst_type()
            ),
            _ => format!(
                "okx: cannot sign {instrument:?}: no SWAP instrument row for it (unknown instrument, not a \
                 SWAP — this adapter trades SWAP only — or public/instruments was unreachable); the \
                 order was not sent"
            ),
        });
    };
    class_type_refusal(instrument, &row)?;
    match coin {
        Some(coin) => coin_fence(instrument, &row, coin)?,
        None => {
            if let Some(why) = inverse_swap_refusal(instrument, Some(&row)) {
                return Err(why);
            }
        }
    }
    class_row_refusal(instrument, &row, coin.unwrap_or("USDT"))?;
    let positive = |x: f64| x.is_finite() && x > 0.0;
    let p = &row.properties;
    if !(positive(row.ct_val) && positive(p.step_size) && positive(p.tick_size)) {
        return Err(format!(
            "okx: cannot sign {instrument:?}: its instrument row has no usable ctVal/lotSz/tickSz \
             ({}/{}/{}); the order was not sent",
            row.ct_val, p.step_size, p.tick_size
        ));
    }
    // `sz` divides by `ctVal` alone: a contract worth `ctVal x ctMult` would be mis-sized by the
    // multiplier.
    if row.multiplied() {
        return Err(format!(
            "okx: cannot sign {instrument:?}: its contract multiplier ctMult is {} (this adapter sizes \
             a contract by ctVal alone); the order was not sent",
            row.ct_mult
        ));
    }
    let unit = if coin.is_some() { 1.0 } else { row.ct_val };
    sizes.register(instrument, unit);
    Ok(order_rest(creds, instrument, row.properties, unit, leverage, broker_code, net, clock))
}

/// The [`VenueRestPerSymbol`] factory: [`foreign_rest`] on a freshly fetched row. The fetch is one
/// blocking public `instruments` GET on the exec thread, bounded by the transport's timeout. `coin` is
/// [`foreign_rest`]'s.
fn foreign_rest_factory(
    creds: Credentials,
    net: OkxNet,
    leverage: f64,
    broker_code: Option<String>,
    sizes: Arc<ContractSizes>,
    coin: Option<String>,
    clock: VenueClock,
) -> ClientFactory<VenueRestOkx<RestTransportJsonUreq>> {
    Box::new(move |symbol| {
        // instruments rows are keyed by the upper-case instId
        let instrument = symbol.to_uppercase();
        let row = fetch_foreign_row(&instrument, &net);
        foreign_rest(
            &creds,
            &instrument,
            row,
            &sizes,
            leverage,
            broker_code.clone(),
            &net,
            coin.as_deref(),
            &clock,
        )
    })
}

/// Whether a fill of `instrument` may be sized at all: a SWAP always (as ever), a SPOT pair or a dated future only
/// when the engine ADMITTED it ([`load_linear_ct_val`] says why).
fn fill_may_be_sized(instrument: &str, admitted: &ReconcileAdmittedSymbols) -> bool {
    InstrumentType::of_inst_id(instrument).is_none_or(|class| class == InstrumentType::Swap)
        || admitted.symbols().iter().any(|a| a.eq_ignore_ascii_case(instrument))
}

/// A fill's contracts→base factor for an instrument the exec thread holds no client for (a resting
/// order from before a restart, or one placed by hand on the exchange): one row fetch (bounded by
/// [`FOREIGN_FETCH_TIMEOUT`]; [`ContractSizes::get_or_load`] remembers a failure for a while). On a linear
/// mount (`coin` `None`), linear contracts only (an inverse swap's `ctVal` is a USD amount, not base per
/// contract); on a coin-margined mount, only a coin-margined contract of the engine's own `coin`
/// ([`coin_fence`]), whose contracts pass through at 1.0. `None` = unreadable or not this engine's.
///
/// **A SPOT pair or a dated future is sized only when the engine ADMITTED it** (`admitted`; a client that
/// placed an order on it registered its size already): the private `orders` channel is subscribed for every
/// instrument class now, so a coin the operator trades by hand in the OKX app on the same account would
/// otherwise reach the engine as fills of a symbol it never admitted. A SWAP is sized as it always was.
fn load_linear_ct_val(
    instrument: &str,
    net: &OkxNet,
    admitted: &ReconcileAdmittedSymbols,
    coin: Option<&str>,
) -> Option<f64> {
    if !fill_may_be_sized(instrument, admitted) {
        return None;
    }
    let row = fetch_foreign_row(&instrument.to_uppercase(), net)?;
    if let Some(coin) = coin {
        return coin_fence(instrument, &row, coin).is_ok().then_some(1.0);
    }
    inverse_swap_refusal(instrument, Some(&row))
        .is_none()
        .then_some(row.ct_val)
        .filter(|c| c.is_finite() && *c > 0.0)
}

/// **A client for a CANCEL or a status query of any instrument**, built with no network, no grid and no
/// contract size (an `instId` and an order client id are all they send): the command loop's light factory, and the
/// router's whole factory while the mounted row has not loaded ([`run`]'s wait).
fn light_factory(
    creds: &Credentials,
    net: &OkxNet,
    clock: &VenueClock,
) -> ClientFactory<VenueRestOkx<RestTransportJsonUreq>> {
    let (creds, net, clock) = (creds.clone(), net.clone(), clock.clone());
    Box::new(move |inst| {
        Ok(order_rest(
            &creds,
            &inst.to_uppercase(),
            SymbolProperties::default(),
            0.0,
            DEFAULT_LEVERAGE,
            None,
            &net,
            &clock,
        ))
    })
}

/// The exec thread: build the REST client, set leverage, start the private-WS pump, then drain
/// commands until `Shutdown`. ALL network I/O lives HERE — off the single-writer core thread.
#[expect(clippy::too_many_arguments)] // Task 7's on_reconcile / task 4's broker_code pushed this past the default threshold
fn run(
    creds: Credentials,
    symbol: String,
    events: EventSender,
    rx: Receiver<ExecutionClientActorCommand>,
    properties_rec: Option<Arc<vike_data::PropertiesRecorder>>,
    on_reconcile: Option<mpsc::Sender<()>>,
    broker_code: Option<String>,
    net: OkxNet,
    leverage: f64,
    ledger: Arc<DispatchLedger>,
    order_client_ids: Arc<OrderClientIdSymbols>,
    admitted: Arc<ReconcileAdmittedSymbols>,
    clock: VenueClock,
) {
    // Fetch the venue's REAL grid + contract value for `symbol` (off the core thread): until they load
    // every command is refused in words and the fetch asked again, so orders size/price on the right
    // lot/tick/ct_val or not at all (no fallback grid, no guessed ctVal: owner 2026-11-13). Recorded into
    // the PIT properties store below. The SAME response also carries `lever` — this instrument's published
    // account-leverage ceiling. Captured here (free: one request, three fields) and applied to the startup
    // `set_leverage` below. A row without one yields `None`, i.e. no cap and no clamp.
    // Meanwhile a cancel is SENT (it needs neither a grid nor a contract size) through the command loop's
    // router built with neither: the mounted instrument's client and the light factory for every other.
    let grid_less = || {
        let none = SymbolProperties::default();
        let mounted = order_rest(&creds, &symbol, none, 0.0, DEFAULT_LEVERAGE, None, &net, &clock);
        let light = light_factory(&creds, &net, &clock);
        VenueRestPerSymbol::new(
            symbol.clone(),
            mounted,
            light,
            Arc::clone(&order_client_ids),
            refused_no_client,
        )
    };
    let Some(row) = await_properties("okx", &symbol, &events, &rx, NEGATIVE_TTL, grid_less, || {
        fetch_okx_instrument_row(&symbol, &net)
    }) else {
        return;
    };
    // ⚠ **THE EXEC REFUSAL, on the venue's own word and before anything is built** (decision 0170): a word
    // no book is signed for stops here — no REST client, no
    // `set_leverage` POST, no private WS, no signed request of any kind — just a loop that rejects every
    // command with the reason. A COIN-MARGINED mount is signed since decision 0258's okx amendment
    // ([`mount_money`]): its quantity is the contract count, so every client below takes the factor 1.0, and
    // it signs nothing of another coin or kind ([`coin_fence`]).
    let money = match mount_money(&symbol, Some(&row)) {
        Ok(None) => inverse_swap_refusal(&symbol, Some(&row)).map_or(Ok(None), Err),
        other => other,
    };
    let coin = match money {
        Ok(money) => money.map(|(_, coin)| coin),
        Err(why) => {
            tracing::error!(target: "vike_okx", %symbol, "{why}");
            run_loop(&VenueRestRefusing { why }, &events, rx, Vec::new, DEFAULT_FILL_SENTINEL);
            return;
        }
    };
    let (properties, ct_val, venue_leverage_cap) =
        (row.properties, if coin.is_some() { 1.0 } else { row.ct_val }, row.max_leverage);
    vike_data::PropertiesRecorder::record_opt(
        &properties_rec,
        "okx",
        &symbol,
        properties,
        now_ns(),
    );
    // Ceiling: the start resolved the operator's REQUEST, this is the venue's own limit for this
    // instrument. `min(request, cap)` when a cap is known; UNKNOWN (no `lever`) ⇒ the requested
    // value rides through untouched, byte-identical to before this clamp
    // existed. `vike_bridge_core::leverage`'s module doc is the authority, including the residual
    // it leaves open (the RiskGate still sizes against the un-clamped request).
    let leverage = vike_bridge_core::leverage::clamp_to_venue_cap(
        leverage,
        venue_leverage_cap,
        crate::execution::rest::VENUE,
        &symbol,
    );
    // Demo/mainnet selector for this start's PRIVATE exec + user-data WS. Decision 0095: the
    // account's tier alone decides now (`AdapterInputs::tier`, as `crate::adapter` resolves it into
    // `net`), resolved ONCE at the start and threaded in — this thread never re-resolves it. OKX
    // shares the REST host across both, so `sim` (the `x-simulated-trading` header, by the TYPE) and
    // the two socket URLs are what switch. A demo `net` (`sim` = true, the demo sockets) is
    // byte-identical to the historical hardcoded `new(true)` + the demo private socket.
    let sim = simulated(net.mainnet);
    let rest = order_rest(
        &creds,
        &symbol,
        properties,
        ct_val,
        leverage,
        broker_code.clone(),
        &net,
        &clock,
    );
    // The account's position mode, BEFORE the first order of any instrument: this adapter orders net
    // positions, and a long/short account refuses every one. A flat long/short account is switched to
    // net; anything else is said in words (`crate::account::modes`, tiers and limits included). Never
    // fails the start: a probe that cannot read leaves each order to the venue's own verdict.
    let modes = crate::account::modes::settle_modes(&rest.transport, &rest.signer, &rest.base_url);
    crate::account::modes::announce(&modes, &symbol);
    // Network, off the core thread; a benign "leverage unchanged" is tolerated inside.
    // Best-effort, UNCHANGED control flow (a rejection never kills the start) — but the discarded
    // `Result` is now WARNED on: a silently-failed leverage POST is the exact divergence this
    // wiring exists to close (the account keeps its old leverage while the RiskGate sizes against
    // the configured one), and binance/aster already warn here. ⚠ OKX re-raises on ANY non-`'0'`
    // code (see `VenueRestOkx::set_leverage`), so this can also fire on a benign repeat — a warn,
    // never an error, for exactly that reason.
    if let Err(e) = rest.set_leverage() {
        tracing::warn!(target: "vike_okx", %symbol, leverage, code = e.code, msg = %e.msg, "set_leverage failed (continuing) — the account keeps its CURRENT leverage while the RiskGate sizes against the configured one");
    }
    // From here the command loop's client signs each order on the ORDER's instrument: the mounted
    // instId (or none) goes to `rest` above unchanged, any other to a client built on first use, and
    // a FILL of any instrument is converted with that instrument's own `ctVal` (`sizes`).
    let sizes = Arc::new(ContractSizes::new(symbol.clone(), ct_val));
    // A foreign order's CANCEL and status take a client with no grid fetch (an `instId` and an order client id are
    // all they send; its grid and contract size are never read, so they are the inert defaults), so they
    // do not wait for `public/instruments`.
    let light = light_factory(&creds, &net, &clock);
    // The gap-sentinel below reads the foreign instruments the router was asked about
    // (`OrderClientIdSymbols::touch`); the replays size a foreign fill with the same `sizes` as the stream.
    let sentinel_order_client_ids = Arc::clone(&order_client_ids);
    let (resync_sizes, sentinel_sizes) = (Arc::clone(&sizes), Arc::clone(&sizes));
    let routed = VenueRestPerSymbol::new(
        symbol.clone(),
        rest,
        foreign_rest_factory(
            creds.clone(),
            net.clone(),
            leverage,
            broker_code,
            sizes.clone(),
            coin.clone(),
            clock.clone(),
        ),
        order_client_ids,
        refused_no_client,
    )
    .with_light_factory(light);
    // Audit A3: a SEPARATE REST client for the reconnect-resync supervisor (the submit client lives on
    // this thread) — replays recent order/fill history after every WS reconnect so a fill/cancel that
    // landed during the gap is recovered instead of leaving a phantom-live order.
    let resync_rest = VenueRestOkx {
        signer: OkxV5Signer::new(&creds, clock.ms()),
        transport: rest_transport_on(sim, &clock),
        base_url: net.rest.clone(),
        symbol: symbol.clone(),
        properties,
        ct_val,
        // Same mount-resolved value as the submit client; history-only, so it never posts it.
        leverage,
        broker_code: None, // history-only reads, never builds an order body
    };
    // A THIRD client for the run_loop fill-sentinel (also off the pump thread, so its history calls
    // never contend with the pump's own resync).
    let sentinel_rest = VenueRestOkx {
        signer: OkxV5Signer::new(&creds, clock.ms()),
        transport: rest_transport_on(sim, &clock),
        base_url: net.rest.clone(),
        symbol: symbol.clone(),
        properties,
        ct_val,
        // Same mount-resolved value as the submit client; history-only, so it never posts it.
        leverage,
        broker_code: None, // history-only reads, never builds an order body
    };
    // A FOURTH client, for the ALGO socket's reconnect-resync (its own, for the same reason).
    let algo_rest = VenueRestOkx {
        signer: OkxV5Signer::new(&creds, clock.ms()),
        transport: rest_transport_on(sim, &clock),
        base_url: net.rest.clone(),
        symbol: symbol.clone(),
        properties,
        ct_val,
        leverage,
        broker_code: None, // history-only reads, never builds an order body
    };
    let resync_symbol = symbol.clone();
    let sentinel_symbol = symbol.clone();
    let algo_admitted = Arc::clone(&admitted);
    // The three fill-sizing loaders (stream, resync, sentinel) each read the same admitted list and coin.
    let (stream_admitted, resync_admitted, sentinel_admitted) =
        (Arc::clone(&admitted), Arc::clone(&admitted), Arc::clone(&admitted));
    let (stream_coin, resync_coin, sentinel_coin) = (coin.clone(), coin.clone(), coin);
    // ...and the same addresses (the sentinel's closure borrows `net` itself).
    let (stream_net, resync_net) = (net.clone(), net.clone());
    // The fence both threads below share: the pump's fills for an order wait behind that order's own
    // `OrderSubmitted`/`OrderAccepted` (`vike_bridge_core::exec_actor::submit_fence`'s module doc has the
    // race: the stream's `live` row of a market order is its bare `OrderAccepted`, never held, and the fill
    // behind it can still beat the REST answer).
    let fence = Arc::new(SubmitFence::new(events.clone()));
    // The stop's OWN state (`orders-algo`) rides the business socket, a second pump beside the private
    // one, through the same fence: without it a stop cancelled or refused at the venue stays ACCEPTED.
    let algo_feed = spawn_okx_algo_user_data_with_resync(
        net.business_websocket.clone(),
        creds.api_key.expose().clone(),
        creds.api_secret.expose().clone(),
        creds.passphrase.as_ref().map(|p| p.expose().clone()).unwrap_or_default(),
        symbol.clone(),
        events.clone(),
        Arc::clone(&fence),
        // After a reconnect of the ALGO socket: the mounted instrument's ended stops, then every admitted
        // instrument's (read at the reconnect, so one admitted since the start is on the list).
        move || okx_algo_history_events_of(&algo_rest, &algo_admitted.symbols(), FOREIGN_READ_PACE),
        clock.clone(),
    );
    let feed = spawn_okx_perp_user_data_with_resync(
        net.private_websocket.clone(),
        creds.api_key.expose().clone(),
        creds.api_secret.expose().clone(),
        creds.passphrase.as_ref().map(|p| p.expose().clone()).unwrap_or_default(),
        symbol,
        move |instrument: &str| {
            sizes.get_or_load(instrument, || {
                load_linear_ct_val(
                    instrument,
                    &stream_net,
                    &stream_admitted,
                    stream_coin.as_deref(),
                )
            })
        },
        events.clone(),
        Arc::clone(&fence),
        // After a reconnect: the mounted instrument, then EVERY instrument the engine admitted.
        move || {
            let foreign = admitted.symbols();
            let events = okx_history_events_of(
                &resync_rest,
                &resync_symbol,
                ct_val,
                &foreign,
                &|instrument| {
                    resync_sizes.get_or_load(instrument, || {
                        load_linear_ct_val(
                            instrument,
                            &resync_net,
                            &resync_admitted,
                            resync_coin.as_deref(),
                        )
                    })
                },
                FOREIGN_READ_PACE,
            );
            if !foreign.is_empty() {
                tracing::info!(
                    target: "vike_okx", instruments = foreign.len(), events = events.len(),
                    "reconnect resync: replayed the history of the admitted symbols after the mounted one"
                );
            }
            events
        },
        on_reconcile,
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
        // Five seconds after the last command: the mounted instrument, then the foreign instruments the
        // commands since the last look named (not every admitted one: a sentinel per command over sixteen
        // instruments would be a burst of requests for nothing).
        || {
            okx_history_events_of(
                &sentinel_rest,
                &sentinel_symbol,
                ct_val,
                &sentinel_order_client_ids.take_touched(),
                &|instrument| {
                    sentinel_sizes.get_or_load(instrument, || {
                        load_linear_ct_val(
                            instrument,
                            &net,
                            &sentinel_admitted,
                            sentinel_coin.as_deref(),
                        )
                    })
                },
                FOREIGN_READ_PACE,
            )
        },
        DEFAULT_FILL_SENTINEL,
    );
    let _ = feed.shutdown();
    let _ = algo_feed.shutdown();
}

#[path = "execution_tests.rs"]
#[cfg(test)]
mod execution_tests;
