//! OKX `ReconClient` (Task 11 of the reconciliation-engine feature) — the venue-facing report
//! seam (`vike_exec::recon::ReconClient`) for the V5 SWAP perp, reconciling against:
//!
//!   `GET /api/v5/trade/orders-pending` (order reports — CURRENTLY open only, no time filter;
//!   mirrors `perp::OkxPerpRest`'s own `PATH_POSITIONS`/history endpoints).
//!   `GET /api/v5/trade/orders-algo-pending` (the same order report's second half: the resting
//!   conditional STOPS, which `orders-pending` is not documented to list; `ordType` is REQUIRED).
//!   `GET /api/v5/trade/fills` (fill reports; `begin` is the Unix-ms lower bound when `since > 0`).
//!   `GET /api/v5/trade/orders-algo-history` (`state=effective`, read only when a pass has fills: the
//!   stops that FIRED, whose `ordIdList` names the child order each one spawned. `trade/fills` rows
//!   carry the child's own `clOrdId` and no algo field, so this join files a fired stop's fill under
//!   the stop's client id, [`name_algo_children`]).
//!   `GET /api/v5/account/positions` (position reports — reuses the contracts→base conversion
//!   `perp::OkxPerpRest::reconcile_positions` already performs via `ct_val`).
//!
//! Every fetch delegates to a PURE free function (`parse_*`) over the raw JSON `data` array —
//! these are the fixture-tested units (`tests/offline/recon_client_parse.rs`, no network). Signed results
//! are unwrapped (`transport::unwrap_okx`) to the bare `data` array and re-serialized to a string
//! once, handed to the pure parser: the same small, deliberate cost `vike_binance::recon_client`
//! pays to keep every parser directly testable against a literal wire body.
//!
//! **Contracts → base (the tricky bit):** every OKX SWAP size field (`sz`/`accFillSz` on orders,
//! `fillSz` on fills, `pos` on positions) is quoted in CONTRACTS, not base units — see
//! `perp::OkxPerpRest::to_base` (`contracts * ct_val`, a plain float multiply). That shape was
//! PORTED from the retired Python app's `_to_base`; this line used to say it "matches" that
//! function, which nothing can check any more — no Python survives in this tree
//! (`docs/decisions/0021-python-oracle-retired-vike-is-the-reference.md`). What holds the multiply
//! today is the frozen `fixtures/r6/okx.json` replay in
//! `crates/bridges/okx/tests/offline/r6_okx_parity.rs`, whose `map_okx_open_order` case pins
//! `contracts × ct_val → base`. PRICE fields (`avgPx`/`fillPx`) are already quote-per-base and are
//! NEVER rescaled. `pos` on `/api/v5/account/positions` is ALREADY SIGNED (long > 0, short < 0),
//! same as Binance's `positionAmt` — never re-signed by `posSide`.
//!
//! **Flat positions:** unlike Binance's `positionRisk` (which always echoes every symbol,
//! including a `positionAmt: "0"` row), OKX's `/api/v5/account/positions` OMITS a symbol entirely
//! once its position is flat — no `pos: "0"` row is sent. `recon::diff::diff` iterates the REPORT
//! list to find a local position's counterpart, so a silently-omitted row would make "local still
//! shows a position OKX has since closed" invisible (exactly the gap `perp::OkxPerpRest::
//! reconcile_positions` already works around by synthesizing a flat `ReconcileSnapshot` when its
//! `legs` vec ends up empty — see its `if legs.is_empty()` branch). [`parse_positions`] mirrors
//! that: an empty `data` array synthesizes ONE flat (`qty: 0.0`) row for the queried symbol, so a
//! stale local position is still detectable. Any row OKX DOES send (even a defensive `pos: "0"`
//! one) is kept verbatim — never filtered — for the same reason Binance's parser keeps its flat
//! rows.
//!
//! **Admitted instruments (decisions 0173 and 0174):** an account's engine trades any SWAP of the
//! exchange, admitted on demand, and a restart must see the orders, fills and positions it left there.
//! The client is TOLD each admitted instrument (`ReconClient::admit_symbol`, a list bounded by the
//! engine's own cap) and every pass then reads the mounted instrument FIRST, with the requests it
//! always made and the `ctVal` it was built with, and each admitted one after it:
//!
//! * `orders-pending` and the paged `orders-algo-pending`, per instrument (`instId`);
//! * `trade/fills` per instrument, and the fired-stop naming per instrument that has fills (one
//!   failure count per PASS, however many instruments fail it);
//! * `positions`: ONE account-wide `instType=SWAP` read split per instrument client-side (the endpoint
//!   meters 10 requests per 2 seconds), a positive flat row for an instrument with no row.
//!
//! **Every size is scaled by the ROW'S OWN `ctVal`**: an admitted instrument's is read once from
//! `public/instruments` (`OkxReconClient::ct_val_of`) and kept, because a contract of another
//! instrument is another amount of the base asset. Any read failing fails the WHOLE fetch (the restore
//! planner reads an order a successful report lacks as gone), with ONE exception: an instrument the
//! venue no longer lists (`51001`, or no SWAP row) holds nothing and is read as such with a warning,
//! because the admitted list never shrinks and one delisted instrument must not blind the account.
//! `admitted_symbols` is therefore a real claim, and `vike_gatewayhub_client::trade::RECON_READS_ADMITTED`
//! lists okx.
//!
//! `OkxReconClient` reuses the SAME `OkxV5Signer`/`OkxTransport` seam as `perp::OkxPerpRest` (no
//! new HTTP/signing stack): `orders-pending`/`positions` ride the normal `signed` path (the same
//! one `perp::OkxPerpRest::call`/`reconcile_positions` already use for order/position reads),
//! while `fills` — a history-shaped scan, same class as Binance's `myTrades`/`userTrades` — rides
//! `signed_requery`, the SAME short-timeout, rate-gate-EXEMPT path `perp::OkxPerpRest::
//! get_orders_history`/`get_fills_history` already use for the crate's audit-A3 resync, so a
//! reconcile pass can't blow the per-IP order-rate budget submit/cancel depend on.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use vike_bridge_core::json::{json_int, json_num, parse_rows};
use vike_bridge_core::signer::OkxV5Signer;
use vike_exec::recon::{AdmittedSymbols, ReconClient};
use vike_model::events::{LiquiditySide, TradeId};
use vike_model::{
    FeeSchedule, FillReport, MarginMode, OrderStatus, OrderStatusReport, OrderType,
    PositionStatusReport,
};

use crate::perp;
use crate::transport::{OkxTransport, unwrap_okx};

const VENUE: &str = "okx";

pub const PATH_ORDERS_PENDING: &str = "/api/v5/trade/orders-pending";
pub const PATH_FILLS: &str = "/api/v5/trade/fills";
pub const PATH_TRADE_FEE: &str = "/api/v5/account/trade-fee";

/// The venue's code for an instrument id it does not list (`public/instruments` of a delisted or
/// unknown id). The one answer for which an ADMITTED instrument is read as holding nothing.
const INSTRUMENT_NOT_LISTED: i64 = 51001;

/// Rows asked per `orders-algo-pending` request: the venue's maximum (`limit` is at most 100, default
/// 100). Asked explicitly, so that a short page can only mean "the last one".
const ALGO_PAGE_LIMIT: usize = 100;

