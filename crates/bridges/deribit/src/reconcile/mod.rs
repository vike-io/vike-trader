//! Deribit `ReconcileClient` (Task 10 of the reconciliation-engine feature) — the venue-facing
//! report seam (`vike_execution::recon::ReconcileClient`) for options, reconciling against:
//!
//!   `private/get_open_orders_by_instrument` (order reports) — an order the venue reports with no
//!       `label` was never placed by vike (`label` IS our `order_client_id`, see
//!       `execution/rest.rs::build_order_params`), but [`parse_open_orders`] KEEPS the row and maps the
//!       empty/absent label to `order_client_id: None` rather than dropping it — matching
//!       binance's externally-placed-order handling. `recon::diff::diff` can only classify orders
//!       that are present in the report slice, so surfacing (not skipping) is what lets
//!       `Divergence::UnknownOrder` fire for deribit. (`reconcile.rs::build_orders` is a SEPARATE,
//!       unrelated arm-time `OrderRecord` snapshot builder that still skips these — that's fine,
//!       it never feeds `diff::diff`.)
//!   `private/get_open_orders` (the account's, unfiltered) for the orders resting on a COMBO book of
//!       the account's settle currency, which no instrument read names (`ReconcileClientDeribit::combo_orders`).
//!   `private/get_user_trades_by_currency` for the fills of the account's combo LEGS that this client was
//!       not told (`ReconcileClientDeribit::combo_leg_fills`; the node tells it a leg as an admitted instrument
//!       when the gatewayhub's facts take it); each leg's position is then picked out of the position read
//!       below.
//!   `private/get_positions` (currency-scoped, ALL kinds — options AND futures/perps — filtered
//!       client-side to this instrument). The `kind` filter was dropped (Wave 5d) so a started
//!       perp/future row and its coin `delta` actually come back; a `kind:"option"` filter dropped
//!       every perp/future row before it could be seen. `size` is ALREADY SIGNED (negative =
//!       short) — never re-signed by `direction`, exactly as `reconcile.rs::build_positions`
//!       documents. Deribit also reports a per-position `delta` in COIN units — surfaced onto the
//!       report (correct for an inverse contract; folded into net greeks as `coin_delta × spot`).
//!       Deribit's position row carries no per-row timestamp, so [`parse_positions`] takes the
//!       fetch-time `now_ms` as a plain parameter (kept pure/testable) rather than reading the
//!       clock itself.
//!   `private/get_user_trades_by_instrument` (fill reports — NEW: `reconcile.rs` never needed
//!       fills, only `execution/resync.rs`'s audit-A3 resync did, via the sibling `get_user_trades`
//!       helper on `VenueRestDeribit`). `direction` buy/sell -> side ±1 (same mapping as orders),
//!       `amount`/`price` -> `last_qty`/`last_price`, `fee`/`fee_currency` ->
//!       `commission`/`commission_asset`, and Deribit's own `"M"`/`"T"` `liquidity` spelling ->
//!       `LiquiditySide` (a THIRD spelling, distinct from both binance's boolean flags and the
//!       wire's own lowercase `"maker"`/`"taker"` `LiquiditySide::from(&str)`). A trade row's
//!       `label`, when present, maps straight to `order_client_id` (Deribit echoes it, unlike
//!       binance's `myTrades`/`userTrades`, which echo no client id at all).
//!
//! Every fetch delegates to a PURE free function (`parse_*`) over the raw JSON-RPC RESULT
//! payload — the same "just the unwrapped `result` value" shape `reconcile.rs`'s
//! `build_positions`/`build_orders` already take, and what `execution/rest.rs::private_result` already
//! unwraps. These are the fixture-tested units (`tests/offline/reconcile_client_parse.rs`, no network); the
//! live path re-serializes the fetched `Value` to a string once and hands it to the pure parser
//! — the same small, deliberate cost `vike-binance`'s `reconcile/mod.rs` documents.
//!
//! `ReconcileClientDeribit` reuses `VenueRestDeribit::private_result` (`pub(crate)`) — the existing signed
//! JSON-RPC entry point over an authed order WS (`Mutex<DeribitTransport>`) — and no new
//! HTTP/WS/crypto stack. It is constructed via [`ReconcileClientDeribit::connect`] with its OWN
//! dedicated authed socket (NOT the exec side's), so a reconcile fetch never contends the exec
//! order-transport `Mutex` and can never delay an order submit; see that fn's doc for the rationale
//! (this deviates from an earlier "share the exec connection" intent — deliberately). `new` still
//! accepts a caller-supplied `Arc<VenueRestDeribit>` for tests / a future shared-socket path — ⚠ but
//! that client now OWNS the transport it is handed, because it RE-DIALS it.
//!
//! That socket lifecycle is the other half of this module, and it is tested offline:
//! `tests/offline/reconcile_client_redial.rs` drives this client against a local `ws://` Deribit
//! stand-in (`crate::transport::client::DeribitTransport` takes its `websocket_url` as a constructor
//! parameter) that closes the connection mid-session, and asserts the next fetch re-dials and
//! succeeds. See [`ReconcileClientDeribit::call`] for why that retry lives here and may never move
//! anywhere the exec path can reach it.

pub mod venue_read;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use serde_json::{Value, json};

use vike_bridge_core::json::{get_f64 as number, get_f64_opt, json_number, json_text, parse_rows};
use vike_bridge_core::ratelimit::RateGatePause;
use vike_bridge_core::reconcile_admitted::{
    ReconcileAdmittedReadSpec, ReconcileAdmittedSymbols, ReconcileReadError, not_backing_off,
};
use vike_bridge_core::side::sign_default_sell;
use vike_bridge_core::transport::{E_TIMEOUT_AMBIGUOUS, VenueApiError};
use vike_execution::recon::{ReconcileClient, ReconcileFetchError};
use vike_model::events::{LiquiditySide, PositionSide, TradeId};
use vike_model::{
    FeeSchedule, FillReport, OrderStatus, OrderStatusReport, OrderType, PositionStatusReport,
    SymbolProperties, now_ms,
};
use vike_secrets::credentials::Credentials;

use crate::execution::rest::VenueRestDeribit;
use crate::refusals::DeribitReconcileRefusal;
use crate::transport::client::{CallError, DeribitTransport, DeribitUrls};

const VENUE: &str = "deribit";

// --- pure parsers ------------------------------------------------------------------------------

/// Deribit's `get_user_trades_by_instrument` spells the liquidity flag `"M"`/`"T"` — NOT the wire
/// lowercase `"maker"`/`"taker"` `LiquiditySide::from(&str)` parses, and NOT binance's boolean
/// `isMaker`/`maker` flags either. A third, venue-specific spelling.
fn deribit_liquidity(raw: &str) -> LiquiditySide {
    match raw {
        "M" => LiquiditySide::Maker,
        "T" => LiquiditySide::Taker,
        _ => LiquiditySide::Unknown,
    }
}

/// `private/get_open_orders_by_instrument` rows -> `OrderStatusReport`. Every open order is
/// KEPT — an empty/absent `label` (externally-placed — see the module doc) maps to
/// `order_client_id: None` instead of being dropped, mirroring binance's `parse_order_row`, so
/// `recon::diff::diff` can route it to `Divergence::UnknownOrder`. Status is derived from
/// `filled_amount` exactly as `reconcile.rs` already seeds `OrderRecord::status` on arm
/// (`filled > 0.0 -> PartiallyFilled`, else `Accepted`) — this endpoint only ever reports
/// currently-open orders, so no terminal status can appear here.
pub fn parse_open_orders(body: &str) -> Result<Vec<OrderStatusReport>, String> {
    parse_rows(body, parse_order_row)
}

