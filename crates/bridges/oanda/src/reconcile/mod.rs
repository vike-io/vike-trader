//! OANDA `ReconcileClient` (recon-breadth → OANDA) — the venue-facing report seam
//! (`vike_execution::recon::ReconcileClient`) over the v20 REST surface, reusing the crate's existing
//! Bearer-token [`OandaTransport`] transport. This is genuinely new: OANDA's existing "reconcile" is the
//! [`crate::execution::resync`] transactions-since backfill (the audit-A3 post-reconnect gap replay — a LIVE
//! order-event recovery path), NOT a `fetch_*` report seam the reconcile engine diffs against local
//! state. This module is that seam.
//!
//! ## Endpoints (four flat v20 GETs, one per report kind)
//! Each rides the SAME rustls `ureq` [`OandaTransport`] the data/exec sides use — a DEDICATED reconcile
//! instance built at [`ReconcileClientOanda::connect`], never the exec client's (the "dedicated
//! reconcile reads" discipline binance/deribit/ctrader/ibkr all follow). All requests carry
//! `Accept-Datetime-Format: UNIX`, so every OANDA timestamp arrives as an epoch-seconds STRING and
//! every numeric field (units/price/commission/balance) as a decimal STRING — hence [`json_number`]
//! (string-or-number tolerant) everywhere, never `as_f64`.
//! - orders — `GET /v3/accounts/{id}/orders?instrument={I}&state=PENDING` (the currently-resting
//!   set — a fully-closed order that fell out of it is what the fill/position reports reconcile,
//!   the same "pending only" semantics bybit/ctrader use).
//! - positions — `GET /v3/accounts/{id}/positions` (every instrument the account has traded,
//!   INCLUDING flat rows), filtered to the engine instrument, its long and short sides netted by
//!   `vike_model::PositionStatusReport::from_legs` (a locked pair on a hedging account is two rows).
//! - fills — `GET /v3/accounts/{id}/transactions/sinceid?id={floor}&type=ORDER_FILL`, the SAME
//!   `ORDER_FILL` wire shape [`crate::execution::user_data`]/[`crate::execution::resync`] already decode, so a reconcile
//!   fill maps identically to a live one, its order client id included (`clientExtensions.id`, else
//!   the transaction's top-level `clientOrderID`). See the fill-floor note below.
//! - balance — `GET /v3/accounts/{id}/summary` → `account.balance` (home-currency cash truth).
//!
//! ## Per-symbol, instrument-filtered (the ctrader/ibkr/binance-perp shape)
//! One [`ReconcileClientOanda`] per (account, USER name e.g. `EURUSD`, decision 0290). Each pass reads the user
//! name's wire name off its KEPT row (`EURUSD` → `EUR_USD`, `crate::names`); every report fetch is account-wide on
//! the wire and filtered client-side to that instrument, stamping the user name back onto each row so it matches
//! local state. A symbol with no kept row (OANDA's list is kept only once a running account's session has read it)
//! fails the fetch in words, folding nothing, and the connect says so at WARN: the start never waits for the list.
//! Positions synthesize a FLAT zero row when the instrument is absent (an instrument the account
//! never traded is omitted from `/positions` entirely), so `recon::diff` can still detect a stale
//! LOCAL position the venue has since closed — the same synthesize-flat contract okx/ctrader/ibkr use.
//!
//! ## Admitted symbols (any-symbol, decision 0173)
//! The client is built on the MOUNTED symbol, and [`ReconcileClient::admit_symbol`] tells it each symbol the
//! account's engine was admitted on demand. Every pass then reads the mounted instrument FIRST and each
//! admitted one after it: the orders read once per instrument with the request it always made
//! (`instrument=` and `state=PENDING`; no unfiltered read, whose 500-row cap could cut an instrument's
//! orders off silently), and the fills and positions, which are account-wide on the wire, parsed once per
//! instrument out of the one body. ANY failed read fails the whole fetch: the restore planner reads an
//! order a successful report lacks as gone, so an admitted symbol with no kept row fails it too. Two spellings of
//! one user name (`GBPUSD` and `gbpusd`) are one instrument, read once. With nothing admitted a pass makes exactly
//! the requests it made before. `ReconcileAdmittedSymbols` caps the list at the engine's own, so a pass costs at most that many extra
//! order reads.
//!
//! ## The fill floor (why `since` is a no-op here)
//! OANDA's object-returning transaction endpoint is `sinceid` (keyed by a monotonic transaction id),
//! and there is no clean map from the trait's `since` (epoch-ms) to a transaction id. So `since` is
//! ignored — instead the client holds a `fill_floor` transaction-id watermark, initialized at
//! connect and advanced past each fetched batch (the SAME watermark discipline `exec`/`stream`'s
//! shared `last_seen` uses). The window is therefore "fills since reconcile started, advancing each
//! pass", not a `since`-bounded lookback, and the reconcile engine's `trade_id` dedup absorbs any
//! re-seen row. Advancing the floor (rather than a fixed connect-time floor) also keeps each request
//! small. OANDA's reference states no row cap on `sinceid` and no paging parameter (read 2026-11-06), and none has
//! been measured; `crate::execution::resync::max_transaction_id` prefers the body's `lastTransactionID`, so a cap would skip rows.
//!
//! **Where the floor starts.** A start handed a state directory keeps the floor in a small file
//! (`crate::reconcile::fill_floor`: written whenever a pass moves it, read at connect), so a fill that happened
//! while the node was down is read by the first pass after the start and reaches the reconcile engine as
//! a `MissingFill` that names its order, instead of lying below a connect-time floor with only the
//! position drift to show for it. A start with nothing usable persisted (the first start, no state
//! directory, a floor that is above the account's latest transaction or too far behind it) starts at the
//! account's current `lastTransactionID`: fills before THAT connect are the position report's job.
//!
//! ⚠ The floor is ACCOUNT-wide, so a pass advances it over the fills of instruments the client does not
//! read (yet). An instrument admitted after that (a pass that ran before the admission reached this client,
//! or a client rebuilt and re-taught its admitted list after its first passes) would never report the
//! fills the floor already passed. So the client remembers which instruments it has read from the floor
//! (`fills_read`) and reads a newly admitted one ONCE from the floor it started with (`start_floor`):
//! one more `sinceid` GET, parsed for that instrument alone, and the floor moves only when every read of
//! the fetch succeeded. Fills before the client started stay the position report's job, as above.
//!
//! **The floor moves when the NEXT pass asks for its orders, not when the fills were read.** The manager's
//! pass is three reads (`ReconcileClient::fetch_mass_status`: orders, fills, positions, three frames from the
//! tradehub) and drops everything when one fails, so a floor moved by the fill read let a pass that then
//! failed on its positions read eat the fills it had read: no later pass named them, and what was left of
//! the fill was a `PositionDrift`. Moved at the positions answer it still ate them when the node stopped before
//! folding the reply. This client therefore keeps what a fill read would move (`FillRead`): a failed positions read
//! drops it and the next pass reads the same fills again, an answered one holds it until the next pass's orders read
//! (`answered_read`), a request that says the node finished the pass before it. The balance read comes between and
//! does not stop a pass, so it does not count. A node and a gateway that stop before that request leave the floor
//! where it was, on disk too, and the first pass of the next run reads those fills again, which the engine's restored
//! `seen_trade_ids` absorbs.
//!
//! **A lease that ended takes that read with it.** A session outlives its lease (`crates/vike-gatewayhub/src/trade/session.rs`'s
//! module doc), so after a link flap or a restart of the tradehub ALONE the next orders read reaches the SAME client,
//! and the reply it would apply may never have been folded. The gatewayhub's reconcile worker says so first
//! (`ReconcileClient::lease_ended`): the client drops what it holds, the floor stays, and the next pass reads those fills
//! again, which the engine's `trade_id` dedup absorbs.
//!
//! shortcut: a node that gives up on a reply and keeps its connection (its stall bound,
//! `crates/vike-gatewayhub-client/src/trade/recon.rs`'s `ReconcileClientGateway`) is not seen, so a fill of that pass the
//! stream also missed is left to the `PositionDrift`. Upgrade if the tradehub's journal ever shows `no reconcile frame
//! for` with no lease change after it.
//!
//! ## A 401 is asked again, inside the read
//! **OANDA refuses, now and then, a token it accepts.** The practice hosts answer `401 Insufficient
//! authorization to perform request.` to single requests of the one token and account they serve before and
//! after. Measured on the CI box's logs, 2026-10-16 to 2026-11-06: on the report reads, on `/summary` and on pricing-stream
//! opens on the OTHER host; in the worst stretch about 40 failed passes of 300 (worked out from the resume lines'
//! `stale_ms`), about twelve reads a pass, so about one read in ninety. This leg asks 0.2 times a second, far under the rates OANDA documents, and the logs show no restart at
//! the rate's jump (about three flaps a day, mostly on half-hour marks, until 2026-11-05 14:08; about six an hour
//! since). A token's scope, a wrong host or a dead token would refuse EVERY request, so this is the venue's
//! answer and not a stored credential's. Why the venue says it is not known; each one used to cost a whole
//! pass, a WARN and a `reconcile leg resumed`.
//!
//! So every read here (the connect probe too: a 401 there leaves the session with no reconcile at all) asks
//! again after `AUTH_RETRY_PAUSES`, and only on a 401. A 401 that lasts fails the fetch as any failed read
//! does, nothing is folded, and its text does not change from pass to pass, so the manager's edge says it ONCE.
//! Each retry is an `info!` naming the read, so the journal counts them per read. The transactions stream's
//! backfill reads through the same function, and the stream's own opens have their own rule
//! (`crate::execution::user_data`'s `MAX_TERMINAL_OPENS`).
//!
//! ## No live fee lane
//! OANDA's cost is spread-based (standard accounts are commission-free; core-pricing commission is
//! reported per-fill, already carried on the fill report), and there is no per-account maker/taker
//! rate endpoint — so `fetch_fee_rates` stays the trait default (`Ok(None)`), and the caller keeps
//! the static [`vike_venues::fee_schedule_for`] default (fail-soft).
//!
//! Every wire→report mapping is a PURE free function (`parse_*`) over the already-parsed
//! `serde_json::Value` (the crate convention — cf. [`crate::history::candles::parse_candles`]), fixture-tested
//! offline in `tests/offline/oanda_reconcile_parse.rs` (synthetic bodies — no network). The `#[ignore]`d,
//! cred-gated `tests/oanda_reconcile_smoke.rs` proves the fetch+parse read-only against a live
//! fxPractice account.