/// Pages the stop read follows before it FAILS: 40 x 100 = 4000 resting stops on ONE instrument. The
/// vendor's own cap on pending TP/SL orders is 100 per instrument (its trading-rules page), so a
/// second page is a safety net against that number being wrong, and this bound is for a venue that
/// never ends its chain.
const ALGO_MAX_PAGES: usize = 40;

/// How many passes in a row may fail the fill read over an unreadable stop history: strict, as the
/// `orders-algo-pending` read beside it. okx CLAIMS to list trigger orders (decision 0192), so a fill
/// list handed over without the names of a fired stop's fills would let the restore planner read a
/// stop that did its job as one the venue dropped. From the next failure on the pass goes ahead with
/// those fills unnamed (one plain WARN an hour), so an endpoint that stays broken cannot blind the
/// whole account (orders, positions, balance) for good. Binance's naming has the same three
/// (`crates/bridges/binance/src/algo.rs`'s `ALGO_NAMING_STRICT_FAILURES`).
const ALGO_NAMING_STRICT_FAILURES: u32 = 3;

/// How often the degraded pass repeats its one plain WARN.
const ALGO_NAMING_NOTICE_EVERY: Duration = Duration::from_secs(3600);

/// How the stop-history reads have been going across passes (the client lives for the process):
/// consecutive failing passes since the last good read, and when the WARN was last logged.
#[derive(Debug, Default)]
struct NamingState {
    failures: u32,
    noticed: Option<Instant>,
}

impl NamingState {
    /// A pass named what it needed (or had nothing to name): strictness comes back.
    fn succeeded(&mut self) {
        self.failures = 0;
    }

    /// A pass's history read failed with `why`. The first [`ALGO_NAMING_STRICT_FAILURES`] consecutive
    /// failures are `Err` (the fill read fails). After that the pass goes ahead unnamed: `Ok(true)`
    /// when its WARN is due, `Ok(false)` inside the hour of the last one.
    fn failed(&mut self, now: Instant, why: String) -> Result<bool, String> {
        self.failures = self.failures.saturating_add(1);
        if self.failures <= ALGO_NAMING_STRICT_FAILURES {
            return Err(why);
        }
        let due = self.noticed.is_none_or(|at| now.duration_since(at) >= ALGO_NAMING_NOTICE_EVERY);
        if due {
            self.noticed = Some(now);
        }
        Ok(due)
    }
}

// --- pure parsers --------------------------------------------------------------------------

fn side_sign(s: &str) -> i32 {
    if s.eq_ignore_ascii_case("buy") { 1 } else { -1 }
}

fn liquidity_side(exec_type: &str) -> LiquiditySide {
    if exec_type == "M" { LiquiditySide::Maker } else { LiquiditySide::Taker }
}

/// Map a raw OKX order `state` onto the FSM [`OrderStatus`] `recon::diff::diff` reads — every state
/// the `orders-pending` / order endpoints document, explicitly: `live` → `Accepted`;
/// `partially_filled` → `PartiallyFilled`; `filled` → `Filled`; `canceled` and `mmp_canceled` (pulled
/// by market-maker protection) → `Canceled`. `mmp_canceled` used to be upper-cased to a word outside
/// the FSM and then read as a LIVE order by every consumer.
///
/// A word outside that list is WARNED and read as `Accepted`: live, the conservative direction —
/// `diff` raises no `MissingTerminal` on it, adoption synthesizes no cumulative fill, and the restore
/// planner keeps the order rather than declaring it gone.
fn normalize_order_status(raw: &str) -> OrderStatus {
    match raw {
        "live" => OrderStatus::Accepted,
        "partially_filled" => OrderStatus::PartiallyFilled,
        "filled" => OrderStatus::Filled,
        "canceled" | "mmp_canceled" => OrderStatus::Canceled,
        other => {
            tracing::warn!(
                venue = VENUE,
                status = other,
                "unknown order state word — reporting the order as live (Accepted)"
            );
            OrderStatus::Accepted
        }
    }
}

/// Map a raw OKX `ordType` onto [`OrderType`]. OKX carries the time-in-force IN the order type, so
/// every resting-price kind is a `Limit` (`limit`, `post_only`, `fok`, `ioc` — the three the exec
/// builder sends for GTC/FOK/IOC — plus `mmp`, `mmp_and_post_only`, `op_fok` and `elp`); `market`
/// and `optimal_limit_ioc` (a futures/swap market order with a protective best-price limit) are
/// `Market`. Any other word has no twin and stays [`OrderType::Unrecognized`] lower-cased, as every
/// report spelled it before the type existed.
fn normalize_order_type(raw: &str) -> OrderType {
    match raw {
        "limit" | "post_only" | "fok" | "ioc" | "mmp" | "mmp_and_post_only" | "op_fok" | "elp" => {
            OrderType::Limit
        }
        "market" | "optimal_limit_ioc" => OrderType::Market,
        other => OrderType::Unrecognized(other.to_ascii_lowercase()),
    }
}

/// The client id a report row is filed under: `algoClOrdId` (the stop's, ours) on a fired stop's CHILD
/// order, else `clOrdId` — [`crate::event_mapper::row_coid`], the one reading of the pair.
fn client_order_id(v: &Value) -> Option<String> {
    Some(crate::event_mapper::row_coid(v)).filter(|c| !c.is_empty())
}

/// `GET /api/v5/trade/orders-pending` rows -> `OrderStatusReport`. `sz`/`accFillSz` are CONTRACTS
/// — rescaled to base via `ct_val` (see module doc). `avgPx` is already quote-per-base.
pub fn parse_orders_pending(body: &str, ct_val: f64) -> Result<Vec<OrderStatusReport>, String> {
    parse_rows(body, |o| {
        let symbol = o.get("instId").and_then(|s| s.as_str()).unwrap_or("").to_string();
        let venue_order_id = o.get("ordId").and_then(|s| s.as_str()).unwrap_or("").to_string();
        let side = side_sign(o.get("side").and_then(|s| s.as_str()).unwrap_or(""));
        let order_type =
            normalize_order_type(o.get("ordType").and_then(|t| t.as_str()).unwrap_or(""));
        let sz = o.get("sz").and_then(json_num).unwrap_or(0.0);
        let acc_fill_sz = o.get("accFillSz").and_then(json_num).unwrap_or(0.0);
        let avg_px = o.get("avgPx").and_then(json_num).unwrap_or(0.0);
        let raw_status = o.get("state").and_then(|s| s.as_str()).unwrap_or("");
        let ts = o.get("uTime").and_then(json_int).unwrap_or(0);
        OrderStatusReport {
            venue: VENUE.to_string(),
            symbol,
            venue_order_id: venue_order_id.into(),
            client_order_id: client_order_id(o),
            side,
            order_type,
            qty: sz * ct_val,
            filled_qty: acc_fill_sz * ct_val,
            avg_px,
            status: normalize_order_status(raw_status),
            ts,
        }
    })
}

