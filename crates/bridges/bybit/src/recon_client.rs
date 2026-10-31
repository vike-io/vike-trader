//! Bybit `ReconClient` (Task 12 of the reconciliation-engine feature) — the venue-facing report
//! seam (`vike_exec::recon::ReconClient`) for the linear-perp category this crate trades,
//! reconciling against:
//!
//!   `GET /v5/order/realtime` — resting + recently-closed orders (order reports). Like binance's
//!   `openOrders`, this is the venue's CURRENTLY-open/recent view with no time filter, so `since`
//!   is a no-op here (a fully-closed order that fell out of this set is exactly what the fill/
//!   position reports, not this one, reconcile). It PAGES: `limit=50`, then `nextPageCursor`
//!   followed while pages come back full; a chain that repeats a cursor or outruns its page cap
//!   FAILS the read, because the restore planner reads an order a successful report lacks as gone.
//!   It is asked with NO `orderFilter`, which the demo host answers with the resting conditional orders
//!   beside the active ones (`orderFilter=Order` lists only the active ones, `StopOrder` only the
//!   conditional ones: measured 2026-10-24), typed by `conditional_order_type`; that is what lets
//!   this client CLAIM it lists trigger orders (`ReconClient::lists_trigger_orders`, decision 0192).
//!   `GET /v5/execution/list` — fill history (fill reports); `since` maps to `startTime`.
//!   `GET /v5/position/list` — net position per (symbol, positionIdx) leg (position reports).
//!   `size` on the wire is UNSIGNED — the sign comes from `side` ("Buy"/"Sell"), exactly like
//!   `perp::BybitPerpRest::reconcile_positions`, whose Buy/Sell→sign + hedge `positionIdx`→
//!   `position_side` derivation this factors out as [`parse_positions`] EXCEPT it keeps every row
//!   including a flat (`size == 0`) leg: `recon::diff::diff` needs the report row PRESENT to
//!   detect "local still shows a position the venue has since closed" (mirrors the binance
//!   `ReconClient`'s Task 7 doc'd choice; `reconcile_positions`'s own `if size == 0.0 { continue }`
//!   filter is right for THAT snapshot's `ReconcileSnapshot` shape but wrong for this one).
//!
//! Every fetch delegates to a PURE free function (`parse_*`) over the raw `result` JSON body
//! (the `{retCode, retMsg, result}` V5 envelope already unwrapped by [`unwrap_envelope`]) — these
//! are the fixture-tested units (`tests/offline/recon_client_parse.rs`, no network).
//!
//! `BybitReconClient` reuses the SAME `BybitTransport`/`BybitV5Signer` seam as
//! `perp::BybitPerpRest` (no new HTTP/signing stack): `order/realtime`/`position/list` ride the
//! normal `signed` path (the same one `BybitPerpRest::call` already uses for order/position
//! reads), while `execution/list` rides `signed_requery` — the SAME short-timeout,
//! rate-gate-EXEMPT path the crate's audit-A3 resync (`get_execution_history`) already uses for
//! this exact endpoint, so a resync burst can't blow the order-rate budget submit/cancel depend on.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use vike_bridge_core::json::{json_int, json_num, json_str};
use vike_bridge_core::signer::BybitV5Signer;
use vike_bridge_core::transport::{ErrorKind, VenueApiError};
use vike_exec::recon::{AdmittedSymbols, ReconClient};
use vike_model::events::{LiquiditySide, TradeId};
use vike_model::{
    FeeSchedule, FillReport, MarginMode, OrderStatus, OrderStatusReport, OrderType,
    PositionStatusReport,
};

use crate::perp::{PATH_ACCOUNT, PATH_EXECUTION_LIST, PATH_ORDER_REALTIME, PATH_POSITION_LIST};
use crate::transport::{BybitTransport, unwrap_envelope};

const VENUE: &str = "bybit";
const PATH_FEE_RATE: &str = "/v5/account/fee-rate";

/// Rows asked per `order/realtime` request: the venue's maximum (`limit` is 1..=50, default 20).
const ORDERS_PAGE_LIMIT: usize = 50;

/// Pages one symbol's open-order read follows before it FAILS: 40 x 50 = 2000 rows, far above what
/// rests on one linear symbol, so the bound is for a venue that never ends its cursor chain.
const ORDERS_MAX_PAGES: usize = 40;

// --- pure parsers ------------------------------------------------------------------------------

/// Every V5 `result` object is `{"list": [...], ...}` — pull the row array out, erroring (never
/// panicking) on a missing/malformed `list`.
fn list_rows(body: &str) -> Result<Vec<Value>, String> {
    let v: Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    v.get("list")
        .and_then(|l| l.as_array())
        .cloned()
        .ok_or_else(|| "expected a `list` array".to_string())
}

/// The page's `nextPageCursor`, `None` when absent, `null` or empty (the venue's three spellings of
/// "no more"). Read off the same `result` body [`parse_orders`] takes.
fn next_cursor(body: &str) -> Option<String> {
    let v: Value = serde_json::from_str(body).ok()?;
    v.get("nextPageCursor")?.as_str().filter(|c| !c.is_empty()).map(str::to_string)
}

