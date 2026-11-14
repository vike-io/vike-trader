//! Shared Binance-grammar `ReconcileClient` core reused by vike-aster — the venue-facing report seam
//! (`vike_execution::recon::ReconcileClient`) for BOTH spot and USDⓈ-M perp. This is rung 5 of the family
//! (see [`crate::family`] mod doc): the pure report parsers AND the fetch/dispatch client, with
//! every per-venue delta — the endpoint paths and the venue string — passed in as parameters.
//!
//! Aster's spot+perp report shapes are byte-identical Binance forks, so both venues call straight
//! into this ONE copy. The sole wire divergences are the endpoint PATHS: Binance splits fapi across
//! `/fapi/v1/*` (orders/trades/commissionRate) and `/fapi/v2/*` (positionRisk/balance), while Aster
//! keeps every fapi path under a single `/fapi/v3/*` namespace. Those paths live in a caller-supplied
//! [`ReconcileSpec`] ([`ReconcilePaths`] + the `venue` label), exactly the same discipline as the market-data
//! ([`crate::family::UrlTable`]) and catalog rungs — nothing here decides which venue or which
//! endpoint it is serving.
//!
//! ⚠ One path slot encodes a real BODY divergence rather than a renamed route:
//! [`ReconcilePaths::spot_commission_rate`]. Binance's spot `/api/v3/account` carries
//! `commissionRates{maker,taker}`, so its spot fee lane prices off the body it already fetches
//! (`None`); Aster's fork of that endpoint dropped the object, so it prices per-symbol off its own
//! `GET /api/v3/commissionRate` (`Some`). See that field's doc and `fetch_fee_rates`.
//!
//! Auth differs too (Binance HMAC vs Aster v3 EIP-712 wallet signatures), but that's the `Signer`
//! seam, invisible here.
//!
//! Reconciles against:
//!
//!   SPOT: `openOrders` (order reports) + `myTrades` (fill reports). Spot has no venue-native
//!         position report: a pair's "position" is its BASE coin's wallet balance (`/api/v3/account`,
//!         `free + locked`), reported per pair as a `Cash` FLOOR row with no price (the spot rule,
//!         owner 2026-11-11: `diff::diff` raises nothing for the coins beyond the engine's, which are
//!         the user's, and holds a wallet below the book). The base coin and step come from each
//!         pair's public `exchangeInfo` row, read once.
//!   PERP: `openOrders` + `userTrades` + `positionRisk` (`positionAmt` is ALREADY SIGNED — long > 0,
//!         short < 0), EXCEPT every row is kept including a flat `qty == 0` leg: `recon::diff::diff`
//!         needs the report row PRESENT to detect "local still shows a position the venue has since
//!         closed" — filtering flat rows (as the deleted legacy snapshot reader did) would make
//!         that divergence invisible here.
//!         `forceOrders` and each fill order's stored client id take the venue's LIQUIDATION, ADL and
//!         settlement trades out of `userTrades` (owner, 2026-11-09: liquidation trades are listed in
//!         userTrades): see `ReconcileClientBinanceFamily::without_liquidations`.
//!
//! **Every PERP report names its instrument by the SERIES label**, the row's `symbol` through
//! [`crate::family::perp_series_symbol`] (`BTCUSDT` → `BTCUSDT.P`), because that is what the local
//! book keys on: `recon::diff`'s position leg matches `(symbol, side)` by plain string, and every
//! event `resolve` synthesizes (a `MissingFill`'s fill, a position leg) carries the report's symbol
//! into an engine that folds only its started `.P` label. Until 2026-10-03 these rows carried the
//! bare `symbol`, so every synthesized fix-up was dropped, and once a history replay had booked a
//! local `.P` position the check raised `PositionOnlyExternal` (venue `BTCUSDT`) +
//! `OrphanLocalPosition` (local `BTCUSDT.P`) on every pass. The REQUESTS still name the bare exchange
//! symbol — the client is built on it — and the SPOT parsers keep the bare label, which is what a
//! spot engine starts on.
//!
//! Every fetch delegates to a PURE free function (`parse_*`) over the raw JSON response body —
//! these are the fixture-tested units. Each venue re-exports them behind a thin 1-arg wrapper that
//! injects its own `venue`, so this shared code stays proven TWICE (once against Binance's golden
//! frames in `vike-binance`'s `tests/offline/reconcile_client_parse.rs`, once against Aster's).
//!
//! Gating precedent: `openOrders`/`positionRisk`/balance/commissionRate ride the normal `signed`
//! path (the one the signed REST clients use for account/position reads), while the heavier `myTrades`/`userTrades` history scans ride `signed_requery` — the SAME
//! short-timeout, rate-gate-EXEMPT path the crate's audit-A3 resync already uses for these exact two
//! endpoints, so a resync burst can't blow the per-IP `ORDERS` budget submit/cancel depend on.
//!
//! **Admitted symbols.** The client is built on the MOUNTED symbol, and `ReconcileClient::admit_symbol`
//! adds the ones the account's engine admitted later (`vike_bridge_core::reconcile_admitted::ReconcileAdmittedSymbols`: bounded,
//! append-only, the engine's spelling). Each fetch then runs the mounted symbol's request FIRST and
//! unchanged, and the same request once per admitted symbol; a row whose symbol was not the one asked
//! for is dropped, a failed read fails the whole fetch (the restore planner reads an order a
//! successful report lacks as gone), and an admitted symbol's fills carry `fill_trade_id`'s
//! symbol-qualified id, as the user-data stream spells them. With none admitted nothing changes.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use vike_bridge_core::json::{json_int, json_number, parse_rows};
use vike_bridge_core::ratelimit::RateGatePause;
use vike_bridge_core::reconcile_admitted::{
    ReconcileAdmittedReadSpec, ReconcileAdmittedSymbols, ReconcileReadError, not_backing_off,
};
use vike_bridge_core::side::sign_default_sell;
use vike_bridge_core::signer::Signer;
use vike_bridge_core::transport::{ErrorKind, RestTransport, VenueApiError};
use vike_execution::recon::ReconcileClient;
use vike_model::events::{LiquiditySide, TradeId};
use vike_model::{
    FeeSchedule, FillReport, OrderStatus, OrderStatusReport, OrderType, PositionStatusReport,
};

use crate::execution::rest::spot::parse_commission_rates;
use crate::family::execution::order_map::json_id;
use crate::family::execution::order_word::OrderWord;
use crate::family::execution::spot_fee::spot_fee_in_quote;

// --- pure parsers (venue supplied as a parameter) ----------------------------------------------

/// Map a raw venue order status ([`OrderWord`] has every word the spot and fapi order endpoints
/// document) onto the FSM [`OrderStatus`] `recon::diff::diff` reads. `NEW` and the other live words →
/// `Accepted`: a pass surfaces the exchange's own liquidation orders as unknown live orders rather than
/// guessing an outcome. The rest → their twins.
///
/// A word outside the list is WARNED and read as `Accepted`
/// (`vike_bridge_core::order_status::unknown_status_word`).
fn normalize_order_status(raw: &str, venue: &str) -> OrderStatus {
    match OrderWord::parse(raw) {
        OrderWord::New | OrderWord::LiveOther => OrderStatus::Accepted,
        OrderWord::PartiallyFilled => OrderStatus::PartiallyFilled,
        OrderWord::Filled => OrderStatus::Filled,
        OrderWord::Canceled => OrderStatus::Canceled,
        OrderWord::PendingCancel => OrderStatus::PendingCancel,
        OrderWord::Rejected => OrderStatus::Rejected,
        OrderWord::Expired => OrderStatus::Expired,
        OrderWord::Unknown => vike_bridge_core::order_status::unknown_status_word(venue, raw),
    }
}

/// Map a raw venue order `type` onto [`OrderType`], the inverse of what the exec builders send
/// (`order_map`'s perp `STOP_MARKET` for a stop) plus the spot and fapi kinds that have a twin:
/// `LIMIT`/`LIMIT_MAKER` → `Limit`; `MARKET` → `Market`; `STOP_MARKET` (fapi) and `STOP_LOSS`
/// (spot) → `Stop`; `STOP` (fapi, a stop whose fill is a limit) and `STOP_LOSS_LIMIT` (spot) →
/// `StopLimit`; `TAKE_PROFIT_MARKET`, `TAKE_PROFIT` and `TAKE_PROFIT_LIMIT` → `TakeProfit` (the
/// report carries no price, so the limit flavour has nothing further to keep). Any other kind
/// (`TRAILING_STOP_MARKET`) has no twin and stays [`OrderType::Unrecognized`] with the word
/// lower-cased, as every report spelled it before the type existed. The fapi conditional words are
/// [`conditional_order_type`]'s.
///
/// `trails`: the row carries a `trailingDelta` above zero (a spot TRAILING stop, the venue's
/// trailing-stop FAQ). A trailing `STOP_LOSS` or `TAKE_PROFIT` is then [`OrderType::Trailing`] (what
/// `crates/bridges/binance/src/execution/rest/spot.rs`'s `VenueRestBinanceSpot::build_order_params` sends for a percent
/// trail, without and with an activation); a trailing `STOP_LOSS_LIMIT` / `TAKE_PROFIT_LIMIT` keeps
/// its word's kind (it rests a FIXED limit, which no trailing kind of ours states). Shared with aster,
/// whose read-back of such a row changes the same way (aster refuses every new kind on submit).
pub(crate) fn normalize_order_type(raw: &str, trails: bool) -> OrderType {
    match raw {
        "STOP_LOSS" | "TAKE_PROFIT" if trails => OrderType::Trailing,
        "LIMIT" | "LIMIT_MAKER" => OrderType::Limit,
        "MARKET" => OrderType::Market,
        "STOP_LOSS" => OrderType::Stop,
        "STOP_LOSS_LIMIT" => OrderType::StopLimit,
        "TAKE_PROFIT_LIMIT" => OrderType::TakeProfit,
        other => conditional_order_type(other),
    }
}