/// `GET /api/v5/trade/orders-algo-pending` rows (untriggered `conditional` STOPS) ->
/// `OrderStatusReport`. The venue id is the `algoId`, the client id the `algoClOrdId`; `sz` is
/// CONTRACTS (rescaled by `ct_val`); a stop has no fill or average price yet. The list is untriggered
/// orders by definition, so a state outside `live`/`pause`/`canceled`/`order_failed` is WARNED and
/// read as live (`Accepted`) — the conservative direction, as [`normalize_order_status`].
///
/// `order_failed` (the trigger fired and the order it placed was refused: nothing rests, nothing
/// filled; the word `order-algo` showed on the demo account on 2026-10-24) is a TERMINAL `Rejected`,
/// never a live stop. The vendor lists only `live`/`pause` on this endpoint, so the arm is reached
/// only if the venue ever breaks that; the failed stop a cancel meets is `confirm_algo_gone`'s.
pub fn parse_algo_pending(body: &str, ct_val: f64) -> Result<Vec<OrderStatusReport>, String> {
    parse_rows(body, |o| {
        let text = |k: &str| o.get(k).and_then(|s| s.as_str()).unwrap_or("");
        let status = match text("state") {
            "canceled" => OrderStatus::Canceled,
            "order_failed" => OrderStatus::Rejected,
            "live" | "pause" => OrderStatus::Accepted,
            other => {
                tracing::warn!(
                    venue = VENUE,
                    state = other,
                    "unknown algo order state word — reporting the stop as live (Accepted)"
                );
                OrderStatus::Accepted
            }
        };
        OrderStatusReport {
            venue: VENUE.to_string(),
            symbol: text("instId").to_string(),
            venue_order_id: text("algoId").to_string().into(),
            client_order_id: Some(text("algoClOrdId")).filter(|c| !c.is_empty()).map(String::from),
            side: side_sign(text("side")),
            order_type: OrderType::Stop,
            qty: o.get("sz").and_then(json_num).unwrap_or(0.0) * ct_val,
            filled_qty: 0.0,
            avg_px: 0.0,
            status,
            ts: o.get("uTime").and_then(json_int).unwrap_or(0),
        }
    })
}

/// `GET /api/v5/trade/fills` rows -> `FillReport`. `fillSz` is CONTRACTS — rescaled to base via
/// `ct_val`. Commission: OKX `fee` is NEGATIVE for a charge, so `commission = -fee` (positive
/// cost / negative rebate) — same sign convention `event_mapper::map_okx_order` already applies
/// to the live WS fill stream.
///
/// A row with no `tradeId` is DROPPED, not reported. A `FillReport` with no `trade_id` is worse than
/// a missing one: `vike_exec::recon::diff` decides "already known" by looking the id up in
/// `seen_trade_ids`, so an id-less row can never match and instead FABRICATES a `MissingFill`
/// divergence — one of the two kinds the `hybrid` policy AUTO-APPLIES
/// (`vike_tradehub::reconcile_config::auto_applied_kinds`), i.e. it would book the fill a second time with
/// no operator in front of it. Dropping is the safe direction: the worst case is a real fill going
/// unreconciled until a pass sees it with its id. OKX documents `tradeId` on every `fills` row.
pub fn parse_fills(body: &str, ct_val: f64) -> Result<Vec<FillReport>, String> {
    // `flatten` is the row DROP — see this fn's doc for why reporting it would be worse.
    let rows = parse_rows(body, |f| {
        let symbol = f.get("instId").and_then(|s| s.as_str()).unwrap_or("").to_string();
        let trade_id = match TradeId::new(f.get("tradeId").and_then(|s| s.as_str()).unwrap_or("")) {
            Ok(id) => id,
            Err(_) => {
                tracing::warn!(
                    venue = VENUE,
                    symbol = %symbol,
                    "trade/fills row carries no `tradeId` — skipping the FillReport; an id-less \
                     row can never match `seen_trade_ids`, so it would fabricate a MissingFill \
                     divergence that `hybrid` auto-applies (booking the fill twice)"
                );
                return None;
            }
        };
        let venue_order_id = f.get("ordId").and_then(|s| s.as_str()).unwrap_or("").to_string();
        let side = side_sign(f.get("side").and_then(|s| s.as_str()).unwrap_or(""));
        let last_qty = f.get("fillSz").and_then(json_num).unwrap_or(0.0) * ct_val;
        let last_px = f.get("fillPx").and_then(json_num).unwrap_or(0.0);
        let commission = -f.get("fee").and_then(json_num).unwrap_or(0.0);
        let commission_asset = f.get("feeCcy").and_then(|c| c.as_str()).unwrap_or("").to_string();
        let exec_type = f.get("execType").and_then(|e| e.as_str()).unwrap_or("");
        let ts = f.get("ts").and_then(json_int).unwrap_or(0);
        Some(FillReport {
            venue: VENUE.to_string(),
            symbol,
            trade_id,
            venue_order_id: venue_order_id.into(),
            client_order_id: client_order_id(f),
            side,
            last_qty,
            last_px,
            commission,
            commission_asset,
            liquidity_side: liquidity_side(exec_type),
            ts,
        })
    })?;
    Ok(rows.into_iter().flatten().collect())
}

/// `GET /api/v5/trade/orders-algo-history` rows (`state=effective`: the stops that FIRED) -> the child
/// order each one spawned: the child's `ordId` (the row's `ordIdList`, plus its `ordId`, "latest order
/// id") -> the stop's `algoClOrdId`. A `trade/fills` row names its order by `ordId` and carries no
/// algo field, so this is the join that files a fired stop's fill under the stop's own client id
/// ([`name_algo_children`]). A row with no `algoClOrdId` (a stop placed by hand) names nothing.
pub fn parse_algo_children(body: &str) -> Result<HashMap<String, String>, String> {
    let rows: Vec<Vec<(String, String)>> = parse_rows(body, |o| {
        let coid = o.get("algoClOrdId").and_then(Value::as_str).unwrap_or("");
        let ids = o
            .get("ordIdList")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .chain(o.get("ordId").and_then(Value::as_str))
            .filter(|id| !id.is_empty());
        ids.filter(|_| !coid.is_empty()).map(|id| (id.to_string(), coid.to_string())).collect()
    })?;
    Ok(rows.into_iter().flatten().collect())
}

/// File each fill whose order is a fired stop's child under the STOP's client id. `trade/fills` carries
/// the child's own `clOrdId` (`O4038740198576748544`, the venue's), which no order of ours has, so
/// unnamed the fill is a `MissingFill` for an order the book never heard of; named, it joins the
/// stop. Fills of any other order are left as they were.
pub fn name_algo_children(fills: &mut [FillReport], children: &HashMap<String, String>) {
    for fill in fills {
        if let Some(coid) = children.get(fill.venue_order_id.as_str()) {
            fill.client_order_id = Some(coid.clone());
        }
    }
}

/// `GET /api/v5/account/positions` rows -> `PositionStatusReport`. `pos` is ALREADY SIGNED
/// CONTRACTS (long > 0, short < 0 in hedge mode; signed net in one-way mode) — rescaled to base
/// via `ct_val`, never re-signed by `posSide`. An empty `data` array synthesizes ONE flat row for
/// `symbol` (see module doc — OKX omits flat positions rather than sending a `pos: "0"` row); any
/// row OKX DOES send (including a defensive `pos: "0"`) is kept, not filtered.
pub fn parse_positions(
    body: &str,
    symbol: &str,
    ct_val: f64,
) -> Result<Vec<PositionStatusReport>, String> {
    let rows = parse_rows(body, |p| {
        let row_symbol = p.get("instId").and_then(|s| s.as_str()).unwrap_or("").to_string();
        let side = crate::event_mapper::pside(p);
        let contracts = p.get("pos").and_then(json_num).unwrap_or(0.0); // already signed
        let avg_px = p.get("avgPx").and_then(json_num).unwrap_or(0.0);
        let ts = p.get("uTime").and_then(json_int).unwrap_or(0);
        let (margin_mode, isolated_margin) = parse_mgn_mode(p);
        PositionStatusReport {
            venue: VENUE.to_string(),
            symbol: row_symbol,
            position_side: side,
            qty: contracts * ct_val,
            avg_px,
            ts,
            margin_mode,
            isolated_margin,
            delta: None,
        }
    })?;
    if rows.is_empty() {
        return Ok(vec![PositionStatusReport::flat(VENUE, symbol)]);
    }
    Ok(rows)
}