pub(crate) mod fill_floor;
pub(crate) mod venue_read;

use std::collections::HashSet;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use serde_json::Value;

use vike_bridge_core::json::{get_f64, get_f64_opt, get_str_borrowed, json_number};
use vike_bridge_core::order_status::OrderStatusTable;
use vike_bridge_core::reconcile_admitted::ReconcileAdmittedSymbols;
use vike_execution::recon::{ReconcileClient, ReconcileFetchError};
use vike_model::events::{LiquiditySide, TradeId};
use vike_model::{
    ContractValue, FillReport, OrderStatus, OrderStatusReport, OrderType, PositionLeg,
    PositionStatusReport,
};

use crate::config::OandaConfig;
use crate::reconcile::fill_floor::{FloorFile, start_floor};
use crate::refusals::OandaReconcileRefusal;
use crate::transport::OandaTransport;
use vike_bridge_core::transport::ErrorKind;
use vike_bridge_core::transport::http_error::VenueApiHttpError;
use vike_venues::reconnect::ReconnectBackoff;
use vike_venues::venue_timings;

/// The venue key stamped on every report row.
pub const VENUE: &str = "oanda";

/// The waits before a reconcile read asks again after a 401. The longest refusal measured between two good
/// answers on a fresh connection is two pricing-stream opens 8 s apart with the next one served
/// (2026-11-06 03:28:15), and most were cured by that lane's next open 3 s later; a read costs at most 11 s more.
pub(crate) const AUTH_RETRY_PAUSES: [Duration; 2] =
    [Duration::from_secs(3), Duration::from_secs(8)];