/// The fapi CONDITIONAL kinds, the only words a perp algo row's `orderType` carries (`crate::execution::rest::algo`'s
/// reads) and the report parser's tail: `STOP_MARKET` → `Stop`, `STOP` → `StopLimit`,
/// `TAKE_PROFIT_MARKET`/`TAKE_PROFIT` → `TakeProfit`; anything else (`TRAILING_STOP_MARKET`) stays
/// [`OrderType::Unrecognized`] lower-cased.
pub(crate) fn conditional_order_type(raw: &str) -> OrderType {
    match raw {
        "STOP_MARKET" => OrderType::Stop,
        "STOP" => OrderType::StopLimit,
        "TAKE_PROFIT_MARKET" | "TAKE_PROFIT" => OrderType::TakeProfit,
        other => OrderType::Unrecognized(other.to_ascii_lowercase()),
    }
}

/// Shared row mapping for `openOrders` (spot AND perp share every field except how `avg_px` is
/// derived — see the two callers below). `clientOrderId` empty string normalizes to `None` (an
/// externally-placed order the venue never echoed our id for). Broker-prefix stripped (unified
/// cross-venue attribution, task 6 fix-round-1): `clientOrderId` is the SAME venue-stored id the WS
/// mappers decode, so it is passed through
/// [`crate::family::execution::order_map::strip_broker_order_client_id_prefix`] before `recon::diff::diff` compares it
/// against the local registry's bare order client id — with the client's link id, so only OUR prefix is stripped
/// and a foreign `x-...` id stays as received (an `UnknownOrder`, never ours: owner ruling V8), and
/// load-bearing: without it, EVERY order carries `x-<link_id>-` once a link id is configured and
/// none match locally, so every live local order diffs as `OrphanLocalOrder` AND every venue order
/// as `UnknownOrder` — reconciliation goes blind on both sides at once. (⚠ This line used to say
/// `hybrid` "auto-cancels them all". It does not: `OrphanLocalOrder` folds zero events under every
/// policy — `crates/vike-execution/tests/recon/reconcile_policy_pin.rs`. The stripping is still required; the
/// failure mode is blindness, not destruction. It is at least no longer SILENT blindness: since the
/// orphan sweep gained an outcome, a whole book orphaning this way raises one aggregated
/// `OrphanLocalOrder` alert under `hybrid`/`quarantine`, which is the symptom to look for if a link
/// id is ever configured on a path that skips this stripping.)
fn parse_order_row(
    o: &serde_json::Value,
    avg_px: f64,
    venue: &str,
    link_id: Option<&str>,
) -> OrderStatusReport {
    let symbol = o.get("symbol").and_then(|s| s.as_str()).unwrap_or("").to_string();
    let venue_order_id = o.get("orderId").map(json_id).unwrap_or_default();
    let order_client_id = o
        .get("clientOrderId")
        .and_then(|c| c.as_str())
        .filter(|c| !c.is_empty())
        .map(|c| {
            crate::family::execution::order_map::strip_broker_order_client_id_prefix(c, link_id)
        })
        .map(|c| c.to_string());
    let side = sign_default_sell(o.get("side").and_then(|s| s.as_str()).unwrap_or(""));
    let trailing_delta = o.get("trailingDelta").and_then(json_number).filter(|d| *d > 0.0);
    let raw_type = o.get("type").and_then(|t| t.as_str()).unwrap_or("");
    let order_type = normalize_order_type(raw_type, trailing_delta.is_some());
    // The terms the row states ("0.00000000" is none). A spot trailing stop's percent is its
    // `trailingDelta` in BIPS (`crate::execution::rest::spot::trailing_delta_bips`'s inverse), and its activation is
    // `stopPrice` only on the `TAKE_PROFIT` form `crate::execution::rest::spot` sends one in (a trailing `STOP_LOSS`'s
    // `stopPrice` arms on the other side, so it is not our activation). A kind with no twin states
    // nothing: what its fields mean is not known here.
    let positive =
        |key: &str| o.get(key).and_then(json_number).filter(|v| v.is_finite() && *v > 0.0);
    let (price, trigger_price, trail_percent) = match &order_type {
        OrderType::Trailing => (
            None,
            positive("stopPrice").filter(|_| raw_type == "TAKE_PROFIT"),
            trailing_delta.map(|bips| bips / 100.0),
        ),
        OrderType::Unrecognized(_) => (None, None, None),
        _ => (positive("price"), positive("stopPrice"), None),
    };
    let qty = o.get("origQty").and_then(json_number).unwrap_or(0.0);
    let filled_qty = o.get("executedQty").and_then(json_number).unwrap_or(0.0);
    let raw_status = o.get("status").and_then(|s| s.as_str()).unwrap_or("");
    let ts = o
        .get("updateTime")
        .and_then(|t| t.as_i64())
        .or_else(|| o.get("time").and_then(|t| t.as_i64()))
        .unwrap_or(0);
    OrderStatusReport {
        venue: venue.to_string(),
        symbol,
        venue_order_id: venue_order_id.into(),
        order_client_id,
        side,
        order_type,
        qty,
        filled_qty,
        average_price: avg_px,
        status: normalize_order_status(raw_status, venue),
        timestamp: ts,
        price,
        trigger_price,
        trail: None,
        trail_percent,
    }
}

/// `GET /api/v3/openOrders` rows -> `OrderStatusReport`. Spot carries no `avgPrice` field, so the
/// average fill price is derived from `cummulativeQuoteQty / executedQty` (0.0 when unfilled —
/// never a divide-by-zero). `link_id` is the broker link id the account's orders carry.
pub fn parse_spot_open_orders(
    body: &str,
    venue: &str,
    link_id: Option<&str>,
) -> Result<Vec<OrderStatusReport>, String> {
    parse_rows(body, |o| {
        let executed = o.get("executedQty").and_then(json_number).unwrap_or(0.0);
        let cum_quote = o.get("cummulativeQuoteQty").and_then(json_number).unwrap_or(0.0);
        let avg_px = if executed > 0.0 { cum_quote / executed } else { 0.0 };
        parse_order_row(o, avg_px, venue, link_id)
    })
}

/// `GET /fapi/v{1,3}/openOrders` rows -> `OrderStatusReport`. Perp echoes `avgPrice` directly.
/// The row's `symbol` is reported as the perp SERIES label (see the module doc's PERP note).
pub fn parse_perp_open_orders(
    body: &str,
    venue: &str,
    link_id: Option<&str>,
) -> Result<Vec<OrderStatusReport>, String> {
    parse_rows(body, |o| {
        let avg_px = o.get("avgPrice").and_then(json_number).unwrap_or(0.0);
        let mut report = parse_order_row(o, avg_px, venue, link_id);
        report.symbol = crate::family::perp_series_symbol(&report.symbol);
        report
    })
}

/// Shared row mapping for `myTrades`/`userTrades` (spot and perp differ ONLY in how `side`/`maker`
/// are spelled on the wire — see the two callers below). Neither endpoint echoes a client order
/// id, so `order_client_id` is always `None` here (the caller is expected to resolve it from the
/// matching `venue_order_id` against local state, same as any other externally-sourced fill).
///
/// `None` for a row carrying no `id`: the callers DROP such a row rather than report it. A
/// `FillReport` with no `trade_id` is worse than a missing one, because `vike_execution::recon::diff`
/// decides "already known" by looking the id up in `seen_trade_ids` — an id-less row can never
/// match, so it FABRICATES a `MissingFill` divergence, and `MissingFill` is one of the two kinds the
/// `hybrid` policy AUTO-APPLIES (`vike_tradehub::reconcile_config::auto_applied_kinds`). It would book
/// the fill a second time with no operator in front of it. Dropping the row is the safe direction:
/// the worst case is a real fill going unreconciled until the next pass sees it with its id.
/// Binance/Aster document `id` on every row of both endpoints, so this is a malformed-response path.
fn parse_fill_row(
    f: &serde_json::Value,
    side: i32,
    maker: bool,
    venue: &str,
) -> Option<FillReport> {
    let symbol = f.get("symbol").and_then(|s| s.as_str()).unwrap_or("").to_string();
    let trade_id = match TradeId::new(f.get("id").map(json_id).unwrap_or_default()) {
        Ok(id) => id,
        Err(_) => {
            tracing::warn!(
                venue,
                symbol = %symbol,
                "myTrades/userTrades row carries no `id` (tradeId) — skipping the FillReport; an \
                 id-less row can never match `seen_trade_ids`, so it would fabricate a MissingFill \
                 divergence that `hybrid` auto-applies (booking the fill twice)"
            );
            return None;
        }
    };
    let venue_order_id = f.get("orderId").map(json_id).unwrap_or_default();
    let last_qty = f.get("qty").and_then(json_number).unwrap_or(0.0);
    let last_price_value = f.get("price").and_then(json_number).unwrap_or(0.0);
    let commission = vike_bridge_core::json::get_fee_f64(f, "commission", venue);
    let commission_asset =
        f.get("commissionAsset").and_then(|c| c.as_str()).unwrap_or("").to_string();
    let ts = f.get("time").and_then(|t| t.as_i64()).unwrap_or(0);
    Some(FillReport {
        venue: venue.to_string(),
        symbol,
        trade_id,
        venue_order_id: venue_order_id.into(),
        order_client_id: None,
        side,
        last_qty,
        last_price: last_price_value,
        commission,
        commission_asset,
        liquidity_side: LiquiditySide::from_maker(maker),
        timestamp: ts,
    })
}