/// Map a Deribit `order_type` onto [`OrderType`]: `limit` and `market` are their twins, `stop_market`
/// → `Stop`, `stop_limit` → `StopLimit`, and both take-profit kinds (`take_market`, `take_limit`) →
/// `TakeProfit` (a `take_limit`'s limit rides the report's `price`), and
/// `market_limit` → `MarketToLimit` (MEASURED on the testnet 2026-11-08, `deribit_market_limit_on_the_testnet`
/// on `ALT_USDC-PERPETUAL`: a RESTING remainder reads `order_type: "limit"`, with `original_order_type:
/// "market_limit"`, so it is reported as the `Limit` it rests as), and `trailing_stop` →
/// `Trailing` (the one trailing kind the bridge places). An ABSENT field is `limit`, as before. Any
/// other word has no twin and stays [`OrderType::Unrecognized`] verbatim.
fn order_type_of(raw: &str) -> OrderType {
    match raw {
        "limit" => OrderType::Limit,
        "market" => OrderType::Market,
        "stop_market" => OrderType::Stop,
        "stop_limit" => OrderType::StopLimit,
        "take_market" | "take_limit" => OrderType::TakeProfit,
        "market_limit" => OrderType::MarketToLimit,
        "trailing_stop" => OrderType::Trailing,
        other => OrderType::Unrecognized(other.to_string()),
    }
}

/// Whether `name` is a COMBO book's instrument, told by the NAME because an order row states no
/// `kind`: every plain instrument names `PERPETUAL` or an expiry date (`27MAR26`, `5OCT26`) after
/// its first `-`, and a spot pair names nothing there, while a combo names its structure
/// (`BTC-FS-5OCT26_PERP`, `BTC-CSR12-6OCT26-86000_86500`). Pinned against every row of the captured
/// `public/get_instruments` answer (`crates/bridges/deribit/tests/fixtures/deribit_get_instruments.json`,
/// both combo kinds included) by `reconcile_client_parse.rs`'s `a_combo_books_limit_is_stated_as_given`.
fn is_combo_name(name: &str) -> bool {
    let is_expiry = |t: &[u8]| {
        let n = t.len();
        (6..=7).contains(&n)
            && t[..n - 5].iter().all(u8::is_ascii_digit)
            && t[n - 5..n - 2].iter().all(u8::is_ascii_uppercase)
            && t[n - 2..].iter().all(u8::is_ascii_digit)
    };
    name.split('-').nth(1).is_some_and(|t| t != "PERPETUAL" && !is_expiry(t.as_bytes()))
}

fn parse_order_row(row: &Value) -> OrderStatusReport {
    let label = row.get("label").and_then(|l| l.as_str()).unwrap_or("");
    let order_client_id = if label.is_empty() { None } else { Some(label.to_string()) };
    let symbol = row.get("instrument_name").and_then(|s| s.as_str()).unwrap_or("").to_string();
    let side = sign_default_sell(row.get("direction").and_then(|d| d.as_str()).unwrap_or(""));
    let order_type =
        order_type_of(row.get("order_type").and_then(|t| t.as_str()).unwrap_or("limit"));
    let qty = number(row, "amount");
    let filled_qty = number(row, "filled_amount");
    let avg_px = number(row, "average_price");
    let ts = row
        .get("last_update_timestamp")
        .and_then(|t| t.as_i64())
        .or_else(|| row.get("creation_timestamp").and_then(|t| t.as_i64()))
        .unwrap_or(0);
    // The terms `execution/rest.rs`'s `order_params_on` sends, read back: a limit kind's `price` (a market
    // kind's is the word "market_price", no number), a trigger kind's `trigger_price`, a trailing
    // stop's distance `trigger_offset` (in price; the row's `trigger_price` there is the moving stop,
    // not an activation, which the venue does not take).
    // A COMBO book's limit is the signed net `execution/combo.rs`'s `build_combo_order_params` sends: a credit
    // is negative and a flat spread zero, so it is stated as given, where a plain instrument's
    // price at or below zero is no price.
    let px = |k: &str| row.get(k).and_then(json_number).filter(|v| v.is_finite() && *v > 0.0);
    let (price, trigger_price, trail) = match order_type {
        OrderType::Limit if is_combo_name(&symbol) => {
            (row.get("price").and_then(json_number).filter(|v| v.is_finite()), None, None)
        }
        OrderType::Limit => (px("price"), None, None),
        OrderType::Stop | OrderType::StopLimit | OrderType::TakeProfit => {
            (px("price"), px("trigger_price"), None)
        }
        OrderType::Trailing => (None, None, px("trigger_offset")),
        _ => (None, None, None),
    };
    OrderStatusReport {
        venue: VENUE.to_string(),
        symbol,
        venue_order_id: row.get("order_id").map(json_text).unwrap_or_default().into(),
        order_client_id,
        side,
        order_type,
        qty,
        filled_qty,
        average_price: avg_px,
        status: OrderStatus::from_fill_progress(filled_qty),
        timestamp: ts,
        price,
        trigger_price,
        trail,
        trail_percent: None,
    }
}

/// What an ORDER counts for the position `row` holds, signed. MEASURED on the testnet 2026-10-30
/// (`BTC_USDC-PERPETUAL` and `BTC_USDC-3NOV26`, long and short): a LINEAR future's `size` is its QUOTE
/// notional (0.0001 BTC reads `size` 7.684072) and its `size_currency` the base coin an order counts
/// (0.0001, negative on a short), while an INVERSE future's `size` is already the order's US dollars
/// (`BTC-PERPETUAL` 10) and an option's the base coin. The row carries no margining word, so a linear
/// future is told by its NAME, where no payload field exists: the token before the first `-` holds a
/// `_` (`BTC_USDC-…`; no inverse name does, `crates/bridges/deribit/CLAUDE.md`'s name-parser list).
pub(crate) fn position_amount(row: &Value, symbol: &str) -> f64 {
    let size = number(row, "size"); // ALREADY SIGNED
    let linear = symbol.split('-').next().is_some_and(|base| base.contains('_'));
    if linear
        && row.get("kind").and_then(Value::as_str) == Some("future")
        && let Some(base) = row.get("size_currency").and_then(json_number)
    {
        return base;
    }
    size
}

/// `private/get_positions` rows -> `PositionStatusReport`, filtered to `symbol` (the endpoint is
/// currency+kind scoped, not instrument scoped — mirrors `reconcile.rs::build_positions`'s
/// filter). `size` is ALREADY SIGNED — never re-signed by `direction` (same guard
/// `reconcile.rs` documents). Options are one-way -> `PositionSide::Both` (mirrors
/// `reconcile.rs`'s `position_sides` staying empty). Deribit's row carries no timestamp; `now_ms`
/// is threaded in by the caller so this parser stays pure/deterministic under test. A flat
/// (`size == 0`) row is KEPT, not filtered — `recon::diff::diff` needs the report row present to
/// detect "local still shows a position the venue has since closed", same rationale as binance's
/// `parse_perp_position_risk`.
pub fn parse_positions(
    body: &str,
    symbol: &str,
    now_ms: i64,
) -> Result<Vec<PositionStatusReport>, String> {
    let v: Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    let rows = v.as_array().ok_or_else(|| "expected a JSON array".to_string())?;
    Ok(rows
        .iter()
        .filter(|row| row.get("instrument_name").and_then(|n| n.as_str()) == Some(symbol))
        .map(|row| PositionStatusReport {
            venue: VENUE.to_string(),
            symbol: symbol.to_string(),
            position_side: PositionSide::Both,
            qty: position_amount(row, symbol),
            average_price: number(row, "average_price"),
            timestamp: now_ms,
            // Deribit is cross-only (portfolio margin = a maintenance-rate variation within
            // cross, see `vike_venues::venue_margin_support`) — the defaults are the truth here.
            margin_mode: Default::default(),
            isolated_margin: None,
            // Deribit's per-position `delta` in COIN units — correct for an INVERSE contract,
            // whose `size` above is USD NOTIONAL (perp/future) rather than coin. Surfaced so a
            // linear perp/future hedge leg folds into net portfolio greeks as `coin_delta × spot`
            // (Wave 5d) instead of re-deriving coin exposure from the USD-notional `size`. Absent
            // (`None`) when the row carries no `delta`; harmless for options (their delta is priced
            // from the chain, not this field).
            delta: row.get("delta").and_then(json_number),
        })
        .collect())
}