/// A 200 whose body lacks the array its read exists for is NOT an empty report: an empty read of a real book is what
/// a false "gone" and a false drift are made of (the restore planner, `recon::diff`), and an empty fill read moves
/// the floor past fills nobody saw. So it fails the fetch like any failed read.
pub(crate) fn require_array(
    v: &Value,
    key: &'static str,
    what: &'static str,
) -> Result<(), String> {
    if v.get(key).is_some_and(Value::is_array) {
        return Ok(());
    }
    Err(OandaReconcileRefusal::NoArray { what, key }.to_string())
}

/// One GET of a report, asked again after each of `pauses` while OANDA answers 401 (the module doc's "A 401 is
/// asked again"). Every other answer is final. `what` names the read in the log; a path carries the account id.
pub(crate) fn read_past_401(
    rest: &OandaTransport,
    base: &str,
    path: &str,
    query: &str,
    what: &str,
    pauses: &[Duration],
) -> Result<Value, VenueApiHttpError> {
    let mut answer = rest.get(base, path, query);
    for pause in pauses {
        if !answer.as_ref().is_err_and(|e| e.status == 401) {
            return answer;
        }
        tracing::info!(
            venue = VENUE,
            read = what,
            pause_s = pause.as_secs(),
            "OANDA answered 401 to a read; asking again"
        );
        std::thread::sleep(*pause);
        answer = rest.get(base, path, query);
    }
    answer.map_err(|mut e| {
        if e.status == 401 {
            e.message.push_str(&format!(
                " Asked {} times over {} s; a 401 that lasts means OANDA refuses this account's API_KEY or ACCOUNT_ID.",
                pauses.len() + 1,
                pauses.iter().sum::<Duration>().as_secs()
            ));
        }
        e
    })
}

/// What one fill read moves once the next pass asks for its orders (the module doc's fill floor).
struct FillRead {
    /// The highest transaction id the read's body states.
    floor: Option<u64>,
    /// The instruments it read from the floor for the first time.
    instruments: Vec<String>,
}

// --- pure field helpers (OANDA sends every scalar as a STRING under `Accept-Datetime-Format: UNIX`) -

/// An OANDA epoch-seconds timestamp string (`"1478012400.000000000"`) → epoch-MILLIS. Absent/
/// unparseable → `0`. The exact read every OANDA `time` takes (`vike_utility::calendar::parse_epoch_seconds_ms`),
/// the one [`crate::execution::user_data::fill_from_transaction`] stamps a fill with.
fn ts_ms(v: &Value, key: &str) -> i64 {
    vike_utility::calendar::parse_epoch_seconds_ms(get_str_borrowed(v, key)).unwrap_or(0)
}

/// The order/fill's echoed `clientExtensions.id` (vike sets it = order_client_id). Empty/absent →
/// `None` — the "externally-placed order" convention every venue parser uses for an absent client id.
/// Only that field: the order path reads nothing else, and [`parse_fill_reports`] adds its own
/// top-level `clientOrderID` fallback.
fn order_client_id(v: &Value) -> Option<String> {
    v.pointer("/clientExtensions/id")
        .and_then(|c| c.as_str())
        .filter(|c| !c.is_empty())
        .map(str::to_string)
}

/// OANDA `units` are a SIGNED string (long > 0, short < 0). `< 0` → sell (`-1`), else buy (`+1`).
fn side_of(units: f64) -> i32 {
    if units < 0.0 { -1 } else { 1 }
}

// --- pure parsers (over the already-parsed v20 JSON bodies — the fixture-tested units) -----------

/// Map an OANDA order `state` onto the FSM [`OrderStatus`] `recon::diff` reads — every state v20
/// documents, explicitly: `PENDING` and `TRIGGERED` → `Accepted` (both live); `FILLED` → `Filled`;
/// `CANCELLED` → `Canceled`. OANDA carries no partial-fill state on the *order* object (the filled
/// portion becomes a separate Trade), so there is no `PartiallyFilled` derive here.
///
/// A word outside that list is WARNED and read as `Accepted` (a `/orders?state=PENDING` fetch only
/// returns resting orders; `vike_bridge_core::order_status::unknown_status_word`).
pub fn normalize_order_state(state: &str) -> OrderStatus {
    ORDER_STATE.status_of(VENUE, state)
}

/// The words of [`normalize_order_state`].
const ORDER_STATE: OrderStatusTable = OrderStatusTable {
    words: &[
        ("PENDING", OrderStatus::Accepted),
        ("TRIGGERED", OrderStatus::Accepted),
        ("FILLED", OrderStatus::Filled),
        ("CANCELLED", OrderStatus::Canceled),
    ],
};

/// Map an OANDA order `type` onto [`OrderType`]: `MARKET`, `LIMIT`, `STOP` and `TAKE_PROFIT` are
/// their twins, and `MARKET_IF_TOUCHED` (what `crate::execution` sends for a priceless `take_profit`) is a
/// `TakeProfit` too. Every other kind (`STOP_LOSS`, `GUARANTEED_STOP_LOSS`, `TRAILING_STOP_LOSS`,
/// `FIXED_PRICE`) has none and stays [`OrderType::Unrecognized`] lower-cased, as every report spelled
/// it before the type existed.
fn normalize_order_type(raw: &str) -> OrderType {
    match raw {
        "MARKET" => OrderType::Market,
        "LIMIT" => OrderType::Limit,
        "STOP" => OrderType::Stop,
        "TAKE_PROFIT" | "MARKET_IF_TOUCHED" => OrderType::TakeProfit,
        other => OrderType::Unrecognized(other.to_ascii_lowercase()),
    }
}