/// OKX position-row margin-mode read (margin-mode step-2, read-side only), shared by
/// [`parse_positions`] and `perp::OkxPerpRest::reconcile_positions`'s snapshot builder:
/// `mgnMode` `"isolated"` ⇒ `Isolated` + the position's `margin` balance (OKX documents `margin`
/// as "margin balance, applicable to isolated" — an empty string on cross rows, which `json_num`
/// already yields `None` for); anything else — `"cross"` or an absent field — is the fail-safe
/// `Cross`/`None`, byte-identical to pre-field behavior. The REQUEST side now honors
/// `OrderRequest.margin_mode` via `perp::swap_td_mode` (unset = the historical `"cross"`).
pub fn parse_mgn_mode(p: &Value) -> (MarginMode, Option<f64>) {
    let isolated = p
        .get("mgnMode")
        .and_then(|m| m.as_str())
        .is_some_and(|m| m.eq_ignore_ascii_case("isolated"));
    if isolated {
        (MarginMode::Isolated, p.get("margin").and_then(json_num))
    } else {
        (MarginMode::Cross, None)
    }
}

/// `GET /api/v5/account/balance` `data[].details[]` rows -> the account's USDT `cashBal` (total
/// cash — free + frozen; NOT `availBal`, which excludes funds currently locked by open orders/
/// positions). Same field `perp::OkxPerpRest::fetch_usdt_balance` already extracts for the legacy
/// `ReconcileSnapshot.balance` — factored out here as the pure, fixture-tested parser this
/// `ReconClient::fetch_balance` override delegates to. `None` when no USDT detail row is present;
/// malformed JSON (not an array) is a hard `Err`, never a panic.
pub fn parse_balance(body: &str) -> Result<Option<f64>, String> {
    let v: Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    let rows = v.as_array().ok_or_else(|| "expected a JSON array".to_string())?;
    for acct in rows {
        if let Some(details) = acct.get("details").and_then(|d| d.as_array())
            && let Some(row) =
                details.iter().find(|d| d.get("ccy").and_then(|c| c.as_str()) == Some("USDT"))
        {
            return Ok(row.get("cashBal").and_then(json_num));
        }
    }
    Ok(None)
}

/// `GET /api/v5/account/trade-fee` `data[0]` -> live [`FeeSchedule`]. OKX reports fee rates
/// **negative for a charge** (their sign convention: negative = fee-you-pay, positive = rebate),
/// so the parser NEGATES — the same `commission = -fee` normalization [`parse_fills`] already
/// applies to the live fill stream — leaving `commission > 0` a cost, `< 0` a rebate. For a
/// USDT-margined SWAP the effective rate is `makerU`/`takerU` (falls back to `maker`/`taker` when
/// those are absent/empty). `Ok(None)` when no row/rate is present (fail-soft → static default).
pub fn parse_fee_rate(body: &str) -> Result<Option<FeeSchedule>, String> {
    let v: Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    let rows = v.as_array().ok_or_else(|| "expected a JSON array".to_string())?;
    Ok(rows.first().and_then(|r| {
        let maker_raw =
            r.get("makerU").and_then(json_num).or_else(|| r.get("maker").and_then(json_num))?;
        let taker_raw =
            r.get("takerU").and_then(json_num).or_else(|| r.get("taker").and_then(json_num))?;
        Some(FeeSchedule::from_fractions(-maker_raw, -taker_raw))
    }))
}

// --- the factory -------------------------------------------------------------------------------

/// The venue -> `ReconClient` factory (ReconFactory seam, wave-2 task 6) — moved verbatim from
/// `vike_tradehub_core::build_recon_client`'s `"okx"` match arm (called from `crate::adapter` since
/// docs/decisions/0096): a FRESH stateless-HMAC `ureq` REST client dedicated to reconcile reads,
/// reusing the SAME `Credentials` the venue's `ExecutionClient`/`spawn_with_recorder` already
/// builds from. `ct_val` is the caller's already-fetched (or fallback) contracts->base scale
/// factor, threaded in rather than re-fetched here — see `crate::adapter`'s `OkxVenueAdapter::start`,
/// which fetches it once and reuses it for both the exec grid and this recon client. Uses the
/// shared host (`perp::REST` — demo and mainnet share it), with the `x-simulated-trading` header
/// selected by `mainnet` — the SAME already-resolved ceiling verdict (decision 0095: `live` means
/// mainnet) the start threaded into the exec path, resolved ONCE there, so reconcile stays in
/// lockstep with exec instead of re-reading anything for itself. Always `Some` — construction here
/// is pure/infallible (no network); `Option` is kept so the return type matches every other
/// bridge's `recon_client` (deribit's genuinely can fail).
pub fn recon_client(
    creds: &vike_secrets::credentials::Credentials,
    symbol: &str,
    ct_val: f64,
    mainnet: bool,
) -> Option<Box<dyn ReconClient>> {
    Some(Box::new(OkxReconClient::new(
        OkxV5Signer::new(creds, vike_model::now_ms),
        // Reconcile reads follow the SAME resolved verdict the exec path binds to: the shared REST
        // host stays, but the `x-simulated-trading` header is dropped on mainnet. `false` ⇒ demo
        // (header present), byte-identical to before the switch existed.
        crate::transport::UreqOkxTransport::new(crate::perp::simulated(mainnet))
            .with_rate_gate(crate::ratelimit::rest_rate_gate()),
        crate::perp::REST,
        symbol.to_string(),
        ct_val,
    )))
}

// --- the client --------------------------------------------------------------------------------

/// One reconcile client per venue-symbol — the SAME per-symbol shape `perp::OkxPerpRest` uses,
/// but carrying only what a report fetch needs (signer + transport + base_url + symbol + ct_val;
/// no order-formatting `SymbolProperties`/leverage) — and, since the engine admits instruments on
/// demand (decision 0173), every instrument it was told about (`ReconClient::admit_symbol`), each read
/// with ITS OWN `ctVal` (see the module doc's "Admitted instruments").
pub struct OkxReconClient<T: OkxTransport> {
    signer: OkxV5Signer,
    transport: T,
    base_url: String,
    /// instId form: BTC-USDT-SWAP
    symbol: String,
    /// base units per contract — the contracts→base rescale factor (see module doc).
    ct_val: f64,
    /// The instruments the engine admitted beyond `symbol`, as OKX spells them (upper-case `instId`).
    admitted: AdmittedSymbols,
    /// `ctVal` of each admitted instrument already asked for (a row's contract size does not change
    /// for the life of the client). Only a usable linear row is kept, never a failure.
    ct_vals: Mutex<HashMap<String, f64>>,
    /// **The account id, remembered after the first ask.**
    ///
    /// ⚠ Unlike binance, nothing here can fill it as a side effect: no other call on this client
    /// reads `/api/v5/account/config`, so the first `fetch_account_identity` IS the request. The
    /// cache therefore saves the REPEATS — a reconcile pass asking again — rather than the first
    /// call. `vike_exec::recon::ReconClient::fetch_account_identity` carries why this is the one
    /// value on the trait that may be cached at all.
    identity: std::sync::OnceLock<Option<String>>,
    /// How the stop-history reads have been going across passes ([`NamingState`]).
    naming: Mutex<NamingState>,
}