/// A fill row's SIDE, read across the two spellings this family uses on the wire.
///
/// The Binance family spells a fill's direction two ways, and which one a body carries is a
/// property of the ENDPOINT, not of the venue:
///   - `side`: the string `"BUY"`/`"SELL"` — Binance's `/fapi/*/userTrades`, **and Aster's SPOT
///     `/api/v3/userTrades`**, which is futures-shaped despite being the spot lane.
///   - `isBuyer`: the boolean — Binance's spot `/api/v3/myTrades`.
///
/// The explicit `side` string wins when present; `isBuyer` is the fallback; a row carrying NEITHER
/// yields `-1`, exactly the pre-union behavior of both callers. This is a documented UNION, not
/// loose parsing: no body in this family carries both spellings, so every existing venue-lane
/// combination reads byte-identically to before (pinned by `fill_grammar_is_a_union_not_a_guess`).
///
/// ⚠ Do NOT "simplify" this to one spelling. Aster's spot lane returns `side`/`maker` with NO
/// `isBuyer`/`isMaker` (verified against the published response example — see
/// `vike_aster::reconcile`'s `ASTER_RECONCILE`), so an `isBuyer`-only reader silently signs EVERY
/// aster spot fill as a SELL.
pub fn fill_side(f: &serde_json::Value) -> i32 {
    match f.get("side").and_then(|s| s.as_str()) {
        Some(s) if !s.is_empty() => sign_default_sell(s),
        _ => {
            if f.get("isBuyer").and_then(|b| b.as_bool()).unwrap_or(false) {
                1
            } else {
                -1
            }
        }
    }
}

/// A fill row's LIQUIDITY flag, the twin of [`fill_side`]: `maker` (futures grammar, and Aster's
/// spot lane) wins, `isMaker` (Binance spot grammar) is the fallback, absent ⇒ `false` (Taker) —
/// the pre-union behavior of both callers.
pub fn fill_is_maker(f: &serde_json::Value) -> bool {
    match f.get("maker").and_then(|b| b.as_bool()) {
        Some(m) => m,
        None => f.get("isMaker").and_then(|b| b.as_bool()).unwrap_or(false),
    }
}

/// Spot account-trade rows -> `FillReport`. The PATH differs by venue — Binance serves
/// `GET /api/v3/myTrades`, Aster `GET /api/v3/userTrades` (see [`ReconcilePaths::spot_my_trades`]) —
/// and so does the row grammar, which is why side/liquidity are read through the family union
/// [`fill_side`]/[`fill_is_maker`] rather than Binance's `isBuyer`/`isMaker` alone.
pub fn parse_spot_my_trades(body: &str, venue: &str) -> Result<Vec<FillReport>, String> {
    // `flatten` is the row DROP: an id-less row yields `None` and leaves the list — see
    // [`parse_fill_row`] for why reporting it would be worse than losing it.
    //
    // The fee is stated in QUOTE units and a BUY's quantity net of the base coin it paid, as the live
    // executionReport and the history replay state them (`crate::family::execution::spot_fee`), so a
    // `MissingFill` built from this report books the figure the stream would have. The perp twin
    // below keeps the venue's quote figure and prices only a third-asset fee (`perp_fee_in_quote`).
    let rows = parse_rows(body, |f| {
        parse_fill_row(f, fill_side(f), fill_is_maker(f), venue).map(|mut report| {
            let fee = spot_fee_in_quote(
                &report.symbol,
                report.last_qty,
                report.commission,
                &report.commission_asset,
                report.last_price,
                report.side > 0,
                venue,
            );
            report.last_qty = fee.net_qty;
            report.commission = fee.commission;
            report.commission_asset = fee.commission_asset;
            report
        })
    })?;
    Ok(rows.into_iter().flatten().collect())
}

/// `GET /fapi/v{1,3}/userTrades` rows -> `FillReport`. Perp spells side as the string `side`
/// (`"BUY"`/`"SELL"`) and the liquidity flag as `maker`; read through the same family union as its
/// spot twin above, so the two lanes cannot drift apart on a grammar the family shares. The row's
/// `symbol` is reported as the perp SERIES label (see the module doc's PERP note).
pub fn parse_perp_user_trades(body: &str, venue: &str) -> Result<Vec<FillReport>, String> {
    // Same id-less row DROP as the spot twin above — see [`parse_fill_row`].
    let rows = parse_rows(body, |f| {
        parse_fill_row(f, fill_side(f), fill_is_maker(f), venue).map(|mut report| {
            report.symbol = crate::family::perp_series_symbol(&report.symbol);
            // A fee in a third asset (BNB) is priced into the quote, as on the stream and the replay.
            if let Some((commission, quote)) = crate::family::execution::spot_fee::perp_fee_in_quote(
                &report.symbol,
                report.commission,
                &report.commission_asset,
                venue,
            ) {
                report.commission = commission;
                report.commission_asset = quote;
            }
            report
        })
    })?;
    Ok(rows.into_iter().flatten().collect())
}

/// `GET /fapi/v{2,3}/positionRisk` rows -> `PositionStatusReport`. `positionAmt` is ALREADY SIGNED
/// (long > 0, short < 0) — never re-signed by `positionSide`. Every row is kept, including a flat
/// (`qty == 0`) leg — see the module doc for why filtering it out would hide a real divergence here.
///
/// Margin-mode step-2 (read-side only): `marginType` (`"cross"`/`"isolated"`, case-insensitive;
/// absent/unrecognized ⇒ Cross, fail-safe = pre-field behavior) and the isolated-wallet carrier
/// (`isolatedWallet`, falling back to `isolatedMargin` — positionRisk carries both; the wallet is
/// the allocated balance, `isolatedMargin` ≈ wallet + uPnL) are parsed into the report's inert
/// carrier fields. Nothing on the REQUEST side changes.
///
/// The row's `symbol` is reported as the perp SERIES label (see the module doc's PERP note): this
/// is the report whose `(symbol, side)` key `recon::diff` matches against the local position book.
pub fn parse_perp_position_risk(
    body: &str,
    venue: &str,
) -> Result<Vec<PositionStatusReport>, String> {
    parse_rows(body, |p| {
        let symbol = crate::family::perp_series_symbol(
            p.get("symbol").and_then(|s| s.as_str()).unwrap_or(""),
        );
        let position_side =
            crate::family::position_side_of(p.get("positionSide").and_then(|s| s.as_str()), venue);
        let qty = p.get("positionAmt").and_then(json_number).unwrap_or(0.0); // ALREADY SIGNED
        let avg_px = p.get("entryPrice").and_then(json_number).unwrap_or(0.0);
        let ts = p.get("updateTime").and_then(|t| t.as_i64()).unwrap_or(0);
        let (margin_mode, isolated_margin) = parse_margin_type(p);
        PositionStatusReport {
            venue: venue.to_string(),
            symbol,
            position_side,
            qty,
            average_price: avg_px,
            timestamp: ts,
            margin_mode,
            isolated_margin,
            delta: None,
        }
    })
}

/// The Binance-grammar margin-mode read of the report parser above: `marginType` `"isolated"` (case-insensitive) ⇒
/// `Isolated` + the isolated-wallet balance (`isolatedWallet`, fallback `isolatedMargin`);
/// anything else (including an absent field) ⇒ `Cross`/`None` — fail-safe, byte-identical to
/// pre-field behavior. A cross row's `isolatedWallet` (`"0"`) is deliberately NOT surfaced.
pub fn parse_margin_type(p: &serde_json::Value) -> (vike_model::MarginMode, Option<f64>) {
    let isolated = p
        .get("marginType")
        .and_then(|m| m.as_str())
        .is_some_and(|m| m.eq_ignore_ascii_case("isolated"));
    if isolated {
        let wallet = p
            .get("isolatedWallet")
            .and_then(json_number)
            .or_else(|| p.get("isolatedMargin").and_then(json_number));
        (vike_model::MarginMode::Isolated, wallet)
    } else {
        (vike_model::MarginMode::Cross, None)
    }
}

/// `GET /fapi/v{2,3}/balance` rows -> the perp USDT wallet `balance` (realized cash — cross wallet
/// balance including realized PnL, EXCLUDING unrealized PnL from open positions; deliberately NOT
/// `availableBalance`, which further excludes margin currently locked by open positions/orders).
/// `None` when no USDT row is present (an account with
/// zero USDT never appears in this array); malformed JSON is a hard `Err`, never a panic (`json_number`
/// never unwraps). The endpoint (Binance `/fapi/v2/balance` vs Aster `/fapi/v3/balance`) lives in the
/// caller's [`ReconcilePaths`], not this body-only parser — the row shape is byte-identical.
pub fn parse_perp_balance(body: &str) -> Result<Option<f64>, String> {
    let v: serde_json::Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    let rows = v.as_array().ok_or_else(|| "expected a JSON array".to_string())?;
    Ok(rows
        .iter()
        .find(|r| r.get("asset").and_then(|a| a.as_str()) == Some("USDT"))
        .and_then(|r| r.get("balance"))
        .and_then(json_number))
}

/// `GET /api/v3/account` `balances[]` rows -> the spot USDT TOTAL, `free + locked`: the same figure
/// the user-data stream's `outboundAccountPosition` states (`execution::event_mapper::spot::map_private` sums the two),
/// which is what the engine's wallet holds. A resting BUY moves price x qty of USDT from `free` to
/// `locked` and changes nothing else, so the `free` figure alone dropped by exactly that lock and
/// the cash diff read it as unexplained (measured on the Binance spot demo: -77.99 for one far BUY).
/// The same choice as the perp wallet `balance` above, which is likewise not `availableBalance`
/// (the figure that excludes what open orders and positions hold). A resting SELL locks the BASE
/// coin, never USDT, so it never touched this row. An absent or null `locked` is zero (the stream
/// mapper reads it so); a `locked` that is present and not a number is `None`, "not reported",
/// never a free-only guess. `None` when no USDT row is present; malformed JSON (or a missing
/// `balances` array) is a hard `Err`, never a panic. Aster's spot lane shares this parser and its
/// stream mapper sums the same two fields.
pub fn parse_spot_balance(body: &str) -> Result<Option<f64>, String> {
    parse_spot_balance_of(body, "USDT")
}