/// `{"orders":[Order,..]}` → [`OrderStatusReport`], filtered to `instrument` (the wire name) and stamped with the
/// user name `symbol`. `id` → `venue_order_id`; `clientExtensions.id` → `order_client_id`; `units`
/// (signed) → side/qty; `type` → `order_type` (`normalize_order_type`); `state` normalized;
/// `createTime` → ts. `filled_qty`/`avg_px` are `0.0` — a resting OANDA order carries no fill
/// progress (the filled portion is a separate Trade). Instrument-less exit orders (TAKE_PROFIT/STOP_LOSS/… reference a
/// `tradeID`, not an instrument) never match and are dropped — reconcile tracks entry orders.
pub fn parse_order_reports(v: &Value, instrument: &str, symbol: &str) -> Vec<OrderStatusReport> {
    let Some(orders) = v.get("orders").and_then(|o| o.as_array()) else {
        return Vec::new();
    };
    orders
        .iter()
        .filter(|o| get_str_borrowed(o, "instrument") == instrument)
        .map(|o| {
            let units = get_f64(o, "units");
            let order_type = normalize_order_type(get_str_borrowed(o, "type"));
            // The order's one `price`, as `crate::execution`'s body sent it: a LIMIT's limit, the level of
            // a STOP or a MARKET_IF_TOUCHED (a take-profit's trigger).
            let level = get_f64_opt(o, "price").filter(|p| p.is_finite() && *p > 0.0);
            let (price, trigger_price) = match order_type {
                OrderType::Limit => (level, None),
                OrderType::Stop | OrderType::TakeProfit => (None, level),
                _ => (None, None),
            };
            OrderStatusReport {
                venue: VENUE.to_string(),
                symbol: symbol.to_string(),
                venue_order_id: get_str_borrowed(o, "id").into(),
                order_client_id: order_client_id(o),
                side: side_of(units),
                order_type,
                qty: units.abs(),
                filled_qty: 0.0,
                average_price: 0.0,
                status: normalize_order_state(get_str_borrowed(o, "state")),
                timestamp: ts_ms(o, "createTime"),
                price,
                trigger_price,
                trail: None,
                trail_percent: None,
            }
        })
        .collect()
}

/// `{"positions":[Position,..]}` → `instrument`'s position rows, stamped with the user name `symbol`.
/// OANDA reports a Position as a `long` side (units ≥ 0) and a `short` side (units ≤ 0), each at its
/// own `averagePrice`; each side is one [`PositionLeg`] (a Position row carries no timestamp, so `ts`
/// is `0`), netted by [`PositionStatusReport::from_legs`]:
///
/// - one side open (every netting account): ONE `PositionSide::Both` row, the SIGN of `qty` carrying
///   direction (the net convention binance-perp/ibkr use, and the key the exec fill lane's `BOTH`
///   fills fold under), at that side's `averagePrice`;
/// - both sides open (a locked pair on a `hedgingEnabled` account): a `Long` and a `Short` row, each at
///   its own side's price, which reconcile holds; one net row priced at the larger side's average was a
///   cost basis the net does not have (owner, 2026-11-11: the locked-pair rule);
/// - both sides at zero (a closed instrument OANDA still lists): the flat row.
///
/// No isolated wallet → `margin_mode` stays the Cross default. When NO row matches, the answer is EMPTY
/// and the caller synthesizes the flat row (see [`ReconcileClientOanda::fetch_position_status_reports`]).
pub fn parse_position_reports(
    v: &Value,
    instrument: &str,
    symbol: &str,
) -> Vec<PositionStatusReport> {
    let Some(positions) = v.get("positions").and_then(|p| p.as_array()) else {
        return Vec::new();
    };
    let legs: Vec<PositionLeg> = positions
        .iter()
        .filter(|p| get_str_borrowed(p, "instrument") == instrument)
        .flat_map(|p| [p.get("long"), p.get("short")]) // `short.units` is already ≤ 0
        .map(|side| PositionLeg {
            size: side.map_or(0.0, |s| get_f64(s, "units")),
            price: side.map_or(0.0, |s| get_f64(s, "averagePrice")),
            timestamp_ms: 0,
        })
        .collect();
    if legs.is_empty() {
        return Vec::new();
    }
    // FX and CFDs are linear: an average reads only the contract's kind.
    PositionStatusReport::from_legs(VENUE, symbol, &legs, ContractValue::default())
}

/// `{"transactions":[Transaction,..]}` (a `sinceid` body) → [`FillReport`], keeping only
/// `ORDER_FILL` transactions for `instrument` and stamping the user name `symbol`. `id` (the
/// transaction id) → `trade_id`; `orderID` → `venue_order_id`; `clientExtensions.id`, else the
/// transaction's top-level `clientOrderID` (where a stop's fill carries it: measured 2026-10-26) →
/// `order_client_id`; `units` (signed) → side/qty; `price`, else `fullVWAP` (the v20 docs mark
/// `OrderFillTransaction.price` deprecated), and `commission` via [`json_number`]; `time` → ts. A row with no
/// finite non-zero `units` or no price above zero is skipped at ERROR, never reported at 0, as
/// `crate::execution::user_data`'s `fill_from_transaction` refuses it live. `commission_asset` is the
/// account `home_currency` (OANDA charges/reports in home currency; the fill row itself carries no
/// currency field); `liquidity_side` is always `Unknown` (no maker/taker flag on the wire). The `type == "ORDER_FILL"` guard is kept even though the request
/// asks for `type=ORDER_FILL` — belt-and-suspenders, since a body may echo other kinds.
pub fn parse_fill_reports(
    v: &Value,
    instrument: &str,
    symbol: &str,
    home_currency: &str,
) -> Vec<FillReport> {
    let Some(txns) = v.get("transactions").and_then(|t| t.as_array()) else {
        return Vec::new();
    };
    txns.iter()
        .filter(|t| {
            get_str_borrowed(t, "type") == "ORDER_FILL"
                && get_str_borrowed(t, "instrument") == instrument
        })
        // SKIP the row rather than admit an id-less report. `vike_execution::recon::diff` looks this id up
        // in `seen_trade_ids` to decide whether the fold thread already booked the fill; an empty id
        // can never match, so it does not go unrecognised — it FABRICATES a `MissingFill`, which the
        // `hybrid` policy AUTO-APPLIES, booking the fill a second time with no operator in front of
        // it. Skipping produces no divergence, which is the safe direction.
        .filter_map(|t| {
            let trade_id = match TradeId::new(get_str_borrowed(t, "id")) {
                Ok(t) => t,
                Err(_) => {
                    tracing::warn!(
                        venue = VENUE,
                        %symbol,
                        "ORDER_FILL transaction carries no `id` — skipping the fill report; an \
                         id-less one would fabricate a MissingFill that `hybrid` auto-applies"
                    );
                    return None;
                }
            };
            let number = |k: &str| get_f64_opt(t, k).filter(|x| x.is_finite());
            let units = number("units").filter(|u| *u != 0.0);
            let price = ["price", "fullVWAP"].into_iter().filter_map(number).find(|p| *p > 0.0);
            // A fill always states both, unlike an order row: a defaulted 0 would be booked at price 0.
            let (Some(units), Some(price)) = (units, price) else {
                tracing::error!(
                    venue = VENUE,
                    %symbol,
                    %trade_id,
                    "ORDER_FILL transaction states no usable `units` or price (`price`, `fullVWAP`) — \
                     skipping the fill report rather than reporting a fill at 0"
                );
                return None;
            };
            Some(FillReport {
                venue: VENUE.to_string(),
                symbol: symbol.to_string(),
                trade_id,
                venue_order_id: get_str_borrowed(t, "orderID").into(),
                // Same precedence as `execution/user_data.rs`'s `order_client_id`, which uses no `orderID` either: a venue id
                // is not our client id, and `None` is what names an external fill.
                order_client_id: order_client_id(t).or_else(|| {
                    t.get("clientOrderID")
                        .and_then(|c| c.as_str())
                        .filter(|c| !c.is_empty())
                        .map(str::to_string)
                }),
                side: side_of(units),
                last_qty: units.abs(),
                last_price: price,
                commission: crate::execution::funding::fill_charges(t),
                commission_asset: home_currency.to_string(),
                liquidity_side: LiquiditySide::Unknown,
                timestamp: ts_ms(t, "time"),
            })
        })
        .collect()
}