/// `private/get_user_trades_by_instrument` (`trades` array, already unwrapped by the caller) rows
/// -> `FillReport`. See the module doc for the field mapping, incl. Deribit's `"M"`/`"T"`
/// liquidity spelling and the `label` -> `order_client_id` passthrough.
///
/// A row with no `trade_id` is DROPPED, not reported. A `FillReport` with no `trade_id` is worse than
/// a missing one: `vike_execution::recon::diff` decides "already known" by looking the id up in
/// `seen_trade_ids`, so an id-less row can never match and instead FABRICATES a `MissingFill`
/// divergence — one of the two kinds the `hybrid` policy AUTO-APPLIES
/// (`vike_tradehub::reconcile_config::auto_applied_kinds`), i.e. it would book the fill a second time with
/// no operator in front of it. Dropping is the safe direction: the worst case is a real fill going
/// unreconciled until a pass sees it with its id. Deribit documents `trade_id` on every trade row.
pub fn parse_user_trades(body: &str) -> Result<Vec<FillReport>, String> {
    // `flatten` is the row DROP — see this fn's doc for why reporting it would be worse.
    let rows = parse_rows(body, |row| {
        let symbol = row.get("instrument_name").and_then(|s| s.as_str()).unwrap_or("").to_string();
        let side = sign_default_sell(row.get("direction").and_then(|d| d.as_str()).unwrap_or(""));
        let order_client_id = row
            .get("label")
            .and_then(|l| l.as_str())
            .filter(|l| !l.is_empty())
            .map(|l| l.to_string());
        let trade_id = match TradeId::new(
            row.get("trade_id").and_then(|t| t.as_str()).unwrap_or(""),
        ) {
            Ok(id) => id,
            Err(_) => {
                tracing::warn!(
                    venue = VENUE,
                    symbol = %symbol,
                    "user-trades row carries no `trade_id` — skipping the FillReport; an id-less \
                     row can never match `seen_trade_ids`, so it would fabricate a MissingFill \
                     divergence that `hybrid` auto-applies (booking the fill twice)"
                );
                return None;
            }
        };
        Some(FillReport {
            venue: VENUE.to_string(),
            symbol,
            trade_id,
            venue_order_id: row.get("order_id").map(json_text).unwrap_or_default().into(),
            order_client_id,
            side,
            last_qty: number(row, "amount"),
            last_price: number(row, "price"),
            commission: vike_bridge_core::json::get_fee_f64(row, "fee", VENUE),
            commission_asset: row
                .get("fee_currency")
                .and_then(|c| c.as_str())
                .unwrap_or("")
                .to_string(),
            liquidity_side: deribit_liquidity(
                row.get("liquidity").and_then(|l| l.as_str()).unwrap_or(""),
            ),
            timestamp: row.get("timestamp").and_then(|t| t.as_i64()).unwrap_or(0),
        })
        .filter(vike_bridge_core::event_map::fill_report_is_bookable)
    })?;
    Ok(rows.into_iter().flatten().collect())
}

/// `private/get_account_summary` result -> the currency's `balance` (raw wallet cash — realized,
/// EXCLUDING unrealized PnL from open option positions). Deribit's `equity` field is the
/// position-inclusive, mark-to-market figure instead; `balance` is picked here to match the OTHER
/// three venues' choice of raw wallet/cash balance (binance `balance`, bybit `walletBalance`, okx
/// `cashBal` — none fold unrealized PnL in), so `ReconcileClient::fetch_balance` means the same thing
/// across every venue this crate's siblings implement it for. `None` when the field is absent;
/// malformed JSON (not an object) is a hard `Err`, never a panic.
pub fn parse_account_summary_balance(body: &str) -> Result<Option<f64>, String> {
    let v: Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    if !v.is_object() {
        return Err("expected a JSON object".to_string());
    }
    Ok(v.get("balance").and_then(json_number))
}

/// `private/get_account_summary` result -> the CASH of a futures engine's settle currency, inverse (its coin,
/// decision 0258) or linear (USDC): `balance` plus `session_rpl`. Deribit moves the session's realized PnL (and
/// the funding it accrues into it) to `balance` only at the 08:00 UTC settlement (its doc: `balance` "does not
/// include open futures PnL", `session_rpl` "resets at each daily settlement"; MEASURED for USDC on the demo log
/// 2026-11-08: a close moved `balance` by its fee alone, the settlement row by the session's cashflows plus
/// funding), so `balance` alone loses the day's realized PnL at a mid-session adoption, and the engine's ledger
/// already holds it. An absent `session_rpl` reads as zero; an absent `balance` is `None` (fail-soft, like
/// [`parse_account_summary_balance`]). The recon client reports [`parse_coin_cash_from_entry`], which also takes
/// back what the settlement moved into `balance` for a futures position still open.
pub fn parse_account_summary_coin_cash(body: &str) -> Result<Option<f64>, String> {
    let v: Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    if !v.is_object() {
        return Err("expected a JSON object".to_string());
    }
    let rpl = v.get("session_rpl").and_then(json_number).unwrap_or(0.0);
    Ok(v.get("balance").and_then(json_number).map(|b| b + rpl))
}

/// The cash a futures engine adopts, which marks every open position from its ENTRY: [`parse_account_summary_coin_cash`]
/// of the `private/get_account_summary` result `summary` less `Σ (total_profit_loss − floating_profit_loss)` over the
/// open `kind: "future"` rows of the same currency's `private/get_positions` result `positions`.
///
/// MEASURED on the testnet 2026-11-09 (`crates/bridges/deribit/tests/fixtures/deribit_settlement_capture.json`): the 08:00
/// UTC settlement moves an open future's PnL from its entry (or the previous settlement) to the settlement price into
/// `balance`, while the position keeps its entry `average_price`, a `total_profit_loss` since the entry and a
/// `floating_profit_loss` since the settlement price. Their difference is every settled day of the position, so
/// `balance + session_rpl` alone would count it twice beside the engine's own mark. A position opened since the last
/// settlement states the two equal (`crates/bridges/deribit/tests/fixtures/deribit_classes_testnet_positions.json`), and a
/// book with no such position reads [`parse_account_summary_coin_cash`]'s figure bit for bit. Only futures rows count:
/// the venue's doc has `balance` exclude "open futures PnL", and an option's premium moved `balance` at the trade (an
/// option held across a settlement is not measured). A row missing either figure counts nothing. Either body
/// malformed is an `Err`, never a panic.
///
/// ⚠ **The two bodies are two reads, so the sum is CROSS-CHECKED against the summary's own** `futures_pl −
/// futures_session_upl` (equal to it in the capture, both books, after 08:00 and after the close). A settlement or a
/// fill landing between the reads makes them differ: past `(k + 1)` units of [`PNL_UNIT`] (`k` the rows summed, each
/// rounded on the venue's grid, plus the summary's own rounding) the pass reports NO balance (an `Err`, so the next
/// pass reads again) and one WARN names both numbers. A summary that states either field not at all skips the check
/// (one DEBUG line) and reports the position sum alone.
pub fn parse_coin_cash_from_entry(summary: &str, positions: &str) -> Result<Option<f64>, String> {
    let cash = parse_account_summary_coin_cash(summary)?;
    let s: Value = serde_json::from_str(summary).map_err(|e| e.to_string())?;
    let v: Value = serde_json::from_str(positions).map_err(|e| e.to_string())?;
    let rows = v.as_array().ok_or_else(|| "expected a JSON array".to_string())?;
    let (settled, k) = rows
        .iter()
        .filter(|r| {
            r.get("kind").and_then(Value::as_str) == Some("future") && number(r, "size") != 0.0
        })
        .filter_map(|r| {
            Some(
                r.get("total_profit_loss").and_then(json_number)?
                    - r.get("floating_profit_loss").and_then(json_number)?,
            )
        })
        // From +0.0, not `sum` (whose empty value is -0.0): `cash - 0.0` is the old figure bit for bit.
        .fold((0.0, 0u32), |(accumulator, k), x| (accumulator + x, k + 1));
    let currency = s.get("currency").and_then(Value::as_str).unwrap_or("");
    let (Some(pl), Some(upl)) =
        (get_f64_opt(&s, "futures_pl"), get_f64_opt(&s, "futures_session_upl"))
    else {
        tracing::debug!(
            target: "vike_deribit::recon",
            currency,
            "the account summary states no futures_pl or futures_session_upl: the settled PnL is not cross-checked"
        );
        return Ok(cash.map(|c| c - settled));
    };
    let stated = pl - upl;
    let unit = PNL_UNIT.iter().find(|(c, _)| *c == currency).map_or(PNL_UNIT_OTHER, |&(_, u)| u);
    if (settled - stated).abs() > f64::from(k + 1) * unit {
        tracing::warn!(
            target: "vike_deribit::recon",
            currency,
            positions = settled,
            summary = stated,
            "the positions read and the account summary state different settled PnL: no balance this pass"
        );
        return Err(
            "the positions and the account summary disagree on the settled PnL (a settlement or a \
                    fill between the two reads): no balance this pass"
                .to_string(),
        );
    }
    Ok(cash.map(|c| c - settled))
}