impl<T: OkxTransport> OkxReconClient<T> {
    pub fn new(
        signer: OkxV5Signer,
        transport: T,
        base_url: impl Into<String>,
        symbol: impl Into<String>,
        ct_val: f64,
    ) -> Self {
        OkxReconClient {
            signer,
            transport,
            base_url: base_url.into(),
            symbol: symbol.into(),
            ct_val,
            admitted: AdmittedSymbols::default(),
            ct_vals: Mutex::new(HashMap::new()),
            identity: std::sync::OnceLock::new(),
            naming: Mutex::new(NamingState::default()),
        }
    }

    /// `ctVal` of the admitted instrument `inst` (upper-case): the cached one, else ONE public
    /// `instruments` read. `Ok(None)` = the venue does not list it as a SWAP any more (code `51001`
    /// or no row): it holds nothing, so the pass reads it as such with a warning and the account's
    /// other reads still go through (the admitted list never shrinks, so a delisted instrument must not
    /// blind the pass for good). `Err` = the read failed, or the row is one this adapter does not size
    /// (inverse, `ctMult` other than 1, no usable `ctVal`; the door never admits one, so it is a venue
    /// change): the whole fetch fails, because the restore planner reads an order a report lacks as gone.
    fn ct_val_of(&self, inst: &str) -> Result<Option<f64>, String> {
        if let Some(known) = self.ct_vals.lock().unwrap_or_else(PoisonError::into_inner).get(inst) {
            return Ok(Some(*known));
        }
        let params = [("instType", "SWAP".to_string()), ("instId", inst.to_string())];
        let data = match self
            .transport
            .public(&self.base_url, perp::PATH_INSTRUMENTS, &params)
            .and_then(unwrap_okx)
        {
            Ok(data) => data,
            Err(e) if e.code == INSTRUMENT_NOT_LISTED => return Ok(None),
            Err(e) => return Err(format!("public/instruments for {inst} could not be read: {e}")),
        };
        let Some(row) =
            perp::parse_okx_perp_instruments(&json!({ "data": data })).get(inst).cloned()
        else {
            return Ok(None);
        };
        if let Some(why) = crate::exec::inverse_swap_refusal(inst, Some(&row)) {
            return Err(why);
        }
        let usable = row.ct_val.is_finite() && row.ct_val > 0.0;
        let multiplied =
            row.ct_mult.is_finite() && row.ct_mult > 0.0 && (row.ct_mult - 1.0).abs() > 1e-12;
        if !usable || multiplied {
            return Err(format!(
                "{inst}: no usable contract size (ctVal {}, ctMult {}); its reports cannot be scaled",
                row.ct_val, row.ct_mult
            ));
        }
        self.ct_vals
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(inst.to_string(), row.ct_val);
        Ok(Some(row.ct_val))
    }

    /// Every instrument this pass reads with the `ctVal` its contract counts are scaled by: the mounted
    /// one FIRST (the value the client was built with, never fetched), then each admitted one. With
    /// nothing admitted this is the mounted instrument alone and no request is made.
    fn read_set(&self) -> Result<Vec<(String, f64)>, String> {
        let mut set = vec![(self.symbol.clone(), self.ct_val)];
        for inst in self.admitted.symbols() {
            match self.ct_val_of(&inst)? {
                Some(ct_val) => set.push((inst, ct_val)),
                None => tracing::warn!(
                    venue = VENUE, symbol = %inst,
                    "reconcile: okx does not list this admitted instrument as a SWAP; reading it as \
                     holding nothing"
                ),
            }
        }
        Ok(set)
    }

    /// The normal signed GET path — the same one `perp::OkxPerpRest::call`/`reconcile_positions`
    /// already use for order/position reads. Returns the unwrapped `data` array, re-serialized.
    fn signed(&self, path: &str, params: &[(&str, Value)]) -> Result<String, String> {
        self.transport
            .signed(&self.base_url, path, "GET", params, &self.signer)
            .and_then(unwrap_okx)
            .map(|v| v.to_string())
            .map_err(|e| e.to_string())
    }

    /// The short-timeout, rate-gate-EXEMPT path — the SAME one `perp::OkxPerpRest::
    /// get_orders_history`/`get_fills_history` already use for the crate's audit-A3 resync, so a
    /// reconcile pass over the heavier fills scan can't blow the per-IP order-rate budget.
    fn signed_requery(&self, path: &str, params: &[(&str, Value)]) -> Result<String, String> {
        self.transport
            .signed_requery(&self.base_url, path, "GET", params, &self.signer)
            .and_then(unwrap_okx)
            .map(|v| v.to_string())
            .map_err(|e| e.to_string())
    }