/// A `/summary` body → the account's home-currency `balance` (`account.balance`). `None` when the
/// field is absent — the account's realized cash truth `recon::diff_balance` reconciles against.
pub fn parse_balance(v: &Value) -> Option<f64> {
    v.get("account").and_then(|a| a.get("balance")).and_then(json_number)
}

// --- the client ----------------------------------------------------------------------------------

/// One reconcile client per (account, user name), holding its OWN dedicated
/// [`OandaTransport`] (never the exec transport's). `ReconcileClient`'s methods take `&self`; the underlying
/// `ureq::Agent` is `Send + Sync`, the `fill_floor` watermark is an [`AtomicU64`] and no lock is held
/// across a request. See the module doc for the per-symbol filter, the flat-row synthesis, and the
/// fill-floor / `since`-no-op rationale.
pub struct ReconcileClientOanda {
    rest: OandaTransport,
    rest_base: String,
    account_id: String,
    /// The mounted USER name (e.g. `EURUSD`) stamped on every report row; its wire name (`EUR_USD`), which the
    /// account-wide rows are filtered to, is its kept row's, read each pass ([`Self::pairs`]).
    symbol: String,
    /// What the connect probe learned: at connect, or on a later pass when that probe never reached OANDA
    /// ([`Self::probed`]).
    probe: OnceLock<SummaryProbe>,
    /// The probe a pass still owes while `probe` is empty; `None` once it answered.
    pending: Mutex<Option<PendingProbe>>,
    /// Highest transaction id consumed by a fill fetch — the `sinceid` floor, advanced each pass.
    fill_floor: AtomicU64,
    /// Where the floor is kept for the next run (`crate::reconcile::fill_floor`), when the start was handed a state directory.
    floor_file: Option<FloorFile>,
    /// The OANDA instruments whose fills a pass has read from the floor it advanced over: the mounted one
    /// from birth (when its kept row was there at connect), an admitted one once its backfill (or its first
    /// pass, when the floor had not moved yet) succeeded. An instrument not in it has fills the floor may already have passed.
    fills_read: Mutex<HashSet<String>>,
    /// What the last fill read moves, until its pass's positions read answers.
    fill_read: Mutex<Option<FillRead>>,
    /// The read of a pass whose positions read answered, until the next pass asks for its orders.
    answered_read: Mutex<Option<FillRead>>,
    /// The symbols the account's engine admitted on demand (any-symbol, decision 0173), in the engine's
    /// spelling: every pass reads them beside the mounted one (the module doc).
    admitted: ReconcileAdmittedSymbols,
    /// [`AUTH_RETRY_PAUSES`], unless a loopback test shortened them.
    auth_pauses: [Duration; 2],
}

/// What the connect probe (`GET /summary`) learned.
struct SummaryProbe {
    /// Account home currency (e.g. `USD`), stamped as each fill's `commission_asset`.
    home_currency: String,
    /// The floor this client started with (what the last run left, else the account's `lastTransactionID` at
    /// the probe): the oldest id a newly admitted instrument's fills are re-read from (the module doc's "backfill").
    start_floor: u64,
}

/// A connect probe that failed where a later one can pass ([`probe_can_pass_later`]): the next pass probes again, and
/// each one that fails waits a step of the exec lane's ladder (`venue_timings::OANDA`'s exec `reconnect`) before the
/// next, so a pass's later reads right behind a failed probe make none. A refusal ends it: asking again cannot cure one.
struct PendingProbe {
    backoff: ReconnectBackoff,
    /// No probe before this instant.
    due: Instant,
    /// OANDA refused a later probe: these words fail every later read, and no pass probes again.
    refused: Option<String>,
}

/// A connect probe that failed where a later one can pass: OANDA was not reached or its reply never came (a status
/// below 100), or it answered with a kind the shared taxonomy retries (429, a 5xx). A refusal (a 401 that lasts, a
/// 403, an unknown account's 404) cannot be cured by asking again.
fn probe_can_pass_later(e: &VenueApiHttpError) -> bool {
    e.status < 100 || ErrorKind::from_http_status(e.status).is_retryable()
}

impl ReconcileClientOanda {
    /// Open a DEDICATED reconcile client: build a fresh [`OandaTransport`] and probe
    /// `GET /v3/accounts/{id}/summary` — which validates the token+account (a refusal is the `Err`, which leaves this
    /// venue-symbol unwired, exec unaffected, the same graceful degradation every absent-recon path
    /// uses) AND captures the account `currency` (fills' `commission_asset`) and current
    /// `lastTransactionID` (the fill floor a start with nothing persisted begins at). A probe that can pass later
    /// ([`probe_can_pass_later`]: OANDA not reached, a 5xx) still builds the client, and the next pass probes again
    /// ([`PendingProbe`]). Never shares the exec side's
    /// transport. `state_dir` is `AdapterInputs::process.state_dir`: with one, the floor the last run left is
    /// the starting floor when it is usable, and the floor is kept there as it moves (`crate::reconcile::fill_floor`).
    pub fn connect(
        config: &OandaConfig,
        symbol: &str,
        state_dir: Option<&Path>,
    ) -> Result<ReconcileClientOanda, VenueApiHttpError> {
        Self::connect_pausing(config, symbol, state_dir, AUTH_RETRY_PAUSES)
    }