/// The grid Deribit states a settle currency's PnL on (a position's `total_profit_loss` and `floating_profit_loss`, the
/// summary's `futures_pl` and `futures_session_upl`): the unit of [`parse_coin_cash_from_entry`]'s cross-check.
/// - USDC: MEASURED, 6 decimals in both reads (`crates/bridges/deribit/tests/fixtures/deribit_settlement_capture.json`,
///   `crates/bridges/deribit/tests/fixtures/deribit_classes_testnet_positions.json`).
/// - USDT: assumed USDC's (a dollar book; not measured).
/// - BTC: MEASURED in the same captures, the summary on 8 decimals and a position on 9, so the coarser 1e-8.
/// - ETH: assumed BTC's (a coin book; not measured).
/// - Any other currency: [`PNL_UNIT_OTHER`].
pub const PNL_UNIT: [(&str, f64); 4] =
    [("USDC", 1e-6), ("USDT", 1e-6), ("BTC", 1e-8), ("ETH", 1e-8)];
/// The unit of a currency [`PNL_UNIT`] does not list: a coin's 1e-8 (not measured).
pub const PNL_UNIT_OTHER: f64 = 1e-8;

/// Whether a mount on the `public/get_instrument` `row` reports [`parse_account_summary_coin_cash`]'s cash: a
/// perpetual or a dated future, inverse or linear, whose realized PnL and funding sit in `session_rpl` until the
/// 08:00 settlement. An option's premium and a spot trade move `balance` at the trade (the recorded USDC log), so
/// those mounts keep `balance`. Until 2026-11-08 only an inverse row did, and a linear engine's books ran ahead of the
/// wallet by the session's realized PnL until 08:00.
pub fn reports_session_cash(row: &Value) -> bool {
    matches!(
        crate::catalog::contract_of(row),
        Some(vike_model::Contract::Perp(_) | vike_model::Contract::Future(_))
    ) || crate::catalog::contract_value_of(row).ok().map(|v| v.kind)
        == Some(vike_model::NotionalKind::Inverse)
}

/// **Who this key is at deribit** — what `private/get_account_summary` answers under
/// `extended: true`.
///
/// # ⚠ MEASURED 2026-09-20, and the measurement decided the SHAPE of the code around it
///
/// `crates/bridges/deribit/tests/deribit_smokes/deribit_identity_probe.rs` asked the live demo endpoint BOTH
/// ways. The plain form — exactly what [`ReconcileClientDeribit::fetch_balance`] sends on every
/// reconcile pass — names **nothing**: no id, no username, no email. The `extended` form is a
/// strict SUPERSET of it (43 keys → 60, nothing dropped, `balance` byte-identical) and carries
/// `id`, `username`, `system_name`, `email`, `type` and `referrer_id`.
///
/// So deribit is **okx-shaped, not binance-shaped**: the account read is a genuinely new call
/// rather than a field riding a body somebody already fetched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BookIdentityRead {
    /// `id` — the account this key trades, rendered as a string because that is what
    /// `vike_secrets::AccountRow::venue_account_id` holds. The venue answers a NUMBER.
    ///
    /// The body also states `type` (`"main"` or `"subaccount"`); nothing in this tree acts on it,
    /// so it is not read.
    pub id: String,
}

/// `private/get_account_summary` result (with `extended: true`) -> the account's identity.
///
/// `Ok(None)` when `id` is absent or blank — fail-soft, the same rule
/// [`parse_account_summary_balance`] has and for the same reason: a missing bookkeeping field must
/// degrade to *not yet known*, which is the state every migrated `account` row already carries,
/// rather than fail a start. A non-object body is a hard `Err`, never a panic.
pub fn parse_account_identity(body: &str) -> Result<Option<BookIdentityRead>, String> {
    let v: Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    if !v.is_object() {
        return Err("expected a JSON object".to_string());
    }
    // The venue answers a NUMBER; the string arm is defensive symmetry with
    // `vike_binance::execution::rest::spot::parse_account_identity`, whose venue answers either.
    let id = match v.get("id") {
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::String(s)) if !s.trim().is_empty() => s.trim().to_string(),
        _ => return Ok(None),
    };
    Ok(Some(BookIdentityRead { id }))
}

/// `public/get_instrument` result -> live [`FeeSchedule`] from `maker_commission`/
/// `taker_commission` (fractions of the underlying). Deribit fees are genuinely
/// PER-INSTRUMENT-class (options 0.03% maker==taker vs futures 0% maker / 0.05% taker), NOT
/// per-account — the live layer is scoped to the engine symbol's instrument (no fee-catalog).
/// `Ok(None)` when the fields are absent (fail-soft → static default); a non-object body is a hard
/// `Err`, never a panic.
pub fn parse_fee_rate(body: &str) -> Result<Option<FeeSchedule>, String> {
    let v: Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    if !v.is_object() {
        return Err("expected a JSON object".to_string());
    }
    let maker = v.get("maker_commission").and_then(json_number);
    let taker = v.get("taker_commission").and_then(json_number);
    Ok(match (maker, taker) {
        (Some(m), Some(t)) => Some(FeeSchedule::from_fractions(m, t)),
        _ => None,
    })
}

// --- the client ----------------------------------------------------------------------------

const PATH_OPEN_ORDERS: &str = "private/get_open_orders_by_instrument";
/// The ACCOUNT's open orders, every currency and kind (`crate::execution::rest`'s `find_order_id_by_label` sends it
/// with no parameter too): the one read that lists an order resting on a COMBO book.
const PATH_ACCOUNT_OPEN_ORDERS: &str = "private/get_open_orders";
/// The settle currency's fills, every instrument (`deribit_combo_fill_probe` read a combo's leg rows
/// back through it; `currency=any` is refused, `docs/ops/deribit-any-symbol-plan.md`).
const PATH_CURRENCY_TRADES: &str = "private/get_user_trades_by_currency";
pub(crate) const PATH_POSITIONS: &str = "private/get_positions";
const PATH_USER_TRADES: &str = "private/get_user_trades_by_instrument";
const PATH_ACCOUNT_SUMMARY: &str = "private/get_account_summary";

/// Trades asked per `get_user_trades_by_*` request: the venue's maximum `count`.
const TRADES_PAGE_COUNT: usize = 1000;

/// Pages one fill read follows before it FAILS: 10 x 1000 = 10 000 trades in one reconcile window (a day by
/// default), far above what this account trades, so the bound is for a venue that never ends its chain.
const TRADES_MAX_PAGES: usize = 10;

/// **Every page of a `get_user_trades_by_*` read** (owner ruling F-2: the read used to be one page of
/// [`TRADES_PAGE_COUNT`] with `has_more` never read, reported as the whole window). `params` names the read
/// (`sorting: asc`, the count); `call` asks one page, `refused` words a walk that cannot finish. The first page
/// starts at `since` (none at 0); while the venue says `has_more` (or a page is full), the next starts at the
/// newest trade read so far, the same millisecond included, and a trade it hands again is dropped by its
/// `trade_id` (a row with none is kept for the parser, which drops it). A page that adds no new trade and
/// [`TRADES_MAX_PAGES`] pages fail the read. The rows come back in the venue's order, deduplicated.
fn trade_pages<E>(
    mut params: Value,
    since: i64,
    call: impl Fn(&Value) -> Result<Value, E>,
    refused: impl Fn(DeribitReconcileRefusal) -> E,
) -> Result<Vec<Value>, E> {
    let mut rows: Vec<Value> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut start = since;
    for _ in 0..TRADES_MAX_PAGES {
        if start > 0 {
            params["start_timestamp"] = json!(start);
        }
        let result = call(&params)?;
        let page = result.get("trades").and_then(Value::as_array).cloned().unwrap_or_default();
        let more = result.get("has_more").and_then(Value::as_bool).unwrap_or(false)
            || page.len() >= TRADES_PAGE_COUNT;
        let newest = page.iter().filter_map(|r| r.get("timestamp").and_then(Value::as_i64)).max();
        let before = rows.len();
        rows.extend(page.into_iter().filter(|r| {
            let id = r.get("trade_id").map(json_text).unwrap_or_default();
            id.is_empty() || seen.insert(id)
        }));
        if !more {
            return Ok(rows);
        }
        match newest {
            Some(newest) if rows.len() > before => start = newest,
            _ => {
                return Err(refused(DeribitReconcileRefusal::NoNewTrade {
                    scope: params
                        .get("instrument_name")
                        .or(params.get("currency"))
                        .map(json_text)
                        .unwrap_or_default(),
                    rows: rows.len(),
                }));
            }
        }
    }
    Err(refused(DeribitReconcileRefusal::PageCap {
        pages: TRADES_MAX_PAGES,
        count: TRADES_PAGE_COUNT,
    }))
}