    fn naming(&self) -> MutexGuard<'_, NamingState> {
        self.naming.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The fired stops of `inst` and the child order each spawned (`orders-algo-history`,
    /// `state=effective`, the newest [`ALGO_PAGE_LIMIT`] stops of the instrument): the join
    /// [`name_algo_children`] applies. The `Err` is the sentence the strict policy below reports.
    fn stop_children(&self, inst: &str) -> Result<HashMap<String, String>, String> {
        let params = vec![
            ("ordType", json!("conditional")),
            ("state", json!("effective")),
            ("instId", json!(inst)),
            ("limit", json!(ALGO_PAGE_LIMIT.to_string())),
        ];
        self.signed_requery(perp::PATH_ORDERS_ALGO_HISTORY, &params)
            .and_then(|body| parse_algo_children(&body))
            .map_err(|why| {
                format!(
                    "orders-algo-history (the fired stops) could not be read: {why}; the fills \
                     are refused rather than reported under the venue's child order ids"
                )
            })
    }

    /// File the fills of fired stops' CHILD orders under the stop's own client id
    /// ([`name_algo_children`]): one [`Self::stop_children`] read per instrument that HAS fills this pass,
    /// none for a pass with no fills.
    ///
    /// STRICT for the first [`ALGO_NAMING_STRICT_FAILURES`] failing passes since the last good read:
    /// okx claims to list trigger orders, so a fill list handed over with a fired stop's fills still
    /// under the venue's child order id would let the restore planner read a stop that did its job as
    /// gone after the quiet passes. The failure fails the fill read, which skips this venue's leg for
    /// the pass (like a failed `orders-algo-pending`); from the next failure on the pass goes ahead
    /// with those fills unnamed and ONE plain WARN an hour, so a broken endpoint cannot blind the
    /// account for good. The policy counts PASSES: however many instruments' reads fail in one pass,
    /// that is one failure (the first one's sentence). ⚠ RESIDUAL, as on Binance: while degraded, a stop
    /// that fired and filled can be read as cancelled after the quiet passes, and a stop older than the
    /// newest hundred fired ones stays unnamed even on a good read (no lookup of the child order by id,
    /// as Binance has).
    fn name_stop_children(&self, per_inst: &mut [(String, Vec<FillReport>)]) -> Result<(), String> {
        let mut failure: Option<String> = None;
        for (inst, fills) in per_inst.iter_mut().filter(|(_, fills)| !fills.is_empty()) {
            match self.stop_children(inst) {
                Ok(children) => name_algo_children(fills, &children),
                Err(why) => {
                    failure.get_or_insert(why);
                }
            }
        }
        let Some(why) = failure else {
            self.naming().succeeded();
            return Ok(());
        };
        if self.naming().failed(Instant::now(), why)? {
            tracing::warn!(
                venue = VENUE,
                "orders-algo-history has stayed unreadable for more than \
                 {ALGO_NAMING_STRICT_FAILURES} passes: the fills of a stop that fired are \
                 reported under the venue's own child order id until it reads again"
            );
        }
        Ok(())
    }

    /// `orders-algo-pending` for ONE instrument, EVERY page of it: the venue lists at most
    /// [`ALGO_PAGE_LIMIT`] rows a request, and `after` (here the `algoId` of the last row seen) asks
    /// for the records EARLIER than it (the vendor's words). The restore planner reads an order a
    /// successful report lacks as gone, so a walk that stopped short must FAIL, never return what it
    /// has: the `ReconClient` seam has no "this report may be truncated" claim (decision 0192 names
    /// `maybe_truncated` as a reopener), hence the error.
    ///
    /// ⚠ NOT measured on a venue: that the rows come newest first (the last row of a page is then the
    /// oldest, the cursor the next page needs) and that a chain of more than one page follows
    /// `after` as documented; only the single-page answer was. The repeated-`algoId` failure below
    /// is what keeps a wrong guess from passing as a complete list.
    ///
    /// The walk ends on a SHORT page (fewer rows than asked). It fails on: a page past
    /// [`ALGO_MAX_PAGES`]; an `algoId` that comes back a second time (a venue that ignored `after`,
    /// or sorts the other way, would otherwise yield a "complete" list with rows missing); a FULL
    /// page whose last row carries no `algoId` (nothing to ask `after` from); and any page failing.
    fn resting_stops(&self, inst: &str, ct_val: f64) -> Result<Vec<OrderStatusReport>, String> {
        let mut stops: Vec<OrderStatusReport> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for _ in 0..ALGO_MAX_PAGES {
            let mut params = vec![
                ("ordType", json!("conditional")),
                ("instId", json!(inst)),
                ("limit", json!(ALGO_PAGE_LIMIT.to_string())),
            ];
            if let Some(last) = stops.last() {
                params.push(("after", json!(last.venue_order_id.as_str())));
            }
            let body = self.signed(perp::PATH_ORDERS_ALGO_PENDING, &params)?;
            let page = parse_algo_pending(&body, ct_val)?;
            let full = page.len() >= ALGO_PAGE_LIMIT;
            if let Some(again) = page
                .iter()
                .map(|s| s.venue_order_id.as_str())
                .find(|id| !id.is_empty() && !seen.insert(id.to_string()))
            {
                return Err(format!(
                    "algoId {again} came back on a second page after {} rows: the read is refused \
                     rather than reported as complete",
                    stops.len()
                ));
            }
            stops.extend(page);
            if !full {
                return Ok(stops);
            }
            if stops.last().is_some_and(|s| s.venue_order_id.is_empty()) {
                return Err(format!(
                    "a full page of {} rows ends in a row with no algoId, so there is no cursor to \
                     ask the next page from: the read is refused rather than reported as complete",
                    stops.len()
                ));
            }
        }
        Err(format!(
            "still paging after {ALGO_MAX_PAGES} pages of {ALGO_PAGE_LIMIT} rows: the read is \
             refused rather than reported as complete"
        ))
    }
}

impl<T: OkxTransport + Send> ReconClient for OkxReconClient<T> {
    /// `since` is a no-op here: `orders-pending` has no time filter and reports only the
    /// CURRENTLY open set — a closed order that fell out of this set is exactly what the fill/
    /// position reports (not this one) reconcile (mirrors Binance's `openOrders`).
    ///
    /// **Resting STOPS are a second read** (`orders-algo-pending`, `ordType=conditional`): `orders-pending`
    /// is not documented to list algo orders, and a report without them would have the restore planner
    /// declare every resting stop `gone`. The algo read is STRICT — a failed one fails the whole report,
    /// so the planner never reads a hidden stop as gone.
    ///
    /// ⚠ **Strict on purpose, and the price is real.** The seam has no "partial listing" channel:
    /// `crates/vike-exec/src/recon/restore.rs`'s `plan_restore` trusts that a fetch it is handed
    /// SUCCEEDED and reads an order the report lacks as gone, so ordinary reports without the stops
    /// would retire a stop that still rests. A failed algo read therefore skips this venue's whole
    /// leg (fills, positions and the balance seed too, `crates/vike-tradehub-core/src/recon_manager/manager.rs`'s
    /// `run_pass_at`), with the error below as the reason. The request shape (`ordType` + `instId`, no
    /// `instType`) was accepted by the demo on 2026-10-23: the smoke listed its stop with that shape
    /// (and panics on a refusal); the stop read now also sends `limit=100`, a documented optional
    /// parameter the demo has not yet seen on this path. It PAGES (`resting_stops`): a chain that
    /// cannot finish fails the whole report, like a failed page.
    ///
    /// **Every admitted instrument is read the same way** (decision 0173; `read_set`): the mounted
    /// instrument first, then each one the engine was told, one `orders-pending` and one paged stop read
    /// apiece, each report scaled by ITS instrument's `ctVal`. Any read failing fails the whole report.
    fn fetch_order_status_reports(&self, _since: i64) -> Result<Vec<OrderStatusReport>, String> {
        let mut reports = Vec::new();
        for (inst, ct_val) in self.read_set()? {
            let body = self.signed(PATH_ORDERS_PENDING, &[("instId", json!(inst))])?;
            reports.extend(parse_orders_pending(&body, ct_val)?);
            let stops = self.resting_stops(&inst, ct_val).map_err(|e| {
                format!("orders-algo-pending (the resting stops) could not be listed: {e}")
            })?;
            reports.extend(stops);
        }
        Ok(reports)
    }

    /// One `trade/fills` read per instrument (`read_set`), each scaled by its own `ctVal`; the
    /// fired-stop naming follows once for the pass (`name_stop_children`).
    fn fetch_fill_reports(&self, since: i64) -> Result<Vec<FillReport>, String> {
        let mut per_inst = Vec::new();
        for (inst, ct_val) in self.read_set()? {
            let mut params: Vec<(&str, Value)> = vec![("instId", json!(inst))];
            if since > 0 {
                params.push(("begin", json!(since.to_string())));
            }
            let body = self.signed_requery(PATH_FILLS, &params)?;
            per_inst.push((inst, parse_fills(&body, ct_val)?));
        }
        self.name_stop_children(&mut per_inst)?;
        Ok(per_inst.into_iter().flat_map(|(_, fills)| fills).collect())
    }