    /// [`Self::connect`] on the given waits between a 401 and the next ask; a loopback test passes them
    /// shortened, so it does not sit through the production seconds.
    #[doc(hidden)]
    pub fn connect_pausing(
        config: &OandaConfig,
        symbol: &str,
        state_dir: Option<&Path>,
        auth_pauses: [Duration; 2],
    ) -> Result<ReconcileClientOanda, VenueApiHttpError> {
        let client = ReconcileClientOanda {
            rest: OandaTransport::new(config.api_token.expose().clone()),
            rest_base: config.rest_base.clone(),
            account_id: config.account_id.clone(),
            symbol: symbol.to_string(),
            probe: OnceLock::new(),
            pending: Mutex::new(None),
            fill_floor: AtomicU64::new(0),
            floor_file: state_dir.map(|dir| FloorFile::new(dir, &config.account_id)),
            fills_read: Mutex::new(HashSet::new()),
            fill_read: Mutex::new(None),
            answered_read: Mutex::new(None),
            admitted: ReconcileAdmittedSymbols::default(),
            auth_pauses,
        };
        match client.probe_summary() {
            Ok(()) => {}
            Err(e) if probe_can_pass_later(&e) => {
                tracing::warn!(
                    venue = VENUE,
                    symbol,
                    error = %e,
                    "reconcile: the connect probe can pass later; the next pass asks again"
                );
                let reconnect = venue_timings::OANDA.exec().reconnect();
                *client.pending.lock().unwrap_or_else(PoisonError::into_inner) =
                    Some(PendingProbe {
                        backoff: ReconnectBackoff::new(reconnect.initial, reconnect.max),
                        due: Instant::now(),
                        refused: None,
                    });
            }
            Err(e) => return Err(e),
        }
        Ok(client)
    }

    /// The connect probe: `GET /summary` (the account's currency and `lastTransactionID`), the starting floor and its
    /// file, and the mounted instrument read from birth. Writes nothing when the read fails.
    fn probe_summary(&self) -> Result<(), VenueApiHttpError> {
        let path = format!("/v3/accounts/{}/summary", self.account_id);
        let v =
            read_past_401(&self.rest, &self.rest_base, &path, "", "connect", &self.auth_pauses)?;
        let account = v.get("account");
        let home_currency = account
            .and_then(|a| a.get("currency"))
            .and_then(|c| c.as_str())
            .unwrap_or_default()
            .to_string();
        let last_transaction = account
            .and_then(|a| a.get("lastTransactionID"))
            .or_else(|| v.get("lastTransactionID"))
            .and_then(|x| x.as_str())
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);
        let persisted = self.floor_file.as_ref().and_then(FloorFile::read);
        let floor = start_floor(persisted, last_transaction);
        // A floor that was not usable (or never written) is replaced by the one this start uses, so a node that
        // stops before its first pass still leaves a floor behind.
        if let Some(file) = &self.floor_file
            && persisted != Some(floor)
        {
            file.write(floor);
        }
        // A held-session venue's list may not be kept yet: the client is built anyway and each pass refuses until
        // the row exists (`Self::pairs`); a mounted instrument read later than this starts at this same floor.
        let mounted = crate::names::wire_name(&self.symbol);
        if let Err(why) = &mounted {
            tracing::warn!(
                venue = VENUE,
                symbol = %self.symbol,
                "reconcile: {why}; no pass reads anything until the symbol's kept row exists"
            );
        }
        self.fill_floor.store(floor, Ordering::Relaxed);
        *self.fills_read.lock().unwrap_or_else(PoisonError::into_inner) =
            mounted.into_iter().collect();
        let _ = self.probe.set(SummaryProbe { home_currency, start_floor: floor });
        Ok(())
    }

    /// What the connect probe learned, probing first when the start could not ([`PendingProbe`]).
    fn probed(&self) -> Result<&SummaryProbe, String> {
        if let Some(probe) = self.probe.get() {
            return Ok(probe);
        }
        let mut pending = self.pending.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(owed) = pending.as_mut() else {
            // Another read probed while this one waited for the lock.
            return self.probe.get().ok_or_else(|| OandaReconcileRefusal::NoProbe.to_string());
        };
        if let Some(words) = &owed.refused {
            return Err(words.clone());
        }
        let now = Instant::now();
        if now < owed.due {
            let wait_ms = (owed.due - now).as_millis();
            return Err(OandaReconcileRefusal::ProbePending { wait_ms }.to_string());
        }
        match self.probe_summary() {
            Ok(()) => {
                tracing::info!(
                    venue = VENUE,
                    symbol = %self.symbol,
                    "reconcile: the connect probe answered on a later pass; reconciliation runs from this pass"
                );
                *pending = None;
                self.probe.get().ok_or_else(|| OandaReconcileRefusal::NoProbe.to_string())
            }
            Err(e) if !probe_can_pass_later(&e) => {
                let words = OandaReconcileRefusal::ProbeRefused { why: e.to_string() }.to_string();
                tracing::error!(venue = VENUE, symbol = %self.symbol, "{words}");
                owed.refused = Some(words.clone());
                Err(words)
            }
            Err(e) => {
                let wait = owed.backoff.failed();
                owed.due = Instant::now() + wait;
                let (why, wait_ms) = (e.to_string(), wait.as_millis());
                Err(OandaReconcileRefusal::ProbeUnanswered { why, wait_ms }.to_string())
            }
        }
    }

    /// One report read ([`read_past_401`]); the error text names the read.
    fn read(&self, what: &'static str, path: &str, query: &str) -> Result<Value, String> {
        read_past_401(&self.rest, &self.rest_base, path, query, what, &self.auth_pauses)
            .map_err(|e| OandaReconcileRefusal::Unread { what, why: e.to_string() }.to_string())
    }

    /// `GET /transactions/sinceid` for the `ORDER_FILL`s after transaction `id` (account-wide).
    fn fills_after(&self, id: u64) -> Result<Value, String> {
        let path = format!("/v3/accounts/{}/transactions/sinceid", self.account_id);
        let query = format!("id={id}&type=ORDER_FILL");
        let v = self.read("fills", &path, &query)?;
        require_array(&v, "transactions", "fills")?;
        Ok(v)
    }

    /// Apply a fill read the node has finished with: move the floor past its batch, so this pass requests only
    /// newer transactions and each request stays bounded, and remember the instruments it read.
    fn apply_fill_read(&self, read: FillRead) {
        if let Some(max) = read.floor {
            let before = self.fill_floor.fetch_max(max, Ordering::Relaxed);
            // The floor moved: an earlier pass reported every fill below it, so the next run may start here.
            if max > before
                && let Some(file) = &self.floor_file
            {
                file.write(max);
            }
        }
        self.fills_read.lock().unwrap_or_else(PoisonError::into_inner).extend(read.instruments);
    }

    /// The `(wire name, user name)` pairs a pass reads: the mounted symbol first, then each admitted one, each wire
    /// name its kept row's (`crate::names::wire_name`). A symbol whose INSTRUMENT is already in the list is skipped,
    /// so no report row is produced twice.
    ///
    /// # Errors
    /// A symbol with no kept row (no list kept yet, an old spelling): the whole fetch fails in words and folds
    /// nothing, because a report that leaves an instrument out reads its orders as gone.
    fn pairs(&self) -> Result<Vec<(String, String)>, String> {
        let mut pairs: Vec<(String, String)> = Vec::new();
        for symbol in std::iter::once(self.symbol.clone()).chain(self.admitted.symbols()) {
            let instrument = crate::names::wire_name(&symbol)
                .map_err(|why| OandaReconcileRefusal::NotKept { why }.to_string())?;
            if !pairs.iter().any(|(i, _)| *i == instrument) {
                pairs.push((instrument, symbol));
            }
        }
        Ok(pairs)
    }
}