/// One `ReconcileClient` per instrument, over a `VenueRestDeribit` whose order-WS transport this client
/// OWNS — see [`ReconcileClientDeribit::connect`] for why it is a dedicated socket rather than the
/// exec side's (the module doc's original "no second connection" intent was deliberately reversed
/// there). Owning it is what licenses [`ReconcileClientDeribit::call`]'s re-dial.
pub struct ReconcileClientDeribit {
    rest: Arc<VenueRestDeribit>,
    /// Log latch for the re-dial path, so ONE dead socket costs ONE line instead of one per
    /// attempt. Set when the outage is first reported, cleared (with a recovery line) by the next
    /// successful call. The incident this whole path exists for logged 795 identical lines from a
    /// single close — the cure must not reproduce that at a different layer.
    socket_down: AtomicBool,
    /// **The account id, remembered after the first ask.**
    ///
    /// ⚠ Nothing else on this client can fill it: `fetch_balance` sends the PLAIN
    /// `private/get_account_summary`, which was MEASURED to name no account at all (see
    /// [`BookIdentityRead`]). So the first [`ReconcileClient::fetch_account_identity`] IS the request,
    /// exactly as on okx, and the cache saves the REPEATS rather than the first call.
    /// `vike_execution::recon::ReconcileClient::fetch_account_identity` carries why this is the one value on
    /// the trait that may be cached at all.
    identity: std::sync::OnceLock<Option<String>>,
    /// The instruments the account's engine admitted beyond the mounted one (any-symbol): every pass
    /// reads each with the request it makes for the mounted instrument. Empty = the old behaviour,
    /// byte for byte.
    admitted: ReconcileAdmittedSymbols,
    /// The mounted instrument is a perpetual or a dated future, inverse or linear ([`reports_session_cash`]): the
    /// balance reported is [`parse_coin_cash_from_entry`]'s. `false` (an option or spot mount, and a client
    /// built by [`Self::new`]) reports `balance` as it always did.
    coin_cash: bool,
    /// The LEG instruments of this account's combos, learned from the fills of its combo legs
    /// ([`Self::combo_leg_fills`]), verbatim, for the life of the client: the core writes a combo's
    /// legs into its engine directly, and a leg the node did not tell this client (`admit_symbol`) had
    /// its position never reported. Its positions cost no request: the settle currency's position read
    /// holds them.
    combo_legs: Mutex<Vec<String>>,
    /// The pass-wide quiet period after an admitted read was answered a rate limit ([`ADMITTED_READ`]).
    pause: RateGatePause,
}

/// The venue's code for a rate-limited request (`too_many_requests`), on every JSON-RPC method.
const TOO_MANY_REQUESTS: i64 = 10028;

/// **`reports` as they leave the bridge** (decision 0290): each `symbol` respelled from the venue's name to its USER
/// name (`crate::names::user_name_of`). The reads between speak the venue's names.
pub(crate) fn user_named<R>(mut reports: Vec<R>, symbol: fn(&mut R) -> &mut String) -> Vec<R> {
    for report in &mut reports {
        let name = symbol(report);
        *name = crate::names::user_name_of(name);
    }
    reports
}

fn is_rate_limited(e: &VenueApiError) -> bool {
    e.code == TOO_MANY_REQUESTS
}

/// No read here is judged "not listed": a failed admitted read fails the whole fetch, as it always has
/// (no venue answer for an instrument it stopped listing has been measured).
fn never_not_listed(_: &VenueApiError) -> bool {
    false
}

fn describe_owned(e: VenueApiError) -> String {
    describe(&e)
}

/// How a read of an ADMITTED instrument is judged (B-5: a rate limit there makes every fetch of the next
/// [`vike_bridge_core::reconcile_admitted::RATE_LIMIT_BACKOFF`] refuse before it asks); the error text is
/// the one every fetch has always surfaced ([`describe`]).
const ADMITTED_READ: ReconcileAdmittedReadSpec = ReconcileAdmittedReadSpec {
    not_listed: never_not_listed,
    rate_limited: is_rate_limited,
    text: describe_owned,
};

impl ReconcileClientDeribit {
    /// ⚠ The client TAKES OVER `rest`'s order-WS transport: [`ReconcileClientDeribit::call`] re-dials
    /// it (`connect()`, which closes the prior socket) on a transport failure. Handing in the
    /// `Arc<VenueRestDeribit>` the EXEC side drives would therefore re-open the order socket underneath
    /// a live order path — so the "future shared-socket path" this constructor was left open for
    /// is now a decision that has to be made HERE, not merely wired up.
    pub fn new(rest: Arc<VenueRestDeribit>) -> Self {
        ReconcileClientDeribit {
            rest,
            socket_down: AtomicBool::new(false),
            identity: std::sync::OnceLock::new(),
            // the venue's names as the kept rows spell them (a decimal strike's lower-case `d`, DER-3)
            admitted: ReconcileAdmittedSymbols::case_kept(),
            coin_cash: false,
            combo_legs: Mutex::default(),
            pause: RateGatePause::default(),
        }
    }

    /// Report the CASH of the settle currency with the session's realized PnL (`balance + session_rpl`) instead of
    /// `balance`: set for a futures mount ([`reports_session_cash`]), an inverse one's coin books (decision 0258)
    /// and a linear one's dollar books alike.
    #[must_use]
    pub fn with_coin_cash(mut self, coin_cash: bool) -> Self {
        self.coin_cash = coin_cash;
        self
    }

    /// Build a DEDICATED-socket recon client for `(creds, symbol)` on Deribit TESTNET (demo): open a
    /// fresh authed order-WS, blocking on the JSON-RPC auth, and wrap it in its own [`VenueRestDeribit`].
    /// `None` on auth/connect failure (the reconcile stays unwired for this venue — same graceful
    /// degradation as every other absent-recon path).
    ///
    /// DELIBERATE deviation from this module's original "share the exec side's ONE authed socket"
    /// note: recon uses its OWN connection so its (occasional) blocking report fetches NEVER take the
    /// exec side's order-transport `Mutex` — an order submit is never delayed behind a reconcile
    /// round-trip. This mirrors the resync supervisor (which already opens a 2nd authed socket) and
    /// every other venue's "recon uses a fresh transport, never shares exec's" convention. The venue
    /// report reads (`private/get_open_orders`/`get_positions`/`get_user_trades`/`get_account_summary`)
    /// need no order-formatting grid, so a default [`SymbolProperties`] is fine (only `symbol` +
    /// `currency` are read by the fetch paths).
    pub fn connect(urls: &DeribitUrls, creds: &Credentials, symbol: &str) -> Option<Self> {
        let mut transport = DeribitTransport::new(
            &urls.ws,
            creds.api_key.expose(),
            creds.api_secret.expose(),
            None,
        );
        if let Err(e) = transport.connect() {
            tracing::warn!(
                target: "vike_deribit::recon",
                %symbol,
                error = %e,
                "recon order-WS could not be opened (dial or auth, the error names which); deribit reconcile disabled (exec unaffected)"
            );
            return None;
        }
        // currency = the row's settlement currency (`BTC_USDC-PERPETUAL` -> USDC), the scope of the
        // balance and position reads; the leg before the first '-' (BTC-PERPETUAL -> BTC) only where
        // the row cannot be read, matching `execution::currency_of`.
        let row = crate::execution::fetch_instrument_row(&urls.rest, symbol).ok();
        let currency = row
            .as_ref()
            .and_then(crate::execution::settlement_currency_of)
            .unwrap_or_else(|| symbol.split('-').next().unwrap_or(symbol).to_uppercase());
        // A futures mount's cash holds the session's realized PnL, read off the same row ([`reports_session_cash`]).
        let session_cash = row.as_ref().is_some_and(reports_session_cash);
        let rest = VenueRestDeribit::new(transport, symbol, SymbolProperties::default(), &currency);
        Some(ReconcileClientDeribit::new(Arc::new(rest)).with_coin_cash(session_cash))
    }