    /// With nothing admitted, the one `positions` read of the mounted instrument it always was. With
    /// admitted instruments, ONE account-wide `instType=SWAP` read, split per instrument client-side:
    /// the endpoint meters 10 requests per 2 seconds, which a read per instrument (up to seventeen)
    /// would overrun. A row of an instrument the engine did not admit is dropped, as the
    /// instrument-filtered read never returned it; an instrument with no row gets the positive flat row
    /// ([`parse_positions`]), so a position the venue closed is still seen, and so does an admitted
    /// instrument the venue no longer lists (it holds nothing).
    fn fetch_position_status_reports(&self) -> Result<Vec<PositionStatusReport>, String> {
        let admitted = self.admitted.symbols();
        if admitted.is_empty() {
            let body = self.signed(
                perp::PATH_POSITIONS,
                &[("instType", json!("SWAP")), ("instId", json!(self.symbol))],
            )?;
            return parse_positions(&body, &self.symbol, self.ct_val);
        }
        let set = self.read_set()?;
        let body = self.signed(perp::PATH_POSITIONS, &[("instType", json!("SWAP"))])?;
        let rows: Vec<Value> = serde_json::from_str(&body).map_err(|e| e.to_string())?;
        let mut reports = Vec::new();
        for (inst, ct_val) in &set {
            let mine: Vec<&Value> = rows
                .iter()
                .filter(|r| {
                    r.get("instId")
                        .and_then(Value::as_str)
                        .is_some_and(|s| s.eq_ignore_ascii_case(inst))
                })
                .collect();
            let mine = serde_json::to_string(&mine).map_err(|e| e.to_string())?;
            reports.extend(parse_positions(&mine, inst, *ct_val)?);
        }
        for gone in admitted.iter().filter(|a| !set.iter().any(|(inst, _)| inst == *a)) {
            reports.push(PositionStatusReport::flat(VENUE, gone));
        }
        Ok(reports)
    }

    /// Balance meaning (pinned per the task brief): the account's USDT `cashBal` from
    /// `/api/v5/account/balance` — same field `perp::fetch_usdt_balance` uses (total cash, not
    /// available). A light, unfiltered account read — rides the normal `signed` (rate-gated)
    /// path, same as `orders-pending`/`positions` above.
    fn fetch_balance(&self) -> Result<Option<f64>, String> {
        let body = self.signed(perp::PATH_ACCOUNT, &[])?;
        parse_balance(&body)
    }

    /// Live account-actual fee rates (fee model 4/5): `/api/v5/account/trade-fee` for the SWAP
    /// instrument class — negated per OKX's sign convention (see [`parse_fee_rate`]). A light,
    /// unfiltered account read — rides the normal `signed` (rate-gated) path. `Ok(None)` keeps
    /// the static default.
    ///
    /// ⚠ **`instType` ALONE — passing `instId` alongside it is a 400 on this endpoint.** OKX
    /// applies `instId` to SPOT/MARGIN only; combined with `instType=SWAP` it rejects the pair
    /// outright rather than ignoring the narrower key. MEASURED against the live demo endpoint
    /// (`x-simulated-trading: 1`) while bringing up the first real tradehub CEX start:
    ///
    /// ```text
    /// GET /api/v5/account/trade-fee?instType=SWAP&instId=BTC-USDT-SWAP
    ///   -> HTTP 400  {"code":"50016","msg":"instId and instType don't match","data":[]}
    /// GET /api/v5/account/trade-fee?instType=SWAP
    ///   -> HTTP 200  {"code":"0", "data":[{"instType":"SWAP","maker":"-0.0002","taker":"-0.0005",
    ///                                      "makerU":"-0.0002","takerU":"-0.0005",...}]}
    /// ```
    ///
    /// The failure was SILENT in the way that matters: `fetch_fee_rates` is a fail-soft read, so the
    /// error fell through to the static default fee schedule and every reconcile pass kept working
    /// while never once using the account's real rates. A maker strategy sized against the wrong
    /// fee is the whole edge on this venue.
    ///
    /// This does NOT narrow the answer. The rates returned are the account's SWAP tier, and
    /// [`parse_fee_rate`] reads `makerU`/`takerU` — the USDT-margined rates, which is what
    /// `BTC-USDT-SWAP` settles in. ⚠ The sibling `fetch_position_status_reports` sends the SAME pair
    /// and is CORRECT: `/api/v5/account/positions` does accept `instType` + `instId` together
    /// (verified `code: 0` in the same session). The rule is per-endpoint, not per-venue — do not
    /// "fix" that one by symmetry.
    fn fetch_fee_rates(&self) -> Result<Option<FeeSchedule>, String> {
        let body = self.signed(PATH_TRADE_FEE, &[("instType", json!("SWAP"))])?;
        parse_fee_rate(&body)
    }

    /// **Tell this client the engine admitted `symbol`** (decision 0173): the next pass reads that
    /// instrument too, with its own `ctVal` (the module doc's "Admitted instruments"). Only the
    /// list is touched here; the `ctVal` read is the pass's, never this call's (it may come from a
    /// thread that must not wait on the venue).
    fn admit_symbol(&self, symbol: &str) {
        self.admitted.admit(symbol, &self.symbol);
    }

    /// **The instruments this client reads beyond the mounted one**: every one it was told (the list is
    /// bounded by the engine's own cap). A CLAIM, so it names only what a pass really reads. The restore
    /// planner may retire a resting order on a claimed instrument as "gone" when the report lacks it,
    /// which is right only because [`Self::fetch_order_status_reports`] reads that instrument.
    fn admitted_symbols(&self) -> Option<Vec<String>> {
        Some(self.admitted.symbols())
    }

    /// **CLAIMS that its open-order read lists the venue's trigger orders** (decision 0192's kind claim:
    /// the restore planner may call an absent stop gone, after the quiet passes, only for a client that
    /// says so). It does: `fetch_order_status_reports` reads `orders-algo-pending` (STRICT, and paged
    /// by `resting_stops`: a failed or unfinished read fails the report, never returns a short one) and
    /// types every row `OrderType::Stop`, the only trigger kind okx's capability row lets `submit`
    /// place. The other half of the proof is the fill side: a stop that FIRED while the node was down
    /// is not pending, and its child's fills are named by the stop's own id (`name_stop_children`,
    /// strict for three failing passes), so a stop that did its job is `filled_while_down` and not
    /// silence. The harness row proves the parsed stop
    /// (`crates/vike-bridge-core/tests/bridge_conformance/venues.rs`'s `okx_algo_open_rows`); the node
    /// answers from `RECON_LISTS_TRIGGER_ORDERS`, which a harness test holds equal to the proven claimants.
    fn lists_trigger_orders(&self) -> bool {
        true
    }

    /// **Which okx account is this key?** — `/api/v5/account/config`'s `uid`, asked once per client.
    ///
    /// ⚠ **The venue answers a SECOND field this seam cannot carry.** `mainUid` is the parent, and
    /// [`crate::account_config::AccountIdentity::is_sub_account`] is the only thing in this tree
    /// that can tell an okx sub-account from its master. The trait carries ONE string, and the one
    /// it carries is the BOOK — `uid`, *this* account, the value that belongs in
    /// `vike_secrets::Account::venue_account_id`. A caller that needs the parentage calls
    /// [`crate::account_config::account_identity`] directly rather than widening this.
    ///
    /// The cache is the one the trait permits and nothing more: an ERROR is never remembered, so an
    /// expired key is re-discovered on the next pass and at every restart.
    fn fetch_account_identity(&self) -> Result<Option<String>, String> {
        if let Some(cached) = self.identity.get() {
            return Ok(cached.clone());
        }
        let uid =
            crate::account_config::account_identity(&self.transport, &self.signer, &self.base_url)
                .map_err(|e| e.to_string())?
                .map(|id| id.uid);
        // `set` races only with another thread asking at the same moment, and both computed the
        // same answer from the same endpoint — so a loser is a discarded duplicate, never a
        // disagreement. Read the winner back rather than returning the local value, so every
        // caller in that race sees ONE id.
        let _ = self.identity.set(uid);
        Ok(self.identity.get().cloned().flatten())
    }
}

#[cfg(test)]
mod identity_tests {
    use super::*;
    use vike_bridge_core::transport::VenueApiError;
    use vike_secrets::credentials::Credentials;