impl ReconcileClient for ReconcileClientOanda {
    /// `_since` is a no-op: `/orders?state=PENDING` reports only the CURRENTLY resting set (no time
    /// filter) — a closed order that fell out of it is what the fill/position reports reconcile. One
    /// read per instrument (the mounted one, then each admitted one), the first failure fails the fetch.
    /// Being asked is what moves the fill floor past the pass before this one (the module doc), whatever this read answers.
    fn fetch_order_status_reports(&self, _since: i64) -> Result<Vec<OrderStatusReport>, String> {
        self.probed()?;
        let answered = self.answered_read.lock().unwrap_or_else(PoisonError::into_inner).take();
        if let Some(read) = answered {
            self.apply_fill_read(read);
        }
        let path = format!("/v3/accounts/{}/orders", self.account_id);
        let mut reports = Vec::new();
        for (instrument, symbol) in &self.pairs()? {
            let query = format!("instrument={instrument}&state=PENDING&count=500");
            let v = self.read("orders", &path, &query)?;
            require_array(&v, "orders", "orders")?;
            reports.extend(parse_order_reports(&v, instrument, symbol));
        }
        Ok(reports)
    }

    /// `_since` is a no-op: the window is the `fill_floor` transaction-id watermark, not the caller's
    /// epoch-ms `since` (see the module doc). Fetches `sinceid` from the floor and maps the `ORDER_FILL`
    /// rows of every instrument the pass reads out of that ONE body; an instrument admitted after the floor
    /// had moved is read from the floor the client started with, in one more read. Moves nothing itself: the
    /// floor moves past the returned batch when the next pass asks for its orders (the module doc).
    fn fetch_fill_reports(&self, _since: i64) -> Result<Vec<FillReport>, String> {
        let probe = self.probed()?;
        let floor = self.fill_floor.load(Ordering::Relaxed);
        let pairs = self.pairs()?;
        let v = self.fills_after(floor)?;
        // An instrument admitted after the floor had moved: the floor is account-wide, so earlier passes
        // advanced over its fills unread. Its fills come from ONE read from the floor this client started
        // with (the others' rows are not taken from it: they were reported already), and a failed read fails
        // the fetch before the floor moves, so nothing is lost and the next pass asks again. While the floor
        // has not moved, the normal read already starts where the backfill would.
        let unread: Vec<&str> = {
            let read = self.fills_read.lock().unwrap_or_else(PoisonError::into_inner);
            pairs.iter().map(|(i, _)| i.as_str()).filter(|i| !read.contains(*i)).collect()
        };
        let backfill = if !unread.is_empty() && floor > probe.start_floor {
            Some(self.fills_after(probe.start_floor)?)
        } else {
            None
        };
        let mut reports = Vec::new();
        for (instrument, symbol) in &pairs {
            let body = match &backfill {
                Some(old) if unread.contains(&instrument.as_str()) => old,
                _ => &v,
            };
            reports.extend(parse_fill_reports(body, instrument, symbol, &probe.home_currency));
        }
        *self.fill_read.lock().unwrap_or_else(PoisonError::into_inner) = Some(FillRead {
            floor: crate::execution::resync::max_transaction_id(&v),
            instruments: unread.into_iter().map(str::to_string).collect(),
        });
        Ok(reports)
    }