/// [`parse_spot_balance`] for ANY asset's row: the quote coin's for the balance, a spot pair's BASE coin's
/// for its holding ([`parse_spot_holding`]). Same rules, same `None`s.
pub fn parse_spot_balance_of(body: &str, asset: &str) -> Result<Option<f64>, String> {
    let v: serde_json::Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    let balances = v
        .get("balances")
        .and_then(|b| b.as_array())
        .ok_or_else(|| "expected a `balances` array".to_string())?;
    let Some(row) =
        balances.iter().find(|b| b.get("asset").and_then(|a| a.as_str()) == Some(asset))
    else {
        return Ok(None);
    };
    let locked = match row.get("locked") {
        None | Some(serde_json::Value::Null) => Some(0.0),
        Some(l) => json_number(l),
    };
    Ok(row.get("free").and_then(json_number).zip(locked).map(|(free, locked)| free + locked))
}

/// A spot pair's public `GET /api/v3/exchangeInfo?symbol=` answer -> `(base coin, LOT_SIZE step)`, the step
/// the order grid reads ([`crate::family::catalog::spot_properties`]). `None` when the answer lists no row of
/// `symbol` or the row names no base coin.
pub fn parse_spot_pair(info: &serde_json::Value, symbol: &str) -> Option<(String, f64)> {
    let row = info.get("symbols")?.as_array()?.iter().find(|r| {
        r.get("symbol").and_then(|s| s.as_str()).is_some_and(|s| s.eq_ignore_ascii_case(symbol))
    })?;
    let base = row.get("baseAsset")?.as_str().filter(|b| !b.is_empty())?.to_string();
    Some((base, crate::family::catalog::spot_properties(row).step_size))
}

/// **A spot pair's holding as its position row** (the spot rule, owner 2026-11-11): the `GET /api/v3/account`
/// row of its BASE coin, `free + locked` as [`parse_spot_balance`] reads the quote coin (a resting SELL locks the
/// coin it sells), through the one spot constructor (`PositionStatusReport::spot_holding`: `Cash`, no price,
/// flat under one `step`). A coin the body does not list is none held. `symbol` is the engine's bare label.
pub fn parse_spot_holding(
    account: &str,
    symbol: &str,
    base: &str,
    step: f64,
    venue: &str,
    ts: i64,
) -> Result<PositionStatusReport, String> {
    let held = parse_spot_balance_of(account, base)?.unwrap_or(0.0);
    Ok(PositionStatusReport::spot_holding(venue, symbol, held, step, ts))
}

/// `/api/v3/account` body -> live SPOT [`FeeSchedule`] from `commissionRates{maker,taker}`
/// (fractions). Delegates to the pure [`crate::execution::rest::spot::parse_commission_rates`] (the field extractor
/// that surfaces what the `/api/v3/account` body already carries). `Ok(None)` when the object is
/// absent (fail-soft → static default); malformed JSON is a hard `Err`, never a panic.
pub fn parse_spot_fee_rates(body: &str) -> Result<Option<FeeSchedule>, String> {
    let v: serde_json::Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    Ok(parse_commission_rates(&v).map(|r| FeeSchedule::from_binance_rates(r.maker, r.taker)))
}

/// A `commissionRate` endpoint body -> live [`FeeSchedule`] from `makerCommissionRate`/
/// `takerCommissionRate` (fractions). `Ok(None)` when either field is absent (fail-soft → static
/// default); malformed JSON is a hard `Err`.
///
/// This grammar is LANE-NEUTRAL, which is why the function is not named for one: the family serves
/// it on the perp host (`GET /fapi/v{1,3}/commissionRate`, both venues) **and** on Aster's spot host
/// (`GET /api/v3/commissionRate`). Only the PATH differs — see [`ReconcilePaths::perp_commission_rate`]
/// and [`ReconcilePaths::spot_commission_rate`]. [`parse_perp_fee_rates`] is the historical perp-lane
/// name, kept because both venues re-export it 1-arg for their golden-body fixture tests.
pub fn parse_commission_rate_body(body: &str) -> Result<Option<FeeSchedule>, String> {
    let v: serde_json::Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    let maker = v.get("makerCommissionRate").and_then(json_number);
    let taker = v.get("takerCommissionRate").and_then(json_number);
    Ok(match (maker, taker) {
        (Some(m), Some(t)) => Some(FeeSchedule::from_binance_rates(m, t)),
        _ => None,
    })
}

/// The PERP-lane name for [`parse_commission_rate_body`] — the `GET /fapi/v{1,3}/commissionRate`
/// body. Delegates verbatim; kept as its own name because both venues re-export it 1-arg and their
/// `reconcile_client_parse` test fixtures are written against it.
pub fn parse_perp_fee_rates(body: &str) -> Result<Option<FeeSchedule>, String> {
    parse_commission_rate_body(body)
}

// --- venue spec (the ONE real per-venue delta: endpoint paths + label) -------------------------

/// The reconcile endpoint table for one venue. The spot paths (`/api/v3/*`) happen to be identical
/// across the two venues; the perp paths are the real divergence (Binance's `/fapi/v1|v2` split vs
/// Aster's single `/fapi/v3`), which is exactly why they are passed as values rather than baked in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReconcilePaths {
    /// Spot live open orders (signed).
    pub spot_open_orders: &'static str,
    /// Spot recent fills (signed, `signed_requery` history path).
    ///
    /// ⚠ **This is a REAL per-venue divergence, not a shared constant.** Binance serves
    /// `/api/v3/myTrades`; Aster renamed it to `/api/v3/userTrades` and serves NO `myTrades` at
    /// all (a plain 404), so hardcoding Binance's spelling made aster's spot `fetch_fill_reports`
    /// fail every pass — and because `run_pass` short-circuits on the first `Err`, that aborted the
    /// WHOLE spot reconcile pass, orders and positions included.
    pub spot_my_trades: &'static str,
    /// Spot account snapshot (balances + — on Binance — `commissionRates`), signed. Always used for
    /// [`ReconcileClient::fetch_balance`]; used for the spot FEE lane only when
    /// [`Self::spot_commission_rate`] is `None`.
    pub spot_account: &'static str,
    /// Spot per-symbol commission-rate endpoint (signed), or `None` when the venue prices spot fees
    /// off `spot_account`'s `commissionRates` object instead.
    ///
    /// The two venues genuinely differ here, which is why this is a slot rather than a constant:
    ///
    ///   - **Binance is `None`.** Its `GET /api/v3/account` body carries `commissionRates{maker,
    ///     taker}`, so the spot fee lane is free — it reads the SAME body `fetch_balance` already
    ///     fetches, no second endpoint. Keeping `None` is what makes that path byte-identical to
    ///     before this slot existed.
    ///   - **Aster is `Some("/api/v3/commissionRate")`.** Its `/api/v3/account` fork DROPPED the
    ///     `commissionRates` object (measured live 2026-08-05 — the body carries `balances`,
    ///     `canBurnAsset`, `canDeposit`, `canTrade`, `canWithdraw`, `feeTier`, `updateTime` and
    ///     nothing fee-shaped but the tier INDEX), so `parse_spot_fee_rates` answered `Ok(None)`
    ///     forever there. The venue prices spot per-symbol on its own documented endpoint instead.
    ///
    /// `Some` routes the spot fee lane through [`parse_commission_rate_body`] with a `symbol` param;
    /// `None` keeps the `spot_account` + [`parse_spot_fee_rates`] path. Fee-lane only — nothing else
    /// reads this, so a venue that sets it does not change any other verb.
    pub spot_commission_rate: Option<&'static str>,
    /// Spot exchange information (PUBLIC, `symbol=`): a pair's base coin and `LOT_SIZE` step, which the spot
    /// lane's holding row needs (`fetch_position_status_reports`). Read once per pair for the life of the client.
    pub spot_exchange_info: &'static str,
    /// Perp live open orders (signed).
    pub perp_open_orders: &'static str,
    /// Perp recent fills (signed, `signed_requery` history path).
    pub perp_user_trades: &'static str,
    /// Perp liquidation and ADL orders, `GET /fapi/v{1,3}/forceOrders` (signed, `signed_requery`
    /// history path): their trades are taken out of the fills read (`without_liquidations`).
    pub perp_force_orders: &'static str,
    /// Perp order by id, `GET /fapi/v{1,3}/order` (signed, `signed_requery`): the stored client id of
    /// each other fill order, read for a venue liquidation prefix (`without_liquidations`).
    pub perp_order: &'static str,
    /// Perp position risk (signed).
    pub perp_positions: &'static str,
    /// Perp USDT wallet balance (signed).
    pub perp_balance: &'static str,
    /// Perp per-symbol commission-rate endpoint (signed), or `None` when the venue has no verified
    /// commissionRate endpoint — then `fetch_fee_rates` for perp is inert (`Ok(None)`), never
    /// issuing a live call to an unverified path. (Spot fees ride `spot_account` and need no
    /// separate endpoint, so they are always attempted.)
    pub perp_commission_rate: Option<&'static str>,
}

/// A venue's full reconcile spec: its endpoint [`ReconcilePaths`] plus the `venue` string stamped on
/// every report row. `Copy` + all-`&'static str`, so a reconcile thread carries it by value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReconcileSpec {
    /// The canonical venue key stamped on every report (`"binance"` / `"aster"`).
    pub venue: &'static str,
    /// The resolved endpoint paths (see [`ReconcilePaths`]).
    pub paths: ReconcilePaths,
}

// --- the shared client ------------------------------------------------------------------------