/// Undo the venue's percent-encoding of a cursor (`id%3Ats%2C...`). The signer encodes every GET
/// value once, and the venue wants its cursor back as it spelled it, so the signer must be handed the
/// DECODED text: the wire then carries the very `%3A`/`%2C` the venue sent, not `%253A`. A `%` not
/// followed by two hex digits stays as it is.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let escaped = (bytes[i] == b'%')
            .then(|| s.get(i + 1..i + 3))
            .flatten()
            .filter(|h| h.bytes().all(|c| c.is_ascii_hexdigit()))
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        match escaped {
            Some(b) => {
                out.push(b);
                i += 3;
            }
            None => {
                out.push(bytes[i]);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn side_sign(s: &str) -> i32 {
    if s.eq_ignore_ascii_case("Buy") { 1 } else { -1 }
}

/// Map a raw V5 `orderStatus` onto the FSM [`OrderStatus`] `recon::diff::diff` reads — every word
/// the V5 order endpoints document, explicitly:
///
/// - `New`, `Created` (accepted by the system, not yet through the matching engine), `Active` (the
///   legacy USDC-contract spelling of `New`) and `Untriggered` (a resting conditional waiting on its
///   trigger) → `Accepted`: all still open.
/// - `Triggered` → `Triggered`; `PartiallyFilled` → `PartiallyFilled`; `Filled` → `Filled`.
/// - `Cancelled` and `PartiallyFilledCanceled` (partially filled, then canceled) → `Canceled`.
/// - `Rejected` → `Rejected`.
/// - `Deactivated` (a conditional that never triggered and was pulled) → `Expired`, matching
///   `history::map_bybit_history`'s convention for the same status.
///
/// A word outside that list is WARNED and read as `Accepted`: live, the conservative direction —
/// `diff` raises no `MissingTerminal` on it, adoption synthesizes no cumulative fill, and the restore
/// planner keeps the order rather than declaring it gone.
fn normalize_order_status(raw: &str) -> OrderStatus {
    match raw {
        "New" | "Created" | "Active" | "Untriggered" => OrderStatus::Accepted,
        "Triggered" => OrderStatus::Triggered,
        "PartiallyFilled" => OrderStatus::PartiallyFilled,
        "Filled" => OrderStatus::Filled,
        "Cancelled" | "PartiallyFilledCanceled" => OrderStatus::Canceled,
        "Rejected" => OrderStatus::Rejected,
        "Deactivated" => OrderStatus::Expired,
        other => {
            tracing::warn!(
                venue = VENUE,
                status = other,
                "unknown orderStatus word — reporting the order as live (Accepted)"
            );
            OrderStatus::Accepted
        }
    }
}

/// Map a raw V5 `orderType` onto [`OrderType`]: `Limit` → `Limit`, `Market` → `Market` (a
/// conditional's trigger kind rides the separate `stopOrderType`, which [`conditional_order_type`]
/// reads on top of this).
/// Any other word (V5's `UNKNOWN`) has no twin and stays [`OrderType::Unrecognized`] lower-cased,
/// as every report spelled it before the type existed.
fn normalize_order_type(raw: &str) -> OrderType {
    match raw {
        "Limit" => OrderType::Limit,
        "Market" => OrderType::Market,
        other => OrderType::Unrecognized(other.to_ascii_lowercase()),
    }
}

/// Type a CONDITIONAL row: `order/realtime` (no `orderFilter`) lists a resting conditional with the
/// plain `orderType` of what it fires AS (`Market`/`Limit`) and the reason it waits in `stopOrderType`.
/// `base` is [`normalize_order_type`]'s answer; `side` is the row's ±1 and `trigger_direction` its
/// `triggerDirection` (`1` rise, `2` fall, `0` none).
///
/// - `StopLoss`, `PartialStopLoss` → `Stop` (`StopLimit` when it fires as a `Limit`).
/// - `Stop`, a plain conditional order (the one row measured on the demo host, 2026-10-24: a
///   reduce-only `Market` sell, `triggerDirection` 2) → the same, but only when it fires in the LOSS
///   direction of the exit it is, the one `perp.rs`'s `build_order_params` places: a BUY trigger on
///   a RISE (`1`), a SELL trigger on a FALL (`2`). The opposite direction is a conditional take-profit
///   or a fade entry, which the row cannot tell apart, so it stays `base` (not cover for a position).
///   FAILS CLOSED: a missing or `0` direction cannot say which way the row fires either, so it stays
///   `base` too (the warning stays, the safe direction).
/// - `TakeProfit`, `PartialTakeProfit` → `TakeProfit`.
/// - `TrailingStop` has no twin among ours: `Unrecognized("trailing_stop")`, the spelling binance's
///   `trailing_stop_market` gets, which the stop-cover rule reads as a stop.
///
/// Everything else keeps `base`, as it always did: the empty word of a plain order, `MmRateClose` (a
/// margin-rate close, not a price trigger), the spot-only `tpslOrder`/`OcoOrder`/`BidirectionalTpslOrder`
/// (this client reads `category=linear`) and any word the docs do not list. A `base` that is not
/// `Market`/`Limit` (V5's `UNKNOWN`) is never retyped. Only `Stop` was recorded; the other words are the
/// documented ones (`/v5/enum`'s `stopOrderType`), not captures.
fn conditional_order_type(
    stop_order_type: &str,
    trigger_direction: i64,
    side: i32,
    base: OrderType,
) -> OrderType {
    if !matches!(base, OrderType::Market | OrderType::Limit) {
        return base;
    }
    let stop = if base == OrderType::Limit { OrderType::StopLimit } else { OrderType::Stop };
    let loss_direction = if side > 0 { 1 } else { 2 };
    match stop_order_type {
        "StopLoss" | "PartialStopLoss" => stop,
        "Stop" if trigger_direction == loss_direction => stop,
        "TakeProfit" | "PartialTakeProfit" => OrderType::TakeProfit,
        "TrailingStop" => OrderType::Unrecognized("trailing_stop".to_string()),
        _ => base,
    }
}

/// `GET /v5/order/realtime` rows -> `OrderStatusReport`. `qty`/`cumExecQty`/`avgPrice` are
/// echoed directly (unlike binance spot, no average-price derivation needed). An empty
/// `orderLinkId` normalizes to `None` (an externally-placed order the venue never echoed our id
/// for) — same convention as the binance `ReconClient`. A resting conditional row is typed by
/// [`conditional_order_type`] (`Stop`, `StopLimit`, `TakeProfit`…) from `stopOrderType`; the report
/// type has no `reduceOnly` or `triggerPrice` field, so those two are not carried (okx's and
/// binance's report rows carry only the kind either).
pub fn parse_orders(body: &str) -> Result<Vec<OrderStatusReport>, String> {
    let rows = list_rows(body)?;
    Ok(rows
        .iter()
        .map(|o| {
            let client_order_id = o
                .get("orderLinkId")
                .and_then(|c| c.as_str())
                .filter(|c| !c.is_empty())
                .map(|c| c.to_string());
            let side = side_sign(o.get("side").and_then(|s| s.as_str()).unwrap_or(""));
            OrderStatusReport {
                venue: VENUE.to_string(),
                symbol: o.get("symbol").and_then(|s| s.as_str()).unwrap_or("").to_string(),
                venue_order_id: o.get("orderId").map(json_str).unwrap_or_default().into(),
                client_order_id,
                side,
                order_type: conditional_order_type(
                    o.get("stopOrderType").and_then(|t| t.as_str()).unwrap_or(""),
                    o.get("triggerDirection").and_then(json_int).unwrap_or(0),
                    side,
                    normalize_order_type(o.get("orderType").and_then(|t| t.as_str()).unwrap_or("")),
                ),
                qty: o.get("qty").and_then(json_num).unwrap_or(0.0),
                filled_qty: o.get("cumExecQty").and_then(json_num).unwrap_or(0.0),
                avg_px: o.get("avgPrice").and_then(json_num).unwrap_or(0.0),
                status: normalize_order_status(
                    o.get("orderStatus").and_then(|s| s.as_str()).unwrap_or(""),
                ),
                ts: o.get("updatedTime").and_then(json_int).unwrap_or(0),
            }
        })
        .collect())
}

/// `GET /v5/execution/list` rows -> `FillReport`. Unlike binance's `myTrades`/`userTrades`, Bybit
/// DOES echo `orderLinkId` on every execution row (see `history::execution_fill`), so
/// `client_order_id` is populated straight from the wire here rather than left `None`.
///
/// A row with no `execId` is DROPPED (`filter_map`), not reported. A `FillReport` with no `trade_id`
/// is worse than a missing one: `vike_exec::recon::diff` decides "already known" by looking the id
/// up in `seen_trade_ids`, so an id-less row can never match and instead FABRICATES a `MissingFill`
/// divergence — one of the two kinds the `hybrid` policy AUTO-APPLIES
/// (`vike_tradehub::reconcile_config::auto_applied_kinds`), i.e. it would book the fill a second time
/// with no operator in front of it. Dropping is the safe direction: the worst case is a real fill
/// going unreconciled until a pass sees it with its id. Bybit V5 documents `execId` on every row.
pub fn parse_fills(body: &str) -> Result<Vec<FillReport>, String> {
    let rows = list_rows(body)?;
    Ok(rows
        .iter()
        .filter_map(|f| {
            let maker = f.get("isMaker").and_then(|b| b.as_bool()).unwrap_or(false);
            let trade_id = match TradeId::new(f.get("execId").map(json_str).unwrap_or_default()) {
                Ok(id) => id,
                Err(_) => {
                    tracing::warn!(
                        venue = VENUE,
                        symbol = %f.get("symbol").and_then(|s| s.as_str()).unwrap_or(""),
                        "execution/list row carries no `execId` — skipping the FillReport; an \
                         id-less row can never match `seen_trade_ids`, so it would fabricate a \
                         MissingFill divergence that `hybrid` auto-applies (booking it twice)"
                    );
                    return None;
                }
            };
            Some(FillReport {
                venue: VENUE.to_string(),
                symbol: f.get("symbol").and_then(|s| s.as_str()).unwrap_or("").to_string(),
                trade_id,
                venue_order_id: f.get("orderId").map(json_str).unwrap_or_default().into(),
                client_order_id: f
                    .get("orderLinkId")
                    .and_then(|c| c.as_str())
                    .filter(|c| !c.is_empty())
                    .map(|c| c.to_string()),
                side: side_sign(f.get("side").and_then(|s| s.as_str()).unwrap_or("")),
                last_qty: f.get("execQty").and_then(json_num).unwrap_or(0.0),
                last_px: f.get("execPrice").and_then(json_num).unwrap_or(0.0),
                commission: vike_bridge_core::json::get_fee_f64(f, "execFee", VENUE),
                commission_asset: f
                    .get("feeCurrency")
                    .and_then(|c| c.as_str())
                    .unwrap_or("")
                    .to_string(),
                liquidity_side: if maker { LiquiditySide::Maker } else { LiquiditySide::Taker },
                ts: f.get("execTime").and_then(json_int).unwrap_or(0),
            })
        })
        .collect())
}

/// `GET /v5/position/list` rows -> `PositionStatusReport`. `size` is UNSIGNED on the wire — the
/// SIGN comes from `side` ("Buy" -> +1, else -1), exactly like
/// `perp::BybitPerpRest::reconcile_positions`; a flat (`size == 0`) row's sign is forced to `0.0`
/// (never a signed zero) rather than trusting whatever `side` a flat leg happens to carry. Every
/// row is kept, including that flat leg — see the module doc for why filtering it out (as
/// `reconcile_positions`'s own snapshot builder does) would hide a real divergence here.
pub fn parse_positions(body: &str) -> Result<Vec<PositionStatusReport>, String> {
    let rows = list_rows(body)?;
    Ok(rows
        .iter()
        .map(|p| {
            let size = p.get("size").and_then(json_num).unwrap_or(0.0).abs();
            let sign =
                if p.get("side").and_then(|s| s.as_str()) == Some("Buy") { 1.0 } else { -1.0 };
            let qty = if size == 0.0 { 0.0 } else { sign * size };
            PositionStatusReport {
                venue: VENUE.to_string(),
                symbol: p.get("symbol").and_then(|s| s.as_str()).unwrap_or("").to_string(),
                position_side: crate::event_mapper::pside_from_idx(p),
                qty,
                avg_px: p.get("avgPrice").and_then(json_num).unwrap_or(0.0),
                ts: p.get("updatedTime").and_then(json_int).unwrap_or(0),
                margin_mode: parse_trade_mode(p),
                // Bybit v5 has no verified per-position isolated-WALLET field (`positionIM` is
                // plain initial margin, present for cross too; `positionBalance` is undocumented
                // for UTA) — deliberately left un-parsed rather than mislabeled. None always.
                // LIVE-PROBED 2026-07-19 (`tests/bybit_isolated_wallet_probe.rs`): demo blocks
                // both isolation paths (10032/110073), a CROSS row carries `positionBalance:"0"`
                // anyway, and current docs deprecate `positionBalance` (= `positionIM`) and
                // `tradeMode` (always 0 on UTA) — so None stays the verified pin.
                isolated_margin: None,
                delta: None,
            }
        })
        .collect())
}

/// **A spot pair's holding as a position** (decision 0231): the `walletBalance` of its BASE coin from `GET
/// /v5/account/wallet-balance` (`coin=<base>`), booked `Cash` (fully funded, never liquidated, as a Hyperliquid spot
/// pair is). The report names the pair by the engine's symbol (`DOGEUSDT.S`). A quantity under ONE step of the
/// pair (dust a fee or a transfer left) is a FLAT row, and a coin the answer does not list is none held: like a
/// derivative's, the row must be present for `recon::diff` to see a locally open position the venue has since
/// flattened. The wallet states no cost basis, so `avg_px` is `0.0` (an adopted holding has no entry price).
/// A body with no `list` is malformed (`Err`), never read as flat.
pub fn spot_holding(
    body: &str,
    symbol: &str,
    base: &str,
    step: f64,
    ts: i64,
) -> Result<PositionStatusReport, String> {
    let v: Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    let list = v
        .get("list")
        .and_then(|l| l.as_array())
        .ok_or_else(|| "expected a `list` array".to_string())?;
    let held = list
        .iter()
        .filter_map(|a| a.get("coin").and_then(|c| c.as_array()))
        .flatten()
        .find(|c| c.get("coin").and_then(|x| x.as_str()) == Some(base))
        .and_then(|c| c.get("walletBalance").and_then(json_num))
        .unwrap_or(0.0);
    Ok(PositionStatusReport {
        venue: VENUE.to_string(),
        symbol: symbol.to_string(),
        position_side: vike_model::events::PositionSide::Both,
        qty: if held >= step { held } else { 0.0 },
        avg_px: 0.0,
        ts,
        margin_mode: vike_model::MarginMode::Cash,
        isolated_margin: None,
        delta: None,
    })
}

/// [`crate::event_mapper::net_spot_base_fee`]'s rule over a reconcile [`FillReport`] (`symbol` already the
/// engine's `DOGEUSDT.S`).
fn net_spot_fill_report(fill: &mut FillReport) {
    if let Some(net) = crate::event_mapper::SpotNet::of(
        &fill.symbol,
        fill.side,
        &fill.commission_asset,
        fill.commission,
        fill.last_qty,
        fill.last_px,
    ) {
        fill.last_qty = net.qty;
        fill.commission = net.commission;
        fill.commission_asset = net.asset;
    }
}

/// Bybit v5 `tradeMode` (`0` = cross, `1` = isolated; number OR string on the wire) ->
/// [`MarginMode`] (margin-mode step-2, read-side only). Anything other than an explicit `1` —
/// including an absent field (UTA portfolio accounts omit a meaningful tradeMode) — is the
/// fail-safe `Cross`, byte-identical to pre-field behavior. Shared by [`parse_positions`] and
/// `perp::BybitPerpRest::reconcile_positions`'s snapshot builder.
pub fn parse_trade_mode(p: &Value) -> MarginMode {
    let isolated = match p.get("tradeMode") {
        Some(Value::Number(n)) => n.as_i64() == Some(1),
        Some(Value::String(s)) => s == "1",
        _ => false,
    };
    if isolated { MarginMode::Isolated } else { MarginMode::Cross }
}

/// `GET /v5/account/wallet-balance` {accountType:UNIFIED} `result.list[].coin[]` rows -> the
/// UNIFIED account's USDT `walletBalance` (total wallet cash — NOT `availableToWithdraw`, which
/// excludes margin currently locked by open positions/orders). Same field
/// `perp::BybitPerpRest::fetch_usdt_balance` already extracts for the legacy
/// `ReconcileSnapshot.balance` — factored out here as the pure, fixture-tested parser this
/// `ReconClient::fetch_balance` override delegates to. `None` when no USDT coin row is present;
/// malformed JSON (or a missing `list` array) is a hard `Err`, never a panic.
pub fn parse_wallet_balance(body: &str) -> Result<Option<f64>, String> {
    let v: Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    let list = v
        .get("list")
        .and_then(|l| l.as_array())
        .ok_or_else(|| "expected a `list` array".to_string())?;
    for acct in list {
        if let Some(coins) = acct.get("coin").and_then(|c| c.as_array())
            && let Some(row) =
                coins.iter().find(|c| c.get("coin").and_then(|x| x.as_str()) == Some("USDT"))
        {
            return Ok(row.get("walletBalance").and_then(json_num));
        }
    }
    Ok(None)
}

/// `GET /v5/account/fee-rate` `result.list[0]` -> live [`FeeSchedule`] from
/// `makerFeeRate`/`takerFeeRate` (fractions, e.g. `"0.00055"`). `Ok(None)` when the list is empty
/// or a rate is absent (fail-soft → static default); a missing `list` is a hard `Err` (via
/// [`list_rows`]).
pub fn parse_fee_rate(body: &str) -> Result<Option<FeeSchedule>, String> {
    let rows = list_rows(body)?;
    Ok(rows.first().and_then(|r| {
        let maker = r.get("makerFeeRate").and_then(json_num)?;
        let taker = r.get("takerFeeRate").and_then(json_num)?;
        Some(FeeSchedule::from_fractions(maker, taker))
    }))
}

// --- the factory -----------------------------------------------------------------------------

/// The venue -> `ReconClient` factory (ReconFactory seam, wave-2 task 6) — moved verbatim from
/// `vike_tradehub_core::build_recon_client`'s `"bybit"` match arm (called from `crate::adapter` since
/// docs/decisions/0096): a FRESH stateless-HMAC `ureq` REST
/// client dedicated to reconcile reads, reusing the SAME `Credentials` the venue's
/// `ExecutionClient`/`spawn_with_recorder` already builds from. Bybit's reconcile REST binds to
/// [`crate::perp::endpoints`]`(mainnet)` — `mainnet` being the SAME already-resolved ceiling
/// verdict (decision 0095: `live` means mainnet) the start threaded into the exec path, resolved
/// ONCE by `crate::adapter`'s `BybitVenueAdapter::start`, so reconcile stays in lockstep with exec
/// instead of re-reading anything for itself. Always `Some` —
/// construction here is pure/infallible (no network); `Option` is kept so the return type matches
/// every other bridge's `recon_client` (deribit's genuinely can fail).
pub fn recon_client(
    creds: &vike_secrets::credentials::Credentials,
    symbol: &str,
    mainnet: bool,
) -> Option<Box<dyn ReconClient>> {
    recon_client_sharing(creds, symbol, mainnet, Arc::default())
}

/// [`recon_client`] over an admitted-symbol list the CALLER also holds: the adapter hands the same `Arc`
/// to the exec thread, so the symbol the engine tells this client about (`ReconClient::admit_symbol`) is
/// also one whose history the reconnect-resync replays.
pub fn recon_client_sharing(
    creds: &vike_secrets::credentials::Credentials,
    symbol: &str,
    mainnet: bool,
    admitted: Arc<AdmittedSymbols>,
) -> Option<Box<dyn ReconClient>> {
    // Reconcile reads follow the SAME demo/mainnet host switch the exec path binds to, so a
    // mainnet start reconciles against the mainnet account; `false` ⇒ demo host, byte-identical to
    // before the switch existed.
    let (rest_base, _) = crate::perp::endpoints(mainnet);
    Some(Box::new(
        BybitReconClient::new(
            BybitV5Signer::new(creds, vike_model::now_ms),
            crate::transport::UreqBybitTransport::new()
                .with_rate_gate(crate::ratelimit::rest_rate_gate()),
            rest_base,
            symbol.to_string(),
        )
        .with_admitted(admitted),
    ))
}

// --- the client ----------------------------------------------------------------------------

/// One reconcile client per venue-symbol — the SAME per-symbol shape `perp::BybitPerpRest` uses,
/// but carrying only what a report fetch needs (signer + transport + base_url + symbol; no
/// order-formatting `SymbolProperties`/leverage).
pub struct BybitReconClient<T: BybitTransport> {
    pub signer: BybitV5Signer,
    pub transport: T,
    pub base_url: String,
    pub symbol: String,
    /// The symbols the engine admitted beyond `symbol` (`ReconClient::admit_symbol`), bare, as the
    /// bybit engine spells them. ONE list, shared with the exec thread (`with_admitted`): the symbols
    /// this client is told are the ones the reconnect-resync replays (decision 0174's reopener).
    admitted: Arc<AdmittedSymbols>,
    /// The pass-wide pause after a rate limit on an admitted read.
    pause: Pause,
    /// A SPOT pair's `(base coin, quantity step)` from its `instruments-info` row, read once per pair (decision
    /// 0231): the holding of a pair is the wallet row of its base coin, and a quantity under one step is flat.
    spot_rows: Mutex<std::collections::HashMap<String, (String, f64)>>,
}

impl<T: BybitTransport> BybitReconClient<T> {
    pub fn new(
        signer: BybitV5Signer,
        transport: T,
        base_url: impl Into<String>,
        symbol: impl Into<String>,
    ) -> Self {
        BybitReconClient {
            signer,
            transport,
            base_url: base_url.into(),
            symbol: symbol.into(),
            admitted: Arc::default(),
            pause: Pause::default(),
            spot_rows: Mutex::default(),
        }
    }

    /// The `(category, venue symbol)` an ENGINE symbol is read in: `DOGEUSDT.S` is the spot pair `DOGEUSDT`, a
    /// name the shape places is read in its own book, any other name is read in `linear` (the mounted symbol's
    /// book, as every read was before decision 0231; the venue refuses a symbol of another book by name).
    fn book_of(symbol: &str) -> (&'static str, &str, bool) {
        let (wire, spot) = crate::classes::split_spot(symbol);
        let book =
            crate::classes::category_from_name(symbol).unwrap_or(crate::data::Category::Linear);
        (book.wire(), wire, spot)
    }

    /// A spot pair's `(base coin, quantity step)`: its `instruments-info` row, read once and kept.
    fn spot_row(&self, pair: &str) -> Result<(String, f64), ReadFail> {
        if let Some(kept) = self.spot_rows.lock().unwrap_or_else(PoisonError::into_inner).get(pair)
        {
            return Ok(kept.clone());
        }
        let body = self.result_raw(
            crate::perp::PATH_INSTRUMENTS,
            &[("category", json!("spot")), ("symbol", json!(pair))],
        )?;
        let body: Value =
            serde_json::from_str(&body).map_err(|e| ReadFail::Parse(e.to_string()))?;
        let row = body
            .get("list")
            .and_then(Value::as_array)
            .and_then(|l| l.first())
            .ok_or_else(|| ReadFail::Parse(format!("{VENUE} lists no spot pair {pair}")))?;
        let base = row.get("baseCoin").and_then(Value::as_str).unwrap_or("").to_string();
        let step = row.get("lotSizeFilter").and_then(|f| f.get("basePrecision")).and_then(json_num);
        match (base.is_empty(), step) {
            (false, Some(step)) if step > 0.0 => {
                self.spot_rows
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .insert(pair.to_string(), (base.clone(), step));
                Ok((base, step))
            }
            _ => Err(ReadFail::Parse(format!(
                "{VENUE} spot pair {pair}: the row carries no base coin or quantity step"
            ))),
        }
    }

    /// Keep the admitted-symbol list in `admitted` instead of a private one, so the holder of the other
    /// end (the exec thread's resync and gap-sentinel) sees every symbol this client is told.
    #[must_use]
    pub fn with_admitted(mut self, admitted: Arc<AdmittedSymbols>) -> Self {
        self.admitted = admitted;
        self
    }

    /// `order/realtime` for ONE symbol, EVERY page of it: the venue lists at most [`ORDERS_PAGE_LIMIT`]
    /// rows a request, so a busy symbol is walked by `nextPageCursor` until the venue says it is done.
    /// The restore planner reads an order a successful report lacks as gone, so a read that stopped
    /// short must FAIL, never return what it has: a repeated cursor and [`ORDERS_MAX_PAGES`] are errors
    /// (and so is any page failing, a rate limit included, which `admitted_rows` judges as before).
    ///
    /// The walk ends on: an empty page, a SHORT page (fewer than the limit asked: the venue still
    /// hands a cursor on the last page, measured on the demo host 2026-10-24, so the cursor alone
    /// would cost a second request every pass), or no cursor. Only a full page with a cursor goes on.
    fn orders_of(&self, symbol: &str) -> Result<Vec<OrderStatusReport>, ReadFail> {
        let (category, wire, spot) = Self::book_of(symbol);
        let mut reports = Vec::new();
        let mut spent: Vec<String> = Vec::new();
        for _ in 0..ORDERS_MAX_PAGES {
            let mut params = vec![
                ("category", json!(category)),
                ("symbol", json!(wire)),
                ("limit", json!(ORDERS_PAGE_LIMIT)),
            ];
            if let Some(cursor) = spent.last() {
                params.push(("cursor", json!(percent_decode(cursor))));
            }
            let body = self.result_raw(PATH_ORDER_REALTIME, &params)?;
            let mut page = parse_orders(&body).map_err(ReadFail::Parse)?;
            if spot {
                // The rows name the venue's pair; the engine's symbol for it carries the spot suffix.
                for report in &mut page {
                    report.symbol = symbol.to_string();
                }
            }
            let full = page.len() >= ORDERS_PAGE_LIMIT;
            reports.extend(page);
            let Some(next) = next_cursor(&body).filter(|_| full) else { return Ok(reports) };
            if spent.contains(&next) {
                return Err(ReadFail::Parse(format!(
                    "{VENUE} order/realtime for {symbol} repeated a page cursor after {} rows: \
                     the read is refused rather than reported as complete",
                    reports.len()
                )));
            }
            spent.push(next);
        }
        Err(ReadFail::Parse(format!(
            "{VENUE} order/realtime for {symbol} is still paging after {ORDERS_MAX_PAGES} pages of \
             {ORDERS_PAGE_LIMIT} rows: the read is refused rather than reported as complete"
        )))
    }

    /// `execution/list` for ONE symbol since `since` (0 = no lower bound).
    fn fills_of(&self, symbol: &str, since: i64) -> Result<Vec<FillReport>, ReadFail> {
        let (category, wire, spot) = Self::book_of(symbol);
        let mut params: Vec<(&str, Value)> =
            vec![("category", json!(category)), ("symbol", json!(wire))];
        if since > 0 {
            params.push(("startTime", json!(since)));
        }
        let body = self.result_requery_raw(PATH_EXECUTION_LIST, &params)?;
        let mut fills = parse_fills(&body).map_err(ReadFail::Parse)?;
        if spot {
            for fill in &mut fills {
                fill.symbol = symbol.to_string();
                net_spot_fill_report(fill);
            }
        }
        Ok(fills)
    }

    /// `position/list` for ONE symbol (the flat row stays present). A SPOT pair has no position: its holding is the
    /// wallet row of its base coin ([`spot_holding`]).
    fn positions_of(&self, symbol: &str) -> Result<Vec<PositionStatusReport>, ReadFail> {
        let (category, wire, spot) = Self::book_of(symbol);
        if spot {
            let (base, step) = self.spot_row(wire)?;
            let body = self.result_raw(
                PATH_ACCOUNT,
                &[("accountType", json!("UNIFIED")), ("coin", json!(base))],
            )?;
            return spot_holding(&body, symbol, &base, step, vike_model::now_ms())
                .map(|r| vec![r])
                .map_err(ReadFail::Parse);
        }
        let body = self.result_raw(
            PATH_POSITION_LIST,
            &[("category", json!(category)), ("symbol", json!(wire))],
        )?;
        parse_positions(&body).map_err(ReadFail::Parse)
    }

    /// Judge the read of ONE admitted symbol. A symbol bybit no longer lists answers a parameter error
    /// naming the symbol (`10001`, "symbol invalid"): it holds nothing, so that is no rows, with a
    /// warning, and the account's other reads still go through (one delisted symbol must not blind the
    /// pass for good: the list never shrinks). A rate limit (the taxonomy's `RateLimited`, or the
    /// Cloudflare IP block bybit answers as `403`) starts the pass-wide pause. Anything else fails the
    /// whole fetch, as before. ⚠ The `10001` wording is bybit's documented one, not live-proven for a
    /// delisted linear symbol.
    fn admitted_rows<R>(
        &self,
        symbol: &str,
        read: Result<Vec<R>, ReadFail>,
    ) -> Result<Vec<R>, String> {
        match read {
            Err(ReadFail::Venue(e))
                if e.code == 10001 && e.msg.to_ascii_lowercase().contains("symbol") =>
            {
                tracing::warn!(
                    venue = VENUE, %symbol,
                    "reconcile: bybit does not list this admitted symbol ({}); reading it as \
                     holding nothing",
                    e.msg
                );
                Ok(Vec::new())
            }
            Err(ReadFail::Venue(e)) if e.kind() == ErrorKind::RateLimited || e.code == 403 => {
                self.pause.start();
                tracing::warn!(
                    venue = VENUE, %symbol,
                    "reconcile: rate limited reading an admitted symbol ({}); the pass is silent \
                     for {}s",
                    e.msg,
                    RATE_LIMIT_BACKOFF.as_secs()
                );
                Err(e.msg)
            }
            other => other.map_err(String::from),
        }
    }

    /// `Err` while the pass is paused after a rate limit on an admitted symbol. Only a client with
    /// admitted symbols ever pauses, so with none this is always `Ok`.
    fn not_backing_off(&self) -> Result<(), String> {
        match self.pause.left() {
            Some(left) => Err(format!(
                "{VENUE} reconcile is backing off for {}s after a rate-limit answer on an admitted \
                 symbol",
                left.as_secs() + 1
            )),
            None => Ok(()),
        }
    }

    /// The normal signed GET path, envelope-unwrapped to `result` and re-serialized once for the
    /// pure parsers — the same `signed` + `unwrap_envelope` pairing `BybitPerpRest::call` already
    /// uses for order/position reads.
    fn result(&self, path: &str, params: &[(&str, Value)]) -> Result<String, String> {
        self.result_raw(path, params).map_err(|e| e.msg)
    }

    /// [`Self::result`] keeping the venue's error (code and message), for a read that judges it.
    fn result_raw(&self, path: &str, params: &[(&str, Value)]) -> Result<String, VenueApiError> {
        let resp = self.transport.signed(&self.base_url, path, "GET", params, &self.signer)?;
        unwrap_envelope(resp).map(|v| v.to_string())
    }

    /// The short-timeout, rate-gate-EXEMPT path — the SAME one the crate's audit-A3 resync
    /// (`get_execution_history`) already uses for this exact heavier history endpoint, so a
    /// resync burst can't blow the per-symbol order-rate budget submit/cancel depend on.
    fn result_requery_raw(
        &self,
        path: &str,
        params: &[(&str, Value)],
    ) -> Result<String, VenueApiError> {
        let resp =
            self.transport.signed_requery(&self.base_url, path, "GET", params, &self.signer)?;
        unwrap_envelope(resp).map(|v| v.to_string())
    }
}

/// How long a pass stays silent after an admitted read was answered a rate limit. The transport
/// keeps no `Retry-After`, so the pause is fixed. (A scheduling clock, kept in this venue crate:
/// `vike-exec`, which owns the admitted list, reads no clock.)
const RATE_LIMIT_BACKOFF: Duration = Duration::from_secs(60);

/// The pass-wide pause: nothing is sent until it lapses. Never started with nothing admitted.
#[derive(Debug, Default)]
struct Pause(Mutex<Option<Instant>>);

impl Pause {
    fn start(&self) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) =
            Some(Instant::now() + RATE_LIMIT_BACKOFF);
    }

    /// How much longer the pause lasts, or `None` when there is none (or it lapsed).
    fn left(&self) -> Option<Duration> {
        let until = (*self.0.lock().unwrap_or_else(PoisonError::into_inner))?;
        until.checked_duration_since(Instant::now()).filter(|left| !left.is_zero())
    }
}