    /// A transport that RECORDS every signed path. The recording is the whole test: the claim
    /// `fetch_account_identity` makes is *one request per client*, and only a count can check it.
    struct CountingTransport {
        paths: std::sync::Mutex<Vec<String>>,
        answer: Value,
    }

    impl CountingTransport {
        fn new(answer: Value) -> Self {
            CountingTransport { paths: std::sync::Mutex::new(Vec::new()), answer }
        }
        fn paths(&self) -> Vec<String> {
            self.paths.lock().expect("not poisoned").clone()
        }
    }

    impl OkxTransport for CountingTransport {
        fn signed(
            &self,
            _base_url: &str,
            path: &str,
            _method: &str,
            _params: &[(&str, Value)],
            _signer: &OkxV5Signer,
        ) -> Result<Value, VenueApiError> {
            self.paths.lock().expect("not poisoned").push(path.to_string());
            Ok(self.answer.clone())
        }

        fn public(
            &self,
            _base_url: &str,
            _path: &str,
            _params: &[(&str, String)],
        ) -> Result<Value, VenueApiError> {
            unreachable!(
                "the identity read is SIGNED — a public call here would be a wrong endpoint"
            )
        }
    }

    fn client(answer: Value) -> OkxReconClient<CountingTransport> {
        let creds = Credentials {
            api_key: "k".to_string().into(),
            api_secret: "s".to_string().into(),
            passphrase: Some("p".to_string().into()),
        };
        OkxReconClient::new(
            OkxV5Signer::new(&creds, || 0),
            CountingTransport::new(answer),
            "https://example.invalid",
            "BTC-USDT-SWAP",
            0.01,
        )
    }

    fn envelope(rows: Value) -> Value {
        json!({"code": "0", "msg": "", "data": rows})
    }

    /// ⚠ **THE CLAIM THIS METHOD MAKES.** Two asks, ONE signed request — and it is
    /// `/api/v5/account/config`, the endpoint nothing else on this client touches, so the count is
    /// unambiguous about which call was saved.
    #[test]
    fn a_second_ask_costs_no_second_request() {
        let c = client(envelope(json!([{"uid": "42", "mainUid": "42"}])));
        assert_eq!(c.fetch_account_identity(), Ok(Some("42".into())));
        assert_eq!(c.fetch_account_identity(), Ok(Some("42".into())));
        assert_eq!(
            c.transport.paths(),
            vec![crate::account_config::PATH_ACCOUNT_CONFIG.to_string()]
        );
    }

    /// The BOOK is this account, not its parent — a sub-account must not be recorded as its master,
    /// which is exactly the collision `vike_tradehub_core::book_identity` exists to break.
    #[test]
    fn a_sub_account_reports_its_own_uid_not_the_parents() {
        let c = client(envelope(json!([{"uid": "222", "mainUid": "111"}])));
        assert_eq!(c.fetch_account_identity(), Ok(Some("222".into())));
    }

    /// ⚠ **okx claims exactly the instruments it was told, and telling it fetches nothing**: the claim
    /// is what lets the restore planner retire a resting order on an admitted instrument the report
    /// lacks, so it names only what a pass reads. The mounted instrument, a repeat (any case) and an
    /// empty name are not added; the list is bounded by the engine's own cap.
    #[test]
    fn it_claims_each_admitted_instrument_it_is_told_and_fetches_nothing() {
        let c = client(envelope(json!([])));
        assert_eq!(c.admitted_symbols(), Some(Vec::new()), "told nothing: a claim of nothing");
        c.admit_symbol("ETH-USDT-SWAP");
        c.admit_symbol("eth-usdt-swap");
        c.admit_symbol("BTC-USDT-SWAP");
        c.admit_symbol("");
        assert_eq!(c.admitted_symbols(), Some(vec!["ETH-USDT-SWAP".to_string()]));
        for n in 0..(vike_exec::MAX_EXTRA_SYMBOLS + 4) {
            c.admit_symbol(&format!("COIN{n}-USDT-SWAP"));
        }
        assert_eq!(c.admitted_symbols().map(|s| s.len()), Some(vike_exec::MAX_EXTRA_SYMBOLS));
        assert!(c.transport.paths().is_empty(), "telling it fetches nothing");
    }

    /// A venue that answered NOTHING is remembered as *not yet known* rather than re-asked in a
    /// loop — same fail-soft rule the parser has.
    #[test]
    fn an_empty_answer_is_remembered_as_unknown() {
        let c = client(envelope(json!([])));
        assert_eq!(c.fetch_account_identity(), Ok(None));
        assert_eq!(c.fetch_account_identity(), Ok(None));
        assert_eq!(c.transport.paths().len(), 1);
    }

    /// ⚠ **AN ERROR IS NEVER CACHED.** This is what keeps an EXPIRED key discoverable: a bybit key
    /// answered *Your api key has expired* on 2026-09-20 and the preflight degraded that venue to
    /// paper. A remembered failure would retire that check at the first flap.
    #[test]
    fn an_error_is_re_asked_rather_than_remembered() {
        let c = client(json!({"code": "50113", "msg": "Invalid Sign", "data": []}));
        assert!(c.fetch_account_identity().is_err());
        assert!(c.fetch_account_identity().is_err());
        assert_eq!(c.transport.paths().len(), 2, "the second ask must reach the venue again");
    }
}

#[cfg(test)]
mod naming_tests {
    use super::*;

    /// The hourly WARN of the degraded pass: said on the first pass past the strict ones, silent inside
    /// the hour, said again at the hour; one good read makes the next failure strict again. (The count
    /// itself is `tests/offline/claim_restart.rs`'s, through the public fill read.)
    #[test]
    fn the_degraded_warning_repeats_once_an_hour_and_a_good_read_resets_it() {
        let mut state = NamingState::default();
        let t0 = Instant::now();
        let down = || "down".to_string();
        for n in 1..=ALGO_NAMING_STRICT_FAILURES {
            assert_eq!(state.failed(t0, down()), Err(down()), "failing pass {n} is strict");
        }
        assert_eq!(state.failed(t0, down()), Ok(true), "the first degraded pass says so");
        let almost = ALGO_NAMING_NOTICE_EVERY - Duration::from_secs(1);
        assert_eq!(state.failed(t0 + almost, down()), Ok(false), "silent inside the hour");
        assert_eq!(state.failed(t0 + ALGO_NAMING_NOTICE_EVERY, down()), Ok(true), "an hour on");
        state.succeeded();
        assert_eq!(state.failed(t0, down()), Err(down()), "strict again after a good read");
    }
}