/// One reconcile client per venue-symbol, routed `Spot`/`Perp`, generic over the venue's
/// `Signer`/`RestTransport` seam and carrying only what a report fetch needs (signer + transport +
/// base_url + symbol + the venue [`ReconcileSpec`]; no order-formatting `SymbolProperties`). Each venue
/// wraps this behind its own named type (`ReconcileClientBinance`/`ReconcileClientAster`) pinned to its
/// spec.
pub enum ReconcileClientBinanceFamily<S: Signer, T: RestTransport> {
    Spot {
        spec: ReconcileSpec,
        signer: S,
        transport: T,
        base_url: String,
        symbol: String,
        /// **The account id, remembered after whichever call first pulls the account body.**
        ///
        /// ⚠ It and `pairs` (a pair's base coin) are the ONLY values on this client that may be cached, and the
        /// rule is IMMUTABILITY — an account's id does not change for the life of a key, while a balance changes every
        /// second and a fee schedule changes when a tariff does. `vike_execution::recon::ReconcileClient::
        /// fetch_account_identity`'s doc carries the whole argument, including why caching the
        /// BALANCE would silently retire the startup preflight's expired-key check.
        ///
        /// `OnceLock` rather than a `Mutex` because the value is written once and read many times,
        /// and because `ReconcileClient` takes `&self`: the fetches are `&self` methods on a client the
        /// driver holds by reference.
        ///
        /// The outer `Option` is the ANSWER (`None` = the venue named no account); the `OnceLock`'s
        /// own emptiness is *nobody has asked yet*. Two different facts, which is why this is not a
        /// bare `OnceLock<String>`.
        identity: std::sync::OnceLock<Option<String>>,
        /// The symbols the engine admitted beyond `symbol` (`ReconcileClient::admit_symbol`), bare, as
        /// the spot engine spells them. One list, shareable with the exec thread (`with_admitted`).
        admitted: Arc<ReconcileAdmittedSymbols>,
        /// The pass-wide pause after a rate limit on an admitted read.
        pause: RateGatePause,
        /// Each spot pair's base coin and `LOT_SIZE` step ([`parse_spot_pair`]), read once per pair: the same
        /// immutability rule as `identity` (a pair's base coin never changes; a step the venue moved later only
        /// shifts what dust counts as flat), and it keeps the position read to ONE account request a pass.
        pairs: Mutex<HashMap<String, (String, f64)>>,
        /// The broker link id the account's orders carry (`with_link_id`); `None` = none configured.
        link_id: Option<String>,
    },
    Perp {
        spec: ReconcileSpec,
        signer: S,
        transport: T,
        base_url: String,
        symbol: String,
        /// The perp lane's twin of `Spot::identity`. ⚠ It stays EMPTY today: the perp account
        /// endpoint is not one this client reads for identity, so `fetch_account_identity` answers
        /// from the spot lane alone. Carried here so the two variants stay one shape and a perp
        /// implementation is a fill-in rather than a struct change.
        identity: std::sync::OnceLock<Option<String>>,
        /// The symbols the engine admitted beyond `symbol` (`ReconcileClient::admit_symbol`), as the
        /// perp engine spells them: the series label, `.P` included. One list, shareable with the exec
        /// thread (`with_admitted`).
        admitted: Arc<ReconcileAdmittedSymbols>,
        /// The pass-wide pause after a rate limit on an admitted read.
        pause: RateGatePause,
        /// `(exchange symbol, orderId)` -> whether the venue's order is a liquidation one, as its
        /// stored `clientOrderId` says (`without_liquidations`). Kept across passes: an order's
        /// client id never changes, so each fill order costs ONE lookup for the life of the client.
        liquidation_orders: Mutex<HashMap<(String, String), bool>>,
        /// The spot lane's twin.
        link_id: Option<String>,
    },
}

impl<S: Signer, T: RestTransport> ReconcileClientBinanceFamily<S, T> {
    pub fn spot(
        spec: ReconcileSpec,
        signer: S,
        transport: T,
        base_url: impl Into<String>,
        symbol: impl Into<String>,
    ) -> Self {
        ReconcileClientBinanceFamily::Spot {
            spec,
            signer,
            transport,
            base_url: base_url.into(),
            symbol: symbol.into(),
            identity: std::sync::OnceLock::new(),
            admitted: Arc::default(),
            pause: RateGatePause::default(),
            pairs: Mutex::default(),
            link_id: None,
        }
    }

    pub fn perp(
        spec: ReconcileSpec,
        signer: S,
        transport: T,
        base_url: impl Into<String>,
        symbol: impl Into<String>,
    ) -> Self {
        ReconcileClientBinanceFamily::Perp {
            spec,
            signer,
            transport,
            base_url: base_url.into(),
            symbol: symbol.into(),
            identity: std::sync::OnceLock::new(),
            admitted: Arc::default(),
            pause: RateGatePause::default(),
            liquidation_orders: Mutex::default(),
            link_id: None,
        }
    }

    fn spec(&self) -> &ReconcileSpec {
        match self {
            ReconcileClientBinanceFamily::Spot { spec, .. }
            | ReconcileClientBinanceFamily::Perp { spec, .. } => spec,
        }
    }

    /// The identity cell for this lane.
    fn identity_cell(&self) -> &std::sync::OnceLock<Option<String>> {
        match self {
            ReconcileClientBinanceFamily::Spot { identity, .. }
            | ReconcileClientBinanceFamily::Perp { identity, .. } => identity,
        }
    }

    /// **Remember the account id out of a `/api/v3/account` body somebody else just fetched.**
    ///
    /// Called by every read of that body — `fetch_balance`'s spot arm and `fetch_fee_rates`' spot
    /// account arm — rather than by one designated owner, because **neither is guaranteed to run
    /// first**: a startup preflight reaches the balance, a live start reaches the fee schedule, and
    /// which of them a given process performs first is not this client's to know.
    ///
    /// ⚠ `set` on an already-filled cell is a NO-OP and that is exactly right: an account id does
    /// not change for the life of a key, so the first answer is the answer, and a later body cannot
    /// contradict it. (If one ever did, the store's own `BookAlreadyKnown` refusal is what reports
    /// it — not a silent overwrite here.)
    ///
    /// It writes even when the body names NO account: `Some(None)` records *asked, and the venue
    /// said nothing*, which is a different fact from the cell being empty (*nobody asked yet*) and
    /// stops a second request being made to re-learn the same silence.
    fn remember_identity(&self, body: &str) {
        let parsed = serde_json::from_str::<serde_json::Value>(body)
            .ok()
            .as_ref()
            .and_then(crate::execution::rest::spot::parse_account_identity);
        let _ = self.identity_cell().set(parsed);
    }