/// Why one report read failed: the venue answered an error (kept whole, so a read of an admitted
/// symbol can judge its code) or its answer could not be used (a body that did not parse, or a page
/// chain that never ended). Both reach the caller as the same text the mounted symbol's read has
/// always produced (the venue's message).
enum ReadFail {
    Venue(VenueApiError),
    Parse(String),
}

impl From<VenueApiError> for ReadFail {
    fn from(e: VenueApiError) -> Self {
        ReadFail::Venue(e)
    }
}

impl From<ReadFail> for String {
    fn from(f: ReadFail) -> String {
        match f {
            ReadFail::Venue(e) => e.msg,
            ReadFail::Parse(s) => s,
        }
    }
}

impl<T: BybitTransport + Send> ReconClient for BybitReconClient<T> {
    /// `since` is a no-op here: `/v5/order/realtime` has no time filter and reports only the
    /// CURRENTLY open + recently-closed set — a fully-closed order that fell out of this set is
    /// exactly what the fill/position reports (not this one) reconcile.
    ///
    /// **Every ADMITTED symbol is read too**, one `order/realtime?symbol=` walk each (every page of it,
    /// `orders_of`), the mounted symbol's own request shape. The `settleCoin`-wide form would be one call but is a different report (it
    /// lists open orders only, where this one also carries recently closed ones), and any failure
    /// fails the whole fetch: the restore planner reads an order a successful report lacks as gone.
    /// With none admitted the request list is the mounted symbol's alone.
    fn fetch_order_status_reports(&self, _since: i64) -> Result<Vec<OrderStatusReport>, String> {
        self.not_backing_off()?;
        let mut reports = self.orders_of(&self.symbol).map_err(String::from)?;
        for symbol in self.admitted.symbols() {
            let rows = self.admitted_rows(&symbol, self.orders_of(&symbol))?;
            reports.extend(rows.into_iter().filter(|r| r.symbol.eq_ignore_ascii_case(&symbol)));
        }
        Ok(reports)
    }