    /// The open orders of ONE instrument (`private/get_open_orders_by_instrument`), the mounted
    /// instrument's request for any instrument.
    fn open_orders_of(&self, symbol: &str) -> Result<Vec<OrderStatusReport>, ReconcileReadError> {
        let result =
            self.call_venue(PATH_OPEN_ORDERS, &json!({"instrument_name": symbol, "type": "all"}))?;
        parse_open_orders(&result.to_string()).map_err(ReconcileReadError::Parse)
    }

    /// The orders resting on a COMBO book of this account's settle currency. A combo rests on its own
    /// instrument (`BTC-CS-…`), which no instrument read names: not the mounted one, and never an
    /// admitted one (the engine admits a combo's LEGS). So they are picked out of the account's open
    /// orders by name ([`is_combo_name`]) and by the settle currency the name states
    /// (`crate::execution::rest`'s `settle_of_name`, the combo fence's own rule: every combo this account can
    /// place settles in it). Without this read a resting combo was never listed, and the diff raised
    /// its live local order as an `OrphanLocalOrder` on every pass.
    fn combo_orders(&self) -> Result<Vec<OrderStatusReport>, String> {
        let result = self.call(PATH_ACCOUNT_OPEN_ORDERS, &json!({}))?;
        let mut reports = parse_open_orders(&result.to_string())?;
        reports.retain(|r| {
            is_combo_name(&r.symbol)
                && crate::execution::rest::settle_of_name(&r.symbol)
                    .eq_ignore_ascii_case(&self.rest.currency)
        });
        Ok(reports)
    }

    /// The fills of this account's combo LEGS on an instrument no other read names. The engine folds
    /// them (the core admits a combo's legs into it), but a leg the node did not tell this client is
    /// read by no name, so they are picked out of the settle currency's fills: a leg row states the
    /// `combo_id` it executed for and our `label` (CAPTURED,
    /// `crates/bridges/deribit/tests/offline/combo_fill_fixtures.rs`).
    /// Never reported: the combo book's own net print (it states no `combo_id`, and the engine folds no
    /// position from it), a leg row with no label (not ours), and a row of the mounted or an admitted
    /// instrument, which [`Self::fills_of`] already reports under the same `trade_id`. Each leg's
    /// instrument is remembered for [`ReconcileClient::fetch_position_status_reports`] (a pass reads fills
    /// before positions: `ReconcileClient::fetch_mass_status`'s order).
    fn combo_leg_fills(&self, since: i64) -> Result<Vec<FillReport>, String> {
        let params =
            json!({"currency": self.rest.currency, "count": TRADES_PAGE_COUNT, "sorting": "asc"});
        let rows = trade_pages(
            params,
            since,
            |p| self.call(PATH_CURRENCY_TRADES, p),
            |why| why.to_string(),
        )?;
        let read_by_name = self.read_by_name();
        let text =
            |row: &Value, k: &str| row.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        let legs: Vec<Value> = rows
            .into_iter()
            .filter(|row| {
                !text(row, "combo_id").is_empty()
                    && !text(row, "label").is_empty()
                    && !read_by_name
                        .iter()
                        .any(|s| s.eq_ignore_ascii_case(&text(row, "instrument_name")))
            })
            .collect();
        let fills = parse_user_trades(&Value::Array(legs).to_string())?;
        let mut known = self.combo_legs.lock().unwrap_or_else(PoisonError::into_inner);
        for fill in &fills {
            if !known.contains(&fill.symbol) {
                known.push(fill.symbol.clone());
            }
        }
        Ok(fills)
    }

    /// The instruments a pass reads by their own name: the mounted one and every admitted one.
    fn read_by_name(&self) -> Vec<String> {
        let mut names = self.admitted.symbols();
        names.push(self.rest.symbol.clone());
        names
    }

    /// The recent fills of ONE instrument (`private/get_user_trades_by_instrument`): the by-currency
    /// read refuses `currency=any`, so an instrument of another currency is read by its own name.
    fn fills_of(&self, symbol: &str, since: i64) -> Result<Vec<FillReport>, ReconcileReadError> {
        let params = json!({
            "instrument_name": symbol,
            "count": TRADES_PAGE_COUNT,
            "sorting": "asc",
        });
        let call =
            |p: &Value| self.call_venue(PATH_USER_TRADES, p).map_err(ReconcileReadError::Venue);
        let trades =
            trade_pages(params, since, call, |why| ReconcileReadError::Parse(why.to_string()))?;
        parse_user_trades(&Value::Array(trades).to_string()).map_err(ReconcileReadError::Parse)
    }

    /// Every recon fetch below goes through here, and it is the ONE place in this crate that
    /// re-dials a dead order-WS.
    ///
    /// ⚠ **WHY THE RETRY IS HERE AND MAY NOT MOVE DOWN INTO THE TRANSPORT.** Every method this
    /// client sends is an idempotent READ (`private/get_open_orders_by_instrument`,
    /// `private/get_open_orders`, `private/get_positions`, `private/get_user_trades_by_instrument`,
    /// `private/get_user_trades_by_currency`,
    /// `private/get_account_summary`, `public/get_instrument`, and on the funding poll's own client
    /// `private/get_transaction_log`, `crate::execution`'s `spawn_funding_poll`), so re-sending one cannot place,
    /// cancel, double or otherwise move anything. The EXEC path's calls are not: a re-sent
    /// `private/buy` after an ambiguous failure is a SECOND ORDER, which is the entire reason
    /// `crates/bridges/deribit/src/transport/client.rs`'s `call` classifies a response timeout as
    /// [`E_TIMEOUT_AMBIGUOUS`] and hands the decision up. Putting this in
    /// `DeribitTransport::call` — or anywhere `crates/bridges/deribit/src/execution/mod.rs` reaches —
    /// would silently arm blind order retries.
    ///
    /// **What it fixes** (the CI box, 2026-08-23): the transport leaves a failed socket in place and
    /// `connect()` runs exactly once, at start, so the first venue-side close disabled deribit
    /// reconcile for the life of the daemon — 795 identical failures over a day, while the daemon
    /// reported healthy. Every other socket in this crate already has a reconnect lifecycle; this
    /// one had none.
    ///
    /// **ONE retry, no loop.** The reconcile driver re-runs the whole pass every
    /// `VIKE_RECONCILE_INTERVAL_MS` (~60s), which IS the retry cadence, so a genuinely dead venue
    /// costs one failed pass rather than a spin — and a re-dial that fails returns the original
    /// error rather than a connect error, because the original is what the operator needs to see.
    pub(crate) fn call(&self, method: &str, params: &Value) -> Result<Value, String> {
        self.call_venue(method, params).map_err(|e| describe(&e))
    }

    /// [`Self::call`] keeping the venue's error (code and message), for a read that judges it
    /// ([`ADMITTED_READ`]).
    fn call_venue(&self, method: &str, params: &Value) -> Result<Value, VenueApiError> {
        match self.rest.private_result(method, params) {
            Ok(v) => {
                self.mark_healthy();
                Ok(v)
            }
            Err(e) if earns_a_redial(&e) => self.redial_and_retry(method, params, e.into()),
            Err(e) => Err(e.into()),
        }
    }