    fn venue(&self) -> &'static str {
        self.spec().venue
    }

    fn paths(&self) -> ReconcilePaths {
        self.spec().paths
    }

    // `pub(crate)`: binance's `ReconcileClientBinance` reads the Algo Service's open list beside the
    // family's `openOrders` (`crate::execution::rest::algo::fetch_open_algo_reports`) through this and the two below.
    // A visibility change only: aster is another crate and sees nothing.
    pub(crate) fn symbol(&self) -> &str {
        match self {
            ReconcileClientBinanceFamily::Spot { symbol, .. }
            | ReconcileClientBinanceFamily::Perp { symbol, .. } => symbol,
        }
    }

    /// The symbols the engine admitted beyond the mounted one.
    fn admitted(&self) -> &ReconcileAdmittedSymbols {
        match self {
            ReconcileClientBinanceFamily::Spot { admitted, .. }
            | ReconcileClientBinanceFamily::Perp { admitted, .. } => admitted,
        }
    }

    /// The broker link id the account's orders carry, as [`Self::with_link_id`] told it.
    pub fn link_id(&self) -> Option<&str> {
        match self {
            ReconcileClientBinanceFamily::Spot { link_id, .. }
            | ReconcileClientBinanceFamily::Perp { link_id, .. } => link_id.as_deref(),
        }
    }

    /// Tell the client the broker link id the account's orders are placed under (binance's start knows
    /// it; aster has none). Every echoed client id this client reads goes through
    /// `crate::family::execution::order_map`'s `strip_broker_order_client_id_prefix` with it.
    #[must_use]
    pub fn with_link_id(mut self, id: Option<String>) -> Self {
        match &mut self {
            ReconcileClientBinanceFamily::Spot { link_id, .. }
            | ReconcileClientBinanceFamily::Perp { link_id, .. } => {
                *link_id = id;
            }
        }
        self
    }

    /// Keep the admitted-symbol list in `shared` instead of a private one, so the holder of the other end
    /// (the exec thread's resync and gap-sentinel) sees every symbol this client is told.
    #[must_use]
    pub fn with_admitted(mut self, shared: Arc<ReconcileAdmittedSymbols>) -> Self {
        match &mut self {
            ReconcileClientBinanceFamily::Spot { admitted, .. }
            | ReconcileClientBinanceFamily::Perp { admitted, .. } => {
                *admitted = shared;
            }
        }
        self
    }

    /// The pass-wide pause.
    fn pause(&self) -> &RateGatePause {
        match self {
            ReconcileClientBinanceFamily::Spot { pause, .. }
            | ReconcileClientBinanceFamily::Perp { pause, .. } => pause,
        }
    }

    /// The mounted symbol as the ENGINE spells it: the series label on the perp lane (`BTCUSDT.P`),
    /// the bare symbol on spot. This is what an admitted symbol is compared and tagged against.
    fn mounted_label(&self) -> String {
        match self {
            ReconcileClientBinanceFamily::Spot { symbol, .. } => symbol.clone(),
            ReconcileClientBinanceFamily::Perp { symbol, .. } => {
                crate::family::perp_series_symbol(symbol)
            }
        }
    }

    /// Each admitted symbol as `(the engine's label, the exchange symbol a request names)`.
    pub(crate) fn admitted_targets(&self) -> Vec<(String, String)> {
        self.admitted()
            .symbols()
            .into_iter()
            .map(|label| {
                let exchange = vike_venues::split_perp(&label).0.to_string();
                (label, exchange)
            })
            .collect()
    }

    /// `openOrders` for ONE exchange symbol: the request the client has always made for its own.
    fn open_orders_of(&self, symbol: &str) -> Result<Vec<OrderStatusReport>, ReconcileReadError> {
        let paths = self.paths();
        let venue = self.venue();
        let params = [("symbol", symbol.to_string())];
        match self {
            ReconcileClientBinanceFamily::Spot { .. } => {
                let body = self.signed_raw(paths.spot_open_orders, &params)?;
                parse_spot_open_orders(&body, venue, self.link_id())
                    .map_err(ReconcileReadError::Parse)
            }
            ReconcileClientBinanceFamily::Perp { .. } => {
                let body = self.signed_raw(paths.perp_open_orders, &params)?;
                parse_perp_open_orders(&body, venue, self.link_id())
                    .map_err(ReconcileReadError::Parse)
            }
        }
    }

    /// `myTrades` / `userTrades` for ONE exchange symbol since `since` (0 = no lower bound), EVERY page
    /// of it ([`Self::trade_pages`]). `checks` is the pass's by-id order reads ([`OrderChecks`]).
    fn fills_of(
        &self,
        symbol: &str,
        since: i64,
        checks: &mut OrderChecks,
    ) -> Result<Vec<FillReport>, ReconcileReadError> {
        let venue = self.venue();
        let paths = self.paths();
        match self {
            ReconcileClientBinanceFamily::Spot { .. } => {
                self.trade_pages(paths.spot_my_trades, symbol, since, |body| {
                    parse_spot_my_trades(body, venue)
                })
            }
            ReconcileClientBinanceFamily::Perp { .. } => {
                let fills = self.trade_pages(paths.perp_user_trades, symbol, since, |body| {
                    parse_perp_user_trades(body, venue)
                })?;
                self.without_liquidations(symbol, since, fills, checks)
            }
        }
    }

    /// Every page of one symbol's trade read at `path` (owner ruling F-2: it used to be ONE page with no
    /// `limit`, the venue's 500 by default, read as the whole window). The first page is `startTime = since`
    /// (none at 0) and [`TRADES_PAGE_LIMIT`]; while a page comes back FULL (by its row count, before `parse`
    /// drops an id-less row), the next is `fromId` one past the greatest trade `id` read (the venues take
    /// `fromId` alone, never with `startTime`: it answers the trades from that id on, oldest first). A full
    /// page with no `id`, a page that moves the cursor nowhere and [`TRADES_MAX_PAGES`] pages fail the read.
    /// ⚠ NOT measured on a venue past one page (no account has traded a full page in one window).
    fn trade_pages(
        &self,
        path: &str,
        symbol: &str,
        since: i64,
        parse: impl Fn(&str) -> Result<Vec<FillReport>, String>,
    ) -> Result<Vec<FillReport>, ReconcileReadError> {
        let refused = ReconcileReadError::Parse;
        let mut fills = Vec::new();
        let mut from_id: Option<i64> = None;
        for _ in 0..TRADES_MAX_PAGES {
            let mut params: Vec<(&str, String)> = vec![("symbol", symbol.to_string())];
            match from_id {
                Some(id) => params.push(("fromId", id.to_string())),
                None if since > 0 => params.push(("startTime", since.to_string())),
                None => {}
            }
            params.push(("limit", TRADES_PAGE_LIMIT.to_string()));
            let body = self.signed_requery_raw(path, &params)?;
            fills.extend(parse(&body).map_err(refused)?);
            let ids = parse_rows(&body, |t| t.get("id").and_then(json_int)).map_err(refused)?;
            if ids.len() < TRADES_PAGE_LIMIT {
                return Ok(fills);
            }
            let Some(next) = ids.into_iter().flatten().max().map(|id| id + 1) else {
                return Err(refused(format!(
                    "{path} for {symbol}: a full page of {TRADES_PAGE_LIMIT} rows carries no trade id \
                     to ask the next page from: the read is refused rather than reported as complete"
                )));
            };
            if from_id.is_some_and(|asked| next <= asked) {
                return Err(refused(format!(
                    "{path} for {symbol}: the page from trade id {} ended no further on: the read is \
                     refused rather than reported as complete",
                    from_id.unwrap_or_default()
                )));
            }
            from_id = Some(next);
        }
        Err(refused(format!(
            "{path} for {symbol} is still paging after {TRADES_MAX_PAGES} pages of \
             {TRADES_PAGE_LIMIT} rows: the read is refused rather than reported as complete"
        )))
    }

    /// `fills` (one symbol's `userTrades` since `since`) less every trade of a venue LIQUIDATION
    /// order (owner, 2026-11-09: liquidation trades are listed in userTrades). No `userTrades` row
    /// marks a liquidation (it carries `orderId` and no client id), and the user-data stream books
    /// such a trade as a `PositionLiquidated`, deduped by the engine's `seen_liq_ids`, which the fill
    /// diff never reads: reported as a fill it would be a `MissingFill` that `hybrid` books, closing
    /// the position twice. A liquidation the stream missed is not lost by this: `positionRisk` still
    /// shows the position gone and the position diff reports it. Two signals, in order:
    ///
    /// 1. that symbol's `forceOrders` rows (the liquidation and ADL orders), by `orderId`. Weight 20
    ///    (`GET /fapi/v1/forceOrders` with a symbol; aster's `/fapi/v3/forceOrders` and dapi's
    ///    `/dapi/v1/forceOrders` document the same).
    /// 2. every OTHER fill order, by id ([`Self::is_liquidation_order`]): its stored `clientOrderId`
    ///    judged by the stream's own prefix list, which is what catches a `settlement_autoclose-`
    ///    order (delisting, delivery), absent from `forceOrders`. This client holds no list of our
    ///    own orders, so every fill order is asked about, once for the life of the client.
    ///
    /// ⚠ Strict: a failed or unparseable read of either fails this read (never the rows
    /// unfiltered), and so does a page of [`FORCE_ORDERS_LIMIT`] rows, which may have cut the list.
    /// The by-id reads are capped at [`ORDER_LOOKUPS_PER_PASS`] a pass (`checks`, shared by every
    /// symbol of the pass): the fills of an order past it are HELD BACK, not reported, and counted in
    /// `checks.held`; a later pass reports them once their order is checked. Holding a fill back is
    /// safe, where reporting it unchecked is not: reconcile raises a divergence for a reported fill it
    /// does not know, never for an absent one. No request when there are no fills to filter.
    fn without_liquidations(
        &self,
        symbol: &str,
        since: i64,
        mut fills: Vec<FillReport>,
        checks: &mut OrderChecks,
    ) -> Result<Vec<FillReport>, ReconcileReadError> {
        if fills.is_empty() {
            return Ok(fills);
        }
        let mut params: Vec<(&str, String)> =
            vec![("symbol", symbol.to_string()), ("limit", FORCE_ORDERS_LIMIT.to_string())];
        // a minute early: a trade after `since` may belong to an order placed before it
        if since > FORCE_ORDERS_LEAD_MS {
            params.push(("startTime", (since - FORCE_ORDERS_LEAD_MS).to_string()));
        }
        let path = self.paths().perp_force_orders;
        let body = self.signed_requery_raw(path, &params)?;
        let forced =
            parse_rows(&body, |o| o.get("orderId").map(json_id).filter(|id| !id.is_empty()))
                .map_err(|e| ReconcileReadError::Parse(format!("{path}: {e}")))?
                .into_iter()
                .collect::<Option<Vec<String>>>()
                .ok_or_else(|| {
                    ReconcileReadError::Parse(format!("{path}: a row carries no orderId"))
                })?;
        if forced.len() >= FORCE_ORDERS_LIMIT {
            return Err(ReconcileReadError::Parse(format!(
                "{path}: {} rows, the page limit, so the liquidation list may be cut; the fills are \
                 not reported unfiltered",
                forced.len()
            )));
        }
        fills.retain(|f| !forced.iter().any(|id| id == f.venue_order_id.as_str()));
        let mut window = std::collections::HashSet::new();
        let orders: Vec<&str> = fills
            .iter()
            .map(|f| f.venue_order_id.as_str())
            .filter(|id| window.insert(*id))
            .collect();
        if let ReconcileClientBinanceFamily::Perp { liquidation_orders, .. } = self {
            // an order that left the fills window is never asked about again: forget it
            let mut memory = liquidation_orders.lock().unwrap_or_else(PoisonError::into_inner);
            memory.retain(|(s, id), _| s != symbol || window.contains(id.as_str()));
        }
        let mut withheld: Vec<String> = Vec::new();
        for id in orders {
            match self.is_liquidation_order(symbol, id, &mut checks.left)? {
                Some(false) => {}
                // a venue liquidation order: its trades are never reported
                Some(true) => withheld.push(id.to_string()),
                // not checked yet (the pass's reads are spent): held back until a pass checks it
                None => {
                    checks.held += 1;
                    withheld.push(id.to_string());
                }
            }
        }
        fills.retain(|f| !withheld.iter().any(|id| id == f.venue_order_id.as_str()));
        Ok(fills)
    }

    /// Is order `id` of exchange symbol `symbol` a venue liquidation order? `GET /fapi/v1/order`
    /// (weight 1; aster `/fapi/v3/order`, dapi `/dapi/v1/order`) and its stored `clientOrderId`
    /// through [`crate::family::execution::event_mapper::perp::is_liquidation_order_client_id`], the ONE prefix list the stream
    /// mapper reads too. The answer is remembered (`liquidation_orders`), so a second fill of the
    /// order, or the next pass, asks nothing. `None` (not asked) when the pass's `lookups` are spent.
    ///
    /// "No such order" (`-2013`) is NOT one: the venue documents that an order with a filled trade is
    /// found unless it was created 90 days ago or more (binance; aster: always), and a liquidation or
    /// settlement order is created at its trade. Any other failure is an error, never an answer.
    fn is_liquidation_order(
        &self,
        symbol: &str,
        id: &str,
        lookups: &mut usize,
    ) -> Result<Option<bool>, ReconcileReadError> {
        let ReconcileClientBinanceFamily::Perp { liquidation_orders, .. } = self else {
            return Ok(Some(false));
        };
        let lock = || liquidation_orders.lock().unwrap_or_else(PoisonError::into_inner);
        let key = (symbol.to_string(), id.to_string());
        if let Some(known) = lock().get(&key) {
            return Ok(Some(*known));
        }
        let Some(left) = lookups.checked_sub(1) else { return Ok(None) };
        *lookups = left;
        let path = self.paths().perp_order;
        let params = [("symbol", symbol.to_string()), ("orderId", id.to_string())];
        let liquidation = match self.signed_requery_raw(path, &params) {
            Ok(body) => {
                let order: serde_json::Value = serde_json::from_str(&body)
                    .map_err(|e| ReconcileReadError::Parse(format!("{path}: {e}")))?;
                let Some(stored) = order.get("clientOrderId").and_then(|c| c.as_str()) else {
                    return Err(ReconcileReadError::Parse(format!(
                        "{path}: order {id} has no clientOrderId"
                    )));
                };
                crate::family::execution::event_mapper::perp::is_liquidation_order_client_id(
                    crate::family::execution::order_map::strip_broker_order_client_id_prefix(
                        stored,
                        self.link_id(),
                    ),
                )
            }
            Err(e) if crate::transport::error_codes::says_order_unknown(&e) => false,
            Err(e) => return Err(e.into()),
        };
        let mut memory = lock();
        if memory.len() >= LIQUIDATION_ORDER_MEMORY {
            memory.clear(); // a bounded memory: forgotten orders are asked again, once each
        }
        memory.insert(key, liquidation);
        Ok(Some(liquidation))
    }

    /// ONE spot pair's base coin and `LOT_SIZE` step: remembered in `pairs`, else its public `exchangeInfo` row.
    fn spot_pair(
        &self,
        pairs: &Mutex<HashMap<String, (String, f64)>>,
        symbol: &str,
    ) -> Result<(String, f64), ReconcileReadError> {
        let key = symbol.to_ascii_uppercase();
        if let Some(known) = pairs.lock().unwrap_or_else(PoisonError::into_inner).get(&key) {
            return Ok(known.clone());
        }
        let (_, transport, base_url) = self.parts();
        let path = self.paths().spot_exchange_info;
        let info = transport.public(base_url, path, &[("symbol", key.clone())])?;
        let pair = parse_spot_pair(&info, &key).ok_or_else(|| {
            ReconcileReadError::Parse(format!("{path}: no row naming the base coin of {key}"))
        })?;
        pairs.lock().unwrap_or_else(PoisonError::into_inner).insert(key, pair.clone());
        Ok(pair)
    }

    /// `positionRisk` for ONE exchange symbol (perp only; a spot pair's holding is `spot_pair` + the account).
    fn positions_of(&self, symbol: &str) -> Result<Vec<PositionStatusReport>, ReconcileReadError> {
        let venue = self.venue();
        let body =
            self.signed_raw(self.paths().perp_positions, &[("symbol", symbol.to_string())])?;
        parse_perp_position_risk(&body, venue).map_err(ReconcileReadError::Parse)
    }

    /// Judge the read of ONE admitted symbol by the family's rule ([`ADMITTED_READ`], the shared judgement): a venue that
    /// no longer lists the symbol (`-1121`) holds nothing on it, so that is no rows, with a warning, and the account's other
    /// reads still go through (one delisted symbol must not blind the pass for good). A rate limit starts the pass-wide
    /// pause. Anything else fails the whole fetch, as before.
    ///
    /// `pub(crate)` (visibility only, like `symbol`/`admitted_targets`/`signed_raw` above): binance's
    /// Algo Service read goes through THIS judgement so it shares the rate-limit pause.
    pub(crate) fn admitted_rows<R>(
        &self,
        label: &str,
        read: Result<Vec<R>, ReconcileReadError>,
    ) -> Result<Vec<R>, String> {
        ADMITTED_READ.admitted_rows(self.venue(), label, self.pause(), read)
    }

    /// `Err` while the pass is paused after a rate limit on an admitted symbol. Only a client with
    /// admitted symbols ever pauses, so with none this is always `Ok`.
    ///
    /// `pub(crate)` (visibility only, like `admitted_rows` above): binance's by-id naming stops asking
    /// once a rate limit has paused the pass.
    pub(crate) fn not_backing_off(&self) -> Result<(), String> {
        not_backing_off(self.venue(), self.pause())
    }

    fn parts(&self) -> (&S, &T, &str) {
        match self {
            ReconcileClientBinanceFamily::Spot { signer, transport, base_url, .. }
            | ReconcileClientBinanceFamily::Perp { signer, transport, base_url, .. } => {
                (signer, transport, base_url)
            }
        }
    }

    /// The normal signed GET path for account/position reads (rides the venue's order-rate gate,
    /// if any).
    fn signed(&self, path: &str, params: &[(&str, String)]) -> Result<String, String> {
        self.signed_raw(path, params).map_err(|e| e.to_string())
    }

    /// [`Self::signed`] keeping the venue's error (code and message), for a read that judges it.
    pub(crate) fn signed_raw(
        &self,
        path: &str,
        params: &[(&str, String)],
    ) -> Result<String, VenueApiError> {
        let (signer, transport, base_url) = self.parts();
        transport.signed(base_url, path, "GET", params, signer).map(|v| v.to_string())
    }

    /// The short-timeout, rate-gate-EXEMPT path — the SAME one the crate's audit-A3 resync
    /// (`get_my_trades`/`get_user_trades`) already uses for these exact two heavier history
    /// endpoints, so a resync burst can't blow the per-IP `ORDERS` budget submit/cancel depend on.
    ///
    /// `pub(crate)` (visibility only, like `signed_raw` above): binance's Algo Service fill read
    /// (`crate::execution::rest::algo::name_algo_children`) rides it for the same reason the fill scans do.
    pub(crate) fn signed_requery_raw(
        &self,
        path: &str,
        params: &[(&str, String)],
    ) -> Result<String, VenueApiError> {
        let (signer, transport, base_url) = self.parts();
        transport.signed_requery(base_url, path, "GET", params, signer).map(|v| v.to_string())
    }
}