    /// Bybit's `execId` is unique account-wide and the user-data stream folds it bare, so an
    /// admitted symbol's fill keeps its id as is (unlike binance's per-symbol `t`).
    fn fetch_fill_reports(&self, since: i64) -> Result<Vec<FillReport>, String> {
        self.not_backing_off()?;
        let mut reports = self.fills_of(&self.symbol, since).map_err(String::from)?;
        for symbol in self.admitted.symbols() {
            let rows = self.admitted_rows(&symbol, self.fills_of(&symbol, since))?;
            reports.extend(rows.into_iter().filter(|r| r.symbol.eq_ignore_ascii_case(&symbol)));
        }
        Ok(reports)
    }

    fn fetch_position_status_reports(&self) -> Result<Vec<PositionStatusReport>, String> {
        self.not_backing_off()?;
        let mut reports = self.positions_of(&self.symbol).map_err(String::from)?;
        for symbol in self.admitted.symbols() {
            let rows = self.admitted_rows(&symbol, self.positions_of(&symbol))?;
            reports.extend(rows.into_iter().filter(|r| r.symbol.eq_ignore_ascii_case(&symbol)));
        }
        Ok(reports)
    }

    /// **Claims to list trigger orders** (decision 0192), so the restore planner may read a restored
    /// stop this client's report does not list as gone once the quiet passes have counted. What the
    /// claim rests on, each pinned by a test: the read sends no `orderFilter`, so a resting conditional
    /// is listed beside the active orders (measured on the demo host); it pages and FAILS rather than
    /// truncating (`orders_of`); the one trigger kind `submit` places is the stop, which
    /// `conditional_order_type` types back from `stopOrderType` (the conformance harness' resting-kind
    /// row, `tests/offline/claim_prereqs.rs`); and a stop that fired while the node was down shows as a
    /// fill carrying its coid (`execution/list` echoes `orderLinkId`; measured on the demo account), which
    /// the planner joins before it ever reads silence.
    fn lists_trigger_orders(&self) -> bool {
        true
    }