    /// Re-open the order-WS (`connect()` is idempotent and re-authenticates — it closes the prior
    /// socket first, which is what finally drops the dead one) and re-send the request ONCE.
    fn redial_and_retry(
        &self,
        method: &str,
        params: &Value,
        first: VenueApiError,
    ) -> Result<Value, VenueApiError> {
        // The latch, not the attempt, decides whether this is logged: one line per OUTAGE.
        if !self.socket_down.swap(true, Ordering::Relaxed) {
            tracing::warn!(
                target: "vike_deribit::recon",
                symbol = %self.rest.symbol,
                method,
                code = first.code,
                error = %first.msg,
                "recon order-WS unusable — re-dialing; further failures stay silent until it recovers"
            );
        }
        if let Err(e) = self.rest.transport.lock().unwrap().connect() {
            // Still latched: the next pass retries, silently, ~60s from now. The original error, with
            // the re-dial's failure after its words.
            return Err(VenueApiError {
                code: first.code,
                msg: format!("{} (re-dial failed: {e})", first.msg),
            });
        }
        let out = self.rest.private_result(method, params)?;
        self.mark_healthy();
        Ok(out)
    }

    /// Clear the log latch, announcing the recovery exactly once.
    fn mark_healthy(&self) {
        if self.socket_down.swap(false, Ordering::Relaxed) {
            tracing::info!(
                target: "vike_deribit::recon",
                symbol = %self.rest.symbol,
                "recon order-WS re-dialed — deribit reconcile is live again"
            );
        }
    }
}

/// Which failures earn a re-dial — the policy [`crate::transport::client::CallError`]'s variants
/// deliberately refuse to state for the transport.
///
/// A DEFINITE socket death, plus [`E_TIMEOUT_AMBIGUOUS`]. The timeout is included for two reasons,
/// and the first is what makes it safe: on an idempotent READ there is no ambiguity to respect —
/// the sentinel exists so the EXEC path never blind-retries an order that the venue may already
/// have filled, and none of the methods this predicate's callers send can move anything. The
/// second is that without it one failure shape could never heal: a black-holed socket (TCP still
/// open, venue silent) never produces a send error, so every pass would time out at
/// `request_timeout` forever, which is the same never-recovers defect this whole path fixes,
/// wearing a different error.
///
/// A venue JSON-RPC error (`10009 not_enough_funds`, `13009 unauthorized`, `-32602 Invalid
/// params`) is deliberately NOT here: the venue ANSWERED, so the socket is alive, and re-dialing
/// would churn a fresh TCP+TLS+auth handshake against a credit pool that
/// `crates/bridges/deribit/src/transport/ratelimit.rs` documents as one of the roster's two zero-margin meters —
/// while hiding a real API refusal behind a reconnect that cannot fix it.
///
/// ⚠ **`pub(crate)`, and the READ-ONLY licence is a PRECONDITION on every caller, not a property
/// of the predicate.** It says "a fresh socket might fix this"; it says NOTHING about whether the
/// failed request may be sent a second time — the caller's own methods decide that, exactly as
/// `crates/bridges/deribit/src/transport/client.rs`'s `CallError` splits the two questions.
/// The second caller is `crates/bridges/deribit/src/execution/mod.rs`'s `A3Resync`, whose socket carries
/// `public/auth` plus two `get_*` history reads and nothing else; that type's doc argues its own
/// admission. A caller that can send `private/buy` may NOT use this — see
/// `crates/bridges/deribit/src/execution/rest.rs`'s `dispatch_submit`, which re-dials on a
/// `CallError::DeadSocket` and then re-QUERIES rather than re-sending.
pub(crate) fn earns_a_redial(err: &CallError) -> bool {
    err.is_dead_socket() || err.error().code == E_TIMEOUT_AMBIGUOUS
}

/// The `ReconcileClient` trait's error is a `String`; this is the spelling every fetch below has
/// always surfaced (`[code] message`).
fn describe(err: &VenueApiError) -> String {
    format!("[{}] {}", err.code, err.msg)
}

// --- the factory -----------------------------------------------------------------------------

/// The venue -> `ReconcileClient` factory (ReconFactory seam, wave-2 task 6) — a thin type-erasing
/// wrapper over [`ReconcileClientDeribit::connect`], moved verbatim from `vike_tradehub_core::make_engine`'s
/// `("deribit", Some(c))` arm, which has since moved into this crate as `crate::adapter`'s
/// `VenueAdapterDeribit` (see the comment in its `start` for why Deribit needs its OWN blocking
/// authed order-WS `connect` rather than the pure stateless-REST shape every other bridge's factory
/// builds). `None` on auth/connect failure — exactly the same graceful degradation `connect`
/// already documents, now reachable through the same `recon_client` name every other bridge uses.
pub fn recon_client(
    urls: &DeribitUrls,
    creds: &Credentials,
    symbol: &str,
) -> Option<Box<dyn ReconcileClient>> {
    ReconcileClientDeribit::connect(urls, creds, symbol)
        .map(|rc| Box::new(rc) as Box<dyn ReconcileClient>)
}

impl ReconcileClient for ReconcileClientDeribit {
    /// `since` is a no-op here, for the SAME reason as binance's: `get_open_orders_by_instrument`
    /// has no time filter and reports only the CURRENTLY open set. The mounted and every admitted
    /// instrument, then the resting combos ([`Self::combo_orders`]); any failed read fails the fetch, and
    /// a rate limit on an admitted one silences the pass for a while ([`ADMITTED_READ`]).
    fn fetch_order_status_reports(&self, _since: i64) -> Result<Vec<OrderStatusReport>, String> {
        not_backing_off(VENUE, &self.pause)?;
        let mut reports =
            self.open_orders_of(&self.rest.symbol).map_err(|e| e.into_text(describe_owned))?;
        for symbol in self.admitted.symbols() {
            let read = self.open_orders_of(&symbol);
            reports.extend(ADMITTED_READ.admitted_rows(VENUE, &symbol, &self.pause, read)?);
        }
        reports.extend(self.combo_orders()?);
        Ok(user_named(reports, |r| &mut r.symbol))
    }

    fn fetch_fill_reports(&self, since: i64) -> Result<Vec<FillReport>, String> {
        not_backing_off(VENUE, &self.pause)?;
        let mut reports =
            self.fills_of(&self.rest.symbol, since).map_err(|e| e.into_text(describe_owned))?;
        for symbol in self.admitted.symbols() {
            let read = self.fills_of(&symbol, since);
            reports.extend(ADMITTED_READ.admitted_rows(VENUE, &symbol, &self.pause, read)?);
        }
        reports.extend(self.combo_leg_fills(since)?);
        Ok(user_named(reports, |r| &mut r.symbol))
    }

    fn fetch_position_status_reports(&self) -> Result<Vec<PositionStatusReport>, String> {
        not_backing_off(VENUE, &self.pause)?;
        // No `kind` filter: fetch ALL kinds (options AND futures/perpetuals) for the currency, so a
        // started perp/future position row (with its coin `delta`) actually comes back — a
        // `kind:"option"` filter dropped every perp/future row before it could be seen (Wave 5d).
        // Options are still returned (they are part of the all-kinds response); `parse_positions`
        // then filters client-side to the started `symbol` exactly as before.
        let result =
            self.call(PATH_POSITIONS, &json!({"currency": self.rest.currency}))?.to_string();
        let now = now_ms();
        let mut reports = parse_positions(&result, &self.rest.symbol, now)?;
        // A combo's legs settle in the account's currency (`crate::execution::rest`'s `combo_settle_refusal`), so
        // the read above holds their rows; a leg the mounted or an admitted read reports is not reported twice.
        let read_by_name = self.read_by_name();
        let legs = self.combo_legs.lock().unwrap_or_else(PoisonError::into_inner).clone();
        for leg in
            legs.iter().filter(|leg| !read_by_name.iter().any(|s| s.eq_ignore_ascii_case(leg)))
        {
            reports.extend(parse_positions(&result, leg, now)?);
        }
        let admitted = self.admitted.symbols();
        if !admitted.is_empty() {
            // An admitted instrument may settle in ANOTHER currency than the mounted one, so the
            // mounted currency's read cannot span it. `currency=any` spans every currency (measured
            // on the testnet 2026-10-17); each admitted instrument's rows are picked out of it.
            // It is a read made for the admitted instruments alone, so it is judged as theirs.
            let read = self
                .call_venue(PATH_POSITIONS, &json!({"currency": "any"}))
                .map(|all| vec![all.to_string()])
                .map_err(ReconcileReadError::Venue);
            // One body (no read here is judged "not listed").
            let all = ADMITTED_READ
                .admitted_rows(VENUE, &admitted.join(","), &self.pause, read)?
                .concat();
            for symbol in &admitted {
                reports.extend(parse_positions(&all, symbol, now)?);
            }
        }
        Ok(user_named(reports, |r| &mut r.symbol))
    }