    /// Account-wide `/positions` filtered to each instrument the pass reads. OANDA OMITS an instrument
    /// the account never traded, so an empty match synthesizes a FLAT `qty == 0` row — load-bearing for
    /// `recon::diff` to detect a stale LOCAL position the venue has since closed (a flat row that IS
    /// present — a closed position OANDA still lists — already folds to net `0`, same result).
    /// Its answer keeps the pass's fill read for the next pass's orders read to apply (the module doc); a failed read
    /// drops it.
    fn fetch_position_status_reports(&self) -> Result<Vec<PositionStatusReport>, String> {
        self.probed()?;
        let path = format!("/v3/accounts/{}/positions", self.account_id);
        let answer = self
            .read("positions", &path, "")
            .and_then(|v| require_array(&v, "positions", "positions").map(|()| v));
        let fill_read = self.fill_read.lock().unwrap_or_else(PoisonError::into_inner).take();
        let v = answer?;
        // Resolved before the fill read is kept: a pass that cannot name its instruments folds nothing.
        let pairs = self.pairs()?;
        if fill_read.is_some() {
            *self.answered_read.lock().unwrap_or_else(PoisonError::into_inner) = fill_read;
        }
        let mut reports = Vec::new();
        for (instrument, symbol) in &pairs {
            let matched = parse_position_reports(&v, instrument, symbol);
            if matched.is_empty() {
                // An instrument the account never traded is absent from `/positions`: the flat row is
                // what lets `recon::diff` see a stale LOCAL position the venue has since closed.
                reports.push(PositionStatusReport::flat(VENUE, symbol));
            } else {
                reports.extend(matched);
            }
        }
        Ok(reports)
    }

    /// Tell this client the engine admitted `symbol` (any-symbol, decision 0173): every later pass reads
    /// its orders, fills and position too. Idempotent, never fetches; the list is bounded and keeps the
    /// engine's spelling (upper-cased).
    fn admit_symbol(&self, symbol: &str) {
        self.admitted.admit_logged(symbol, &self.symbol, VENUE, "a symbol");
    }

    /// The engine gave `symbol` back (decision 0205): the next pass stops reading it.
    fn forget_symbol(&self, symbol: &str) {
        self.admitted.forget_logged(symbol, VENUE, "a symbol");
    }

    /// Another lease is asking: nothing a read of the old one holds was confirmed folded, so none of it moves the
    /// floor and the next pass reads those fills again (the module doc).
    fn lease_ended(&self) {
        *self.fill_read.lock().unwrap_or_else(PoisonError::into_inner) = None;
        if self.answered_read.lock().unwrap_or_else(PoisonError::into_inner).take().is_some() {
            tracing::info!(
                venue = VENUE,
                "reconcile: the lease ended before the next pass asked; the last pass's fills are read again"
            );
        }
    }

    fn admitted_symbols(&self) -> Option<Vec<String>> {
        Some(self.admitted.symbols())
    }

    /// The account `/summary`'s home-currency `balance` — the realized cash truth `diff_balance`
    /// reconciles against.
    ///
    /// Its failure is the read's text, so it reads unclassified; only the node logs it.
    fn fetch_balance(&self) -> Result<Option<f64>, ReconcileFetchError> {
        self.probed().map_err(ReconcileFetchError::unclassified)?;
        let path = format!("/v3/accounts/{}/summary", self.account_id);
        let v = self.read("balance", &path, "").map_err(ReconcileFetchError::unclassified)?;
        Ok(parse_balance(&v))
    }
}

// --- the factory ---------------------------------------------------------------------------------

/// The venue → `ReconcileClient` factory (the ReconFactory seam) for OANDA: opens
/// [`ReconcileClientOanda`]'s own dedicated reconcile transport from an already-resolved [`OandaConfig`].
/// `None` when the connect probe is refused (bad token / bad account id) — reconcile stays
/// unwired for this venue-symbol, exec unaffected; one that can pass later (OANDA unreachable, a 5xx) still builds the
/// client, and the next pass probes again (`ReconcileClientOanda::connect`).
///
/// **WIRED into this crate's start** — [`crate::adapter::VenueAdapterOanda`]'s `start` calls this
/// through `reconcile_if_enabled`, so the dedicated transport and its blocking `/summary` probe are
/// built only when reconciliation is armed and never on a start whose reconcile gate answered no.
/// (The caller was `vike_tradehub_core::make_engine`'s oanda arm until the venue-adapter contract moved the
/// arm into this crate.) ⚠ This line used to say "not yet wired", which it had stopped being: the
/// header of a factory is the WRONG place to read that from, because a start can adopt a factory
/// without touching it. Read the start.
/// `crates/bridges/ig/CLAUDE.md` carries the family rule — every venue enrolled in the reconcile
/// roster after its factory was written inherited the same stale sentence.
pub fn recon_client(
    config: &OandaConfig,
    symbol: &str,
    state_dir: Option<&Path>,
) -> Option<Box<dyn ReconcileClient>> {
    match ReconcileClientOanda::connect(config, symbol, state_dir) {
        Ok(c) => Some(Box::new(c) as Box<dyn ReconcileClient>),
        Err(e) => {
            tracing::warn!(target: "vike_oanda::recon", symbol, error = %e, "OANDA recon client unwired");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Inline sanity checks for the tiny helpers; the exhaustive body-parsing coverage (synthetic
    // JSON per endpoint) lives in `tests/offline/oanda_reconcile_parse.rs`, mirroring the ctrader recon
    // parse tests.

    #[test]
    fn side_of_reads_the_units_sign() {
        assert_eq!(side_of(1000.0), 1);
        assert_eq!(side_of(-1000.0), -1);
        assert_eq!(side_of(0.0), 1); // flat/zero → the conservative buy default, never panics
    }

    #[test]
    fn order_state_normalizes() {
        assert_eq!(normalize_order_state("PENDING"), OrderStatus::Accepted);
        assert_eq!(normalize_order_state("TRIGGERED"), OrderStatus::Accepted);
        assert_eq!(normalize_order_state("FILLED"), OrderStatus::Filled);
        assert_eq!(normalize_order_state("CANCELLED"), OrderStatus::Canceled);
        assert_eq!(normalize_order_state("whatever"), OrderStatus::Accepted);
    }

    #[test]
    fn timestamp_ms_converts_unix_seconds_string_to_millis() {
        let v = serde_json::json!({ "time": "1478012400.000000000" });
        assert_eq!(ts_ms(&v, "time"), 1_478_012_400_000);
        assert_eq!(ts_ms(&serde_json::json!({}), "time"), 0); // absent → 0
    }

    #[test]
    fn balance_reads_account_balance_as_string() {
        let v = serde_json::json!({ "account": { "balance": "100000.5000" } });
        assert_eq!(parse_balance(&v), Some(100000.5));
        assert_eq!(parse_balance(&serde_json::json!({ "account": {} })), None);
    }
}