    /// Remember that the engine admitted `symbol` (bybit's engine spelling is the bare symbol; a
    /// `.P` is tolerated and stripped), so every later pass reads it too. The mounted symbol, an
    /// empty one, a repeat and a 17th are ignored (`AdmittedSymbols::admit`).
    fn admit_symbol(&self, symbol: &str) {
        let upper = symbol.to_ascii_uppercase();
        let bare = vike_catalog::split_perp(&upper).0;
        if self.admitted.admit(bare, &self.symbol) {
            tracing::info!(
                venue = VENUE, symbol = %bare,
                "reconcile: the engine admitted a symbol; every pass now reads it too"
            );
        }
    }

    /// Forget a symbol the engine gave back (decision 0205), spelled as `admit_symbol` takes it (a `.P` is stripped).
    fn forget_symbol(&self, symbol: &str) {
        let upper = symbol.to_ascii_uppercase();
        let bare = vike_catalog::split_perp(&upper).0;
        if self.admitted.forget(bare) {
            tracing::info!(
                venue = VENUE, symbol = %bare,
                "reconcile: the engine gave a symbol back; no pass reads it any more"
            );
        }
    }

    /// The symbols this client was told and reads on every pass beyond the mounted one: a real claim,
    /// which the gatewayhub names in its reconcile reply (`gw_trade_recon_read`) so the node claims only
    /// what the daemon's client reads.
    fn admitted_symbols(&self) -> Option<Vec<String>> {
        Some(self.admitted.symbols())
    }