    /// Tell this client the engine admitted `symbol` (any-symbol, decision 0173): every later pass
    /// reads its orders, fills and position too, each with the request it makes for the mounted
    /// instrument, and a failed read fails the whole fetch. `symbol` is a USER name (decision 0290): the list holds
    /// its kept row's venue name, verbatim; a name no kept row answers is held by nothing, said at WARN.
    fn admit_symbol(&self, symbol: &str) {
        match crate::names::venue_name_of(symbol) {
            Ok(venue_name) => {
                self.admitted.admit_logged(&venue_name, &self.rest.symbol, VENUE, "an instrument");
            }
            Err(why) => {
                tracing::warn!(venue = VENUE, %symbol, "{why}; the reconcile pass does not read it")
            }
        }
    }

    /// The engine gave `symbol` back (decision 0205): the next pass stops reading it.
    fn forget_symbol(&self, symbol: &str) {
        if let Ok(venue_name) = crate::names::venue_name_of(symbol) {
            self.admitted.forget_logged(&venue_name, VENUE, "an instrument");
        }
    }

    /// The instruments this client reads beyond the mounted one, by their USER names.
    fn admitted_symbols(&self) -> Option<Vec<String>> {
        Some(self.admitted.symbols().iter().map(|name| crate::names::user_name_of(name)).collect())
    }

    /// **CLAIMS that its open-order read lists the venue's trigger orders** (decision 0192's kind claim:
    /// the restore planner may call an absent stop gone, after the quiet passes, only for a client that
    /// says so). It does: [`Self::open_orders_of`] asks `private/get_open_orders_by_instrument` with
    /// `type: "all"`, which lists an `untriggered` stop with its `label` (MEASURED on the testnet
    /// 2026-10-26, `BTC-PERPETUAL`: `crates/bridges/deribit/tests/offline/native_stops.rs`'s
    /// `held_stop_row` is the venue's row), and [`parse_open_orders`] types every trigger kind the
    /// capability row lets `submit` place (`stop_market` is a `Stop`, `stop_limit` a `StopLimit`,
    /// `take_market` and `take_limit` a `TakeProfit`; `trailing_stop` a `Trailing`, whose resting row
    /// was CAPTURED on the testnet 2026-11-08 by `deribit_trailing_stop_testnet_smoke`, `untriggered`
    /// and with its `label`: `crates/bridges/deribit/tests/offline/native_stops.rs`'s
    /// `CAPTURED_TRAILING_ROW`). The read is one unpaged answer per instrument
    /// (the endpoint has no paging) and a failing read fails the whole fetch, mounted and admitted
    /// instruments alike, so a short report never reaches the planner. The other half of the proof is
    /// the fill side: a stop that FIRED while the node was down is no longer listed (its id ceases to
    /// exist), and the executed order carries the stop's `label`, so its fill names the order client id
    /// (`native_stops`' `a_triggered_stops_fill_names_the_stop_by_its_label`) and the stop lands
    /// `filled_while_down` instead of silence. A stop the venue ended without a fill (cancelled, or fired
    /// into nothing) is absent from every read, and absent is what it is: gone after the quiet passes.
    /// The harness row proves the parsed rows
    /// (`crates/vike-bridge-core/tests/bridge_conformance/venues.rs`'s `deribit_open_rows`); the node
    /// answers from `RECONCILE_LISTS_TRIGGER_ORDERS`, which a harness test holds equal to the proven claimants.
    /// NOT proved on a venue: a restart across a stop that fired while the node was down (the Deribit
    /// stop drill, B10, does it on the testnet).
    fn lists_trigger_orders(&self) -> bool {
        true
    }

    /// Balance meaning (pinned per the task brief): the currency's raw `balance` from
    /// `private/get_account_summary` — NOT `equity` (see the parser's doc comment). A light,
    /// currency-scoped account read — reuses the same `private_result` JSON-RPC entry point over
    /// the existing authed order WS as every other fetch here (no new connection). A futures mount (`coin_cash`)
    /// reads the currency's positions too, right after it, for [`parse_coin_cash_from_entry`]; a failed read, or
    /// positions that disagree with the summary, fails the fetch rather than report the settled PnL twice.
    ///
    /// A failed request carries its outcome under deribit's own table (`crate::transport::error_codes::TAXONOMY`):
    /// `10028` (the rate limit) is not now and a code the table does not map outcome unknown, never a refused key; a
    /// summary that does not parse is held as text.
    fn fetch_balance(&self) -> Result<Option<f64>, ReconcileFetchError> {
        let call = |path: &str, params: &Value| {
            self.call_venue(path, params).map_err(|e| {
                vike_bridge_core::error_kind::read_error(
                    Some(&crate::transport::error_codes::TAXONOMY),
                    &e,
                )
            })
        };
        let result = call(PATH_ACCOUNT_SUMMARY, &json!({"currency": self.rest.currency}))?;
        let balance = if self.coin_cash {
            let positions = call(PATH_POSITIONS, &json!({"currency": self.rest.currency}))?;
            parse_coin_cash_from_entry(&result.to_string(), &positions.to_string())
        } else {
            parse_account_summary_balance(&result.to_string())
        };
        balance.map_err(ReconcileFetchError::unclassified)
    }

    /// **Which deribit account is this key?** — `private/get_account_summary`'s `id`, asked once
    /// per client.
    ///
    /// ⚠ **It sends `extended: true`, and [`fetch_balance`](Self::fetch_balance) deliberately does
    /// not.** The plain form was MEASURED to name no account at all, so folding the flag into the
    /// balance read was the tempting move and is refused: that call runs on every reconcile pass
    /// against a live account, its parameters are what the offline fixtures pin, and widening them
    /// to save one startup request would put a bookkeeping field in the way of the balance the
    /// margin lane reads. The extended body is a strict superset — nothing about `balance` changes
    /// — so the refusal costs one JSON-RPC read per start and nothing else.
    ///
    /// The read rides the socket this client already owns, on the NON-matching-engine budget
    /// (`crates/bridges/deribit/src/transport/client.rs`'s `call` routes `get_*` there), so it cannot
    /// starve an order.
    ///
    /// The venue also answers `type` — `"main"` or `"subaccount"`. The trait carries ONE string and
    /// the one it carries is the BOOK; nothing in this tree reads the parentage.
    ///
    /// An ERROR is never remembered, which is what keeps an expired or revoked key discoverable on
    /// the next pass and at every restart.
    fn fetch_account_identity(&self) -> Result<Option<String>, String> {
        if let Some(cached) = self.identity.get() {
            return Ok(cached.clone());
        }
        let result = self.call(
            PATH_ACCOUNT_SUMMARY,
            &json!({"currency": self.rest.currency, "extended": true}),
        )?;
        let id = parse_account_identity(&result.to_string())?.map(|a| a.id);
        // A loser of the `set` race computed the same answer from the same endpoint, so reading the
        // winner back is what makes every caller in that race see ONE id.
        let _ = self.identity.set(id);
        Ok(self.identity.get().cloned().flatten())
    }

    /// Live per-instrument fee rates (fee model 4/5): `public/get_instrument` for the started
    /// symbol — `maker_commission`/`taker_commission` (fractions of underlying). A public method
    /// sent over the recon client's existing authed order-WS (no new connection, no signing
    /// needed). `Ok(None)` keeps the static default (fail-soft). Deribit fees are per-instrument,
    /// so this is deliberately scoped to `self.rest.symbol` — not a fee-catalog.
    fn fetch_fee_rates(&self) -> Result<Option<FeeSchedule>, String> {
        let result =
            self.call("public/get_instrument", &json!({"instrument_name": self.rest.symbol}))?;
        parse_fee_rate(&result.to_string())
    }
}

#[cfg(test)]
#[path = "reconcile_tests.rs"]
mod reconcile_tests;