/// The `limit` a `forceOrders` read asks for: the documented maximum ("Default 50; max 100").
const FORCE_ORDERS_LIMIT: usize = 100;

/// Rows asked per `myTrades` / `userTrades` request: the documented maximum ("Default 500; max 1000") of
/// both venues' spot and futures trade reads.
const TRADES_PAGE_LIMIT: usize = 1000;

/// Pages one symbol's fill read follows before it FAILS: 10 x 1000 = 10 000 trades in one reconcile window (a
/// day by default), far above what this account trades on one symbol.
const TRADES_MAX_PAGES: usize = 10;

/// How far before the fills window the `forceOrders` window opens (ms).
const FORCE_ORDERS_LEAD_MS: i64 = 60_000;

/// How many fill orders' liquidation verdicts a client remembers before it starts over (a backstop:
/// each symbol's read forgets the orders that left its window). More than one pass can hold
/// (seventeen symbols of at most 500 `userTrades` rows, the default page), or a pass could never get
/// through its orders.
const LIQUIDATION_ORDER_MEMORY: usize = 10_000;

/// The by-id order reads (`GET /fapi/v1/order`, weight 1) one fill fetch may make, all symbols
/// together: the IP weight budget is shared with every other read the node makes.
const ORDER_LOOKUPS_PER_PASS: usize = 100;

/// One fill fetch's by-id order reads: how many it may still make ([`ORDER_LOOKUPS_PER_PASS`]), and
/// how many fill orders it held back unchecked (logged once a pass).
struct OrderChecks {
    left: usize,
    held: usize,
}

/// Binance's `-1121 INVALID_SYMBOL` (the venue does not list the symbol). Aster is a byte-identical
/// fork of this grammar and is presumed to answer the same; neither is live-proven for a delisted
/// symbol.
const BAD_SYMBOL: i64 = -1121;

/// The family's judgement of an admitted symbol's read (`vike_bridge_core::reconcile_admitted`): [`BAD_SYMBOL`] is "not
/// listed", the transport's rate-limit kind starts the pause, and a venue error's text is its whole `Display`, the text
/// the mounted symbol's read has always produced.
const ADMITTED_READ: ReconcileAdmittedReadSpec = ReconcileAdmittedReadSpec {
    not_listed: |e| e.code == BAD_SYMBOL,
    rate_limited: |e| e.kind() == ErrorKind::RateLimited,
    text: |e| e.to_string(),
};

/// A failed read's text in the pass error: the venue error's whole `Display`, a parse failure as written.
pub(crate) fn read_text(e: ReconcileReadError) -> String {
    e.into_text(ADMITTED_READ.text)
}

impl<S: Signer, T: RestTransport + Send> ReconcileClient for ReconcileClientBinanceFamily<S, T> {
    /// `since` is a no-op here: `openOrders` has no time filter and reports only the CURRENTLY open
    /// set — there's nothing to filter it by, and a closed order that fell out of this set is
    /// exactly what the fill/position reports (not this one) reconcile.
    ///
    /// **Every ADMITTED symbol is read too**, one `openOrders` per symbol with the mounted symbol's
    /// request shape (weight 1 each on fapi, at most sixteen, against 40 for the account-wide form),
    /// and any failure fails the whole fetch: the restore planner reads an order a successful report
    /// lacks as gone. With none admitted the request list is the mounted symbol's alone.
    fn fetch_order_status_reports(&self, _since: i64) -> Result<Vec<OrderStatusReport>, String> {
        self.not_backing_off()?;
        let mut reports = self.open_orders_of(self.symbol()).map_err(read_text)?;
        for (label, symbol) in self.admitted_targets() {
            let rows = self.admitted_rows(&label, self.open_orders_of(&symbol))?;
            reports.extend(rows.into_iter().filter(|r| r.symbol.eq_ignore_ascii_case(&label)));
        }
        Ok(reports)
    }