    /// Balance meaning (pinned per the task brief): the UNIFIED account's USDT `walletBalance`
    /// from `/v5/account/wallet-balance` — same field `perp::fetch_usdt_balance` uses, matching
    /// binance perp's "wallet balance, not available" choice (Bybit's UNIFIED account has no
    /// separate spot/perp split the way binance does). A light, unfiltered account read — rides
    /// the normal `signed`/`result` (rate-gated) path, same as `order/realtime`/`position/list`.
    fn fetch_balance(&self) -> Result<Option<f64>, String> {
        let body = self.result(PATH_ACCOUNT, &[("accountType", json!("UNIFIED"))])?;
        parse_wallet_balance(&body)
    }

    /// Live account-actual fee rates (fee model 4/5): `/v5/account/fee-rate` for this linear
    /// symbol — `makerFeeRate`/`takerFeeRate` fractions. A light, unfiltered account read — rides
    /// the normal `signed`/`result` (rate-gated) path. `Ok(None)` keeps the static default.
    fn fetch_fee_rates(&self) -> Result<Option<FeeSchedule>, String> {
        let body = self.result(
            PATH_FEE_RATE,
            &[("category", json!("linear")), ("symbol", json!(self.symbol))],
        )?;
        parse_fee_rate(&body)
    }
}