    /// The mounted symbol's fills as always, then each admitted symbol's. An admitted symbol's fill
    /// carries the SYMBOL-QUALIFIED trade id (`family::fill_trade_id`) that the user-data stream
    /// spells for a symbol other than the mounted one, so a fill the stream already folded is a
    /// KNOWN trade id to the diff and two symbols' per-symbol `id` sequences never collide.
    fn fetch_fill_reports(&self, since: i64) -> Result<Vec<FillReport>, String> {
        self.not_backing_off()?;
        let mut checks = OrderChecks { left: ORDER_LOOKUPS_PER_PASS, held: 0 };
        let mut reports = self.fills_of(self.symbol(), since, &mut checks).map_err(read_text)?;
        let mounted = self.mounted_label();
        for (label, symbol) in self.admitted_targets() {
            for mut fill in
                self.admitted_rows(&label, self.fills_of(&symbol, since, &mut checks))?
            {
                if !fill.symbol.eq_ignore_ascii_case(&label) {
                    continue;
                }
                if let Ok(id) =
                    crate::family::fill_trade_id(fill.trade_id.as_str(), &fill.symbol, &mounted)
                {
                    fill.trade_id = id;
                }
                reports.push(fill);
            }
        }
        if checks.held > 0 {
            tracing::info!(
                venue = self.venue(),
                "reconcile: {} orders still being checked; their fills are held back",
                checks.held
            );
        }
        Ok(reports)
    }

    /// Spot has no venue-native position report: a pair's holding is its BASE coin's wallet figure (the spot
    /// rule, owner 2026-11-11), one `Cash` FLOOR row with no price per pair ([`parse_spot_holding`]): the
    /// mounted pair's, then each admitted pair's (its base coin and step remembered from one public
    /// `exchangeInfo` read, a pair the venue no longer lists read as no row), all from ONE account read. The
    /// coins beyond what the engine bought are the user's and raise nothing (`vike_execution::recon::diff`).
    /// Perp: the mounted symbol's `positionRisk`, then each admitted
    /// symbol's with the same per-symbol request (the row stays present when the leg is flat).
    fn fetch_position_status_reports(&self) -> Result<Vec<PositionStatusReport>, String> {
        match self {
            ReconcileClientBinanceFamily::Spot { pairs, .. } => {
                self.not_backing_off()?;
                let mounted = self.symbol().to_string();
                let mut held =
                    vec![(mounted.clone(), self.spot_pair(pairs, &mounted).map_err(read_text)?)];
                for (label, symbol) in self.admitted_targets() {
                    let pair = self.spot_pair(pairs, &symbol).map(|p| vec![(label.clone(), p)]);
                    held.extend(self.admitted_rows(&label, pair)?);
                }
                let body = self.signed(self.paths().spot_account, &[])?;
                self.remember_identity(&body);
                let ts = vike_model::now_ms();
                held.iter()
                    .map(|(label, (base, step))| {
                        parse_spot_holding(&body, label, base, *step, self.venue(), ts)
                    })
                    .collect()
            }
            ReconcileClientBinanceFamily::Perp { .. } => {
                self.not_backing_off()?;
                let mut reports = self.positions_of(self.symbol()).map_err(read_text)?;
                for (label, symbol) in self.admitted_targets() {
                    let rows = self.admitted_rows(&label, self.positions_of(&symbol))?;
                    reports
                        .extend(rows.into_iter().filter(|r| r.symbol.eq_ignore_ascii_case(&label)));
                }
                Ok(reports)
            }
        }
    }

    /// Remember that the engine admitted `symbol`, so every later pass reads it too. A symbol of the
    /// OTHER lane (a `.P` label on the spot client, a bare one on the perp client), the mounted
    /// symbol, an empty one, a repeat and a 17th are ignored (`ReconcileAdmittedSymbols::admit`), and an added one is logged once.
    fn admit_symbol(&self, symbol: &str) {
        let label = symbol.to_ascii_uppercase();
        let is_perp_label = vike_venues::split_perp(&label).1;
        if is_perp_label != matches!(self, ReconcileClientBinanceFamily::Perp { .. }) {
            tracing::debug!(
                venue = self.venue(), symbol = %label,
                "reconcile: ignoring an admitted symbol of the other lane"
            );
            return;
        }
        self.admitted().admit_logged(&label, &self.mounted_label(), self.venue(), "a symbol");
    }

    /// Forget a symbol the engine gave back (decision 0205), in the spelling it was told in (the label, upper-cased).
    /// A label of the other lane was never held, so it is simply not found.
    fn forget_symbol(&self, symbol: &str) {
        let label = symbol.to_ascii_uppercase();
        self.admitted().forget_logged(&label, self.venue(), "a symbol");
    }

    /// The symbols this client was told and reads on every pass beyond the mounted one, in the engine's
    /// spelling (`.P` on the perp lane): a real claim, which the gatewayhub names in its reconcile reply
    /// (`gw_trade_reconcile_read`) so the node claims only what the daemon's client reads.
    fn admitted_symbols(&self) -> Option<Vec<String>> {
        Some(self.admitted().symbols())
    }

    /// Balance meaning: PERP = the USDT wallet `balance` from the perp balance endpoint
    /// ([`parse_perp_balance`]); SPOT = the USDT TOTAL, `free + locked`, from
    /// `/api/v3/account` (includes what a resting BUY locks: [`parse_spot_balance`]). Both
    /// endpoints are light, unfiltered account reads — ride the normal `signed` (rate-gated) path,
    /// never `signed_requery`.
    /// **WHICH ACCOUNT this key trades** — `uid`, measured 2026-09-20 on the live demo endpoint
    /// (`crates/bridges/binance/tests/binance_smokes/binance_identity_probe.rs`), never read off the venue's docs.
    ///
    /// ⚠ **It costs NO request when anything has already read the account body**, which on a live
    /// box is always: the startup preflight reaches it through `fetch_balance` and a live start
    /// through `fetch_fee_rates`. Only a client that has done neither pays one GET here.
    ///
    /// The SPOT lane alone answers. Perp carries the same cell so the two variants stay one shape,
    /// but nothing fills it — its account endpoint is not one this client reads for identity — so a
    /// perp-only client answers `None`, which is *not yet known* rather than *no account*.
    fn fetch_account_identity(&self) -> Result<Option<String>, String> {
        if let Some(cached) = self.identity_cell().get() {
            return Ok(cached.clone());
        }
        match self {
            ReconcileClientBinanceFamily::Spot { .. } => {
                let body = self.signed(self.paths().spot_account, &[])?;
                self.remember_identity(&body);
                Ok(self.identity_cell().get().cloned().flatten())
            }
            // ⚠ No request at all, rather than a fetch that would answer nothing: the perp balance
            // body carries no account id, so asking for one would spend a call to learn a `None`.
            ReconcileClientBinanceFamily::Perp { .. } => Ok(None),
        }
    }

    fn fetch_balance(&self) -> Result<Option<f64>, String> {
        let paths = self.paths();
        match self {
            ReconcileClientBinanceFamily::Spot { .. } => {
                let body = self.signed(paths.spot_account, &[])?;
                // ⚠ The BALANCE is never cached — it changes every second, and this call is what
                // the startup preflight probes a key's liveness with. What IS remembered is the
                // account id riding the same body, because an id does not change for the life of a
                // key. See `remember_identity` and `ReconcileClient::fetch_account_identity`.
                self.remember_identity(&body);
                parse_spot_balance(&body)
            }
            ReconcileClientBinanceFamily::Perp { .. } => {
                let body = self.signed(paths.perp_balance, &[])?;
                parse_perp_balance(&body)
            }
        }
    }

    /// Live account-actual fee rates (fee model 4/5). Each lane picks ONE of two shapes from the
    /// venue's [`ReconcilePaths`], and both ride the normal `signed` (rate-gated) path:
    ///
    ///   - **SPOT.** [`ReconcilePaths::spot_commission_rate`] `Some(path)` ⇒ a per-symbol
    ///     `commissionRate` read through [`parse_commission_rate_body`] (Aster: its account body
    ///     carries no rates). `None` ⇒ `commissionRates` off `spot_account` via
    ///     [`parse_spot_fee_rates`] — the SAME body `fetch_balance` fetches, so it costs
    ///     no separate endpoint (Binance). The `None` arm is verbatim the pre-slot behavior.
    ///   - **PERP.** [`ReconcilePaths::perp_commission_rate`] `Some(path)` ⇒ the same per-symbol read;
    ///     `None` ⇒ the lane is INERT (`Ok(None)`, static default) and issues NO request at all,
    ///     which is how a venue whose commissionRate endpoint is unverified stays un-probed.
    ///
    /// Spot has no inert arm because a venue always has one of the two spot shapes; a venue with
    /// neither would set `spot_commission_rate: None` and its `spot_account` body simply carries no
    /// `commissionRates`, which `parse_spot_fee_rates` already answers `Ok(None)` for (fail-soft).
    fn fetch_fee_rates(&self) -> Result<Option<FeeSchedule>, String> {
        let paths = self.paths();
        match self {
            ReconcileClientBinanceFamily::Spot { .. } => match paths.spot_commission_rate {
                Some(path) => {
                    let body = self.signed(path, &[("symbol", self.symbol().to_string())])?;
                    parse_commission_rate_body(&body)
                }
                None => {
                    let body = self.signed(paths.spot_account, &[])?;
                    // The OTHER caller of the same body, and the reason the cache is written by
                    // whichever runs first rather than by a designated owner: a live start reaches
                    // here through `resolve_fee_schedule`, a startup reaches `fetch_balance`, and
                    // neither is guaranteed to be first.
                    self.remember_identity(&body);
                    parse_spot_fee_rates(&body)
                }
            },
            ReconcileClientBinanceFamily::Perp { .. } => match paths.perp_commission_rate {
                Some(path) => {
                    let body = self.signed(path, &[("symbol", self.symbol().to_string())])?;
                    parse_commission_rate_body(&body)
                }
                None => Ok(None),
            },
        }
    }
}
