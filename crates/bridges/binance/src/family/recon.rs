//! Shared Binance-grammar `ReconClient` core reused by vike-aster — the venue-facing report seam
//! (`vike_exec::recon::ReconClient`) for BOTH spot and USDⓈ-M perp. This is rung 5 of the family
//! (see [`crate::family`] mod doc): the pure report parsers AND the fetch/dispatch client, with
//! every per-venue delta — the endpoint paths and the venue string — passed in as parameters.
//!
//! Aster's spot+perp report shapes are byte-identical Binance forks, so both venues call straight
//! into this ONE copy. The sole wire divergences are the endpoint PATHS: Binance splits fapi across
//! `/fapi/v1/*` (orders/trades/commissionRate) and `/fapi/v2/*` (positionRisk/balance), while Aster
//! keeps every fapi path under a single `/fapi/v3/*` namespace. Those paths live in a caller-supplied
//! [`ReconSpec`] ([`ReconPaths`] + the `venue` label), exactly the same discipline as the market-data
//! ([`crate::family::UrlTable`]) and catalog rungs — nothing here decides which venue or which
//! endpoint it is serving.
//!
//! ⚠ One path slot encodes a real BODY divergence rather than a renamed route:
//! [`ReconPaths::spot_commission_rate`]. Binance's spot `/api/v3/account` carries
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
//!         position report, so `fetch_position_status_reports` is `Ok(vec![])` — spot "position" is
//!         a wallet balance, not a report row `diff::diff` understands.
//!   PERP: `openOrders` + `userTrades` + `positionRisk` (`positionAmt` is ALREADY SIGNED — long > 0,
//!         short < 0), EXCEPT every row is kept including a flat `qty == 0` leg: `recon::diff::diff`
//!         needs the report row PRESENT to detect "local still shows a position the venue has since
//!         closed" — filtering flat rows the way the old `ReconcileSnapshot` builder did would make
//!         that divergence invisible here.
//!
//! Every fetch delegates to a PURE free function (`parse_*`) over the raw JSON response body —
//! these are the fixture-tested units. Each venue re-exports them behind a thin 1-arg wrapper that
//! injects its own `venue`, so this shared code stays proven TWICE (once against Binance's golden
//! frames in `vike-binance`'s `tests/offline/recon_client_parse.rs`, once against Aster's).
//!
//! Gating precedent: `openOrders`/`positionRisk`/balance/commissionRate ride the normal `signed`
//! path (the same one `spot::connect`/`perp::reconcile_positions` use for account/position reads),
//! while the heavier `myTrades`/`userTrades` history scans ride `signed_requery` — the SAME
//! short-timeout, rate-gate-EXEMPT path the crate's audit-A3 resync already uses for these exact two
//! endpoints, so a resync burst can't blow the per-IP `ORDERS` budget submit/cancel depend on.

use vike_bridge_core::json::{json_num, parse_rows};
use vike_bridge_core::signer::Signer;
use vike_bridge_core::transport::RestTransport;
use vike_exec::recon::ReconClient;
use vike_model::events::{LiquiditySide, PositionSide, TradeId};
use vike_model::{FeeSchedule, FillReport, OrderStatusReport, PositionStatusReport};

use crate::spot::parse_commission_rates;

// --- pure parsers (venue supplied as a parameter) ----------------------------------------------

/// Binance/Aster ids arrive as JSON numbers or strings — twin of `spot::json_id`/`perp::json_id`.
fn json_id(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}

fn side_sign(s: &str) -> i32 {
    if s.eq_ignore_ascii_case("BUY") { 1 } else { -1 }
}

fn liquidity_side(maker: bool) -> LiquiditySide {
    if maker { LiquiditySide::Maker } else { LiquiditySide::Taker }
}

/// Normalize a raw venue order-status string to the `OrderStatus::parse` FSM vocabulary that
/// `recon::diff::diff` reads. `NEW` has no direct twin in our vocabulary other than `ACCEPTED`;
/// futures-only `EXPIRED_IN_MATCH` (an STP self-trade-prevention terminal) collapses to `EXPIRED`
/// (both are non-fill terminals). Every other status string already matches our spelling verbatim
/// (`PARTIALLY_FILLED`, `FILLED`, `CANCELED`, `REJECTED`, `EXPIRED`, `PENDING_CANCEL`).
fn normalize_order_status(raw: &str) -> String {
    match raw {
        "NEW" => "ACCEPTED",
        "EXPIRED_IN_MATCH" => "EXPIRED",
        other => other,
    }
    .to_string()
}

/// Shared row mapping for `openOrders` (spot AND perp share every field except how `avg_px` is
/// derived — see the two callers below). `clientOrderId` empty string normalizes to `None` (an
/// externally-placed order the venue never echoed our id for). Broker-prefix stripped (unified
/// cross-venue attribution, task 6 fix-round-1): `clientOrderId` is the SAME venue-stored id the WS
/// mappers decode, so it is passed through
/// [`crate::family::order_map::strip_broker_coid_prefix`] before `recon::diff::diff` compares it
/// against the local registry's bare coid — unconditionally safe (a no-op on an unprefixed id), and
/// load-bearing: without it, EVERY order carries `x-<link_id>-` once a link id is configured and
/// none match locally, so every live local order diffs as `OrphanLocalOrder` AND every venue order
/// as `UnknownOrder` — reconciliation goes blind on both sides at once. (⚠ This line used to say
/// `hybrid` "auto-cancels them all". It does not: `OrphanLocalOrder` folds zero events under every
/// policy — `crates/vike-exec/tests/recon/recon_policy_pin.rs`. The stripping is still required; the
/// failure mode is blindness, not destruction. It is at least no longer SILENT blindness: since the
/// orphan sweep gained an outcome, a whole book orphaning this way raises one aggregated
/// `OrphanLocalOrder` alert under `hybrid`/`quarantine`, which is the symptom to look for if a link
/// id is ever configured on a path that skips this stripping.)
fn parse_order_row(o: &serde_json::Value, avg_px: f64, venue: &str) -> OrderStatusReport {
    let symbol = o.get("symbol").and_then(|s| s.as_str()).unwrap_or("").to_string();
    let venue_order_id = o.get("orderId").map(json_id).unwrap_or_default();
    let client_order_id = o
        .get("clientOrderId")
        .and_then(|c| c.as_str())
        .filter(|c| !c.is_empty())
        .map(crate::family::order_map::strip_broker_coid_prefix)
        .map(|c| c.to_string());
    let side = side_sign(o.get("side").and_then(|s| s.as_str()).unwrap_or(""));
    let order_type = o.get("type").and_then(|t| t.as_str()).unwrap_or("").to_ascii_lowercase();
    let qty = o.get("origQty").and_then(json_num).unwrap_or(0.0);
    let filled_qty = o.get("executedQty").and_then(json_num).unwrap_or(0.0);
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
        client_order_id,
        side,
        order_type,
        qty,
        filled_qty,
        avg_px,
        status: normalize_order_status(raw_status),
        ts,
    }
}

/// `GET /api/v3/openOrders` rows -> `OrderStatusReport`. Spot carries no `avgPrice` field, so the
/// average fill price is derived from `cummulativeQuoteQty / executedQty` (0.0 when unfilled —
/// never a divide-by-zero).
pub fn parse_spot_open_orders(body: &str, venue: &str) -> Result<Vec<OrderStatusReport>, String> {
    parse_rows(body, |o| {
        let executed = o.get("executedQty").and_then(json_num).unwrap_or(0.0);
        let cum_quote = o.get("cummulativeQuoteQty").and_then(json_num).unwrap_or(0.0);
        let avg_px = if executed > 0.0 { cum_quote / executed } else { 0.0 };
        parse_order_row(o, avg_px, venue)
    })
}

/// `GET /fapi/v{1,3}/openOrders` rows -> `OrderStatusReport`. Perp echoes `avgPrice` directly.
pub fn parse_perp_open_orders(body: &str, venue: &str) -> Result<Vec<OrderStatusReport>, String> {
    parse_rows(body, |o| {
        let avg_px = o.get("avgPrice").and_then(json_num).unwrap_or(0.0);
        parse_order_row(o, avg_px, venue)
    })
}

/// Shared row mapping for `myTrades`/`userTrades` (spot and perp differ ONLY in how `side`/`maker`
/// are spelled on the wire — see the two callers below). Neither endpoint echoes a client order
/// id, so `client_order_id` is always `None` here (the caller is expected to resolve it from the
/// matching `venue_order_id` against local state, same as any other externally-sourced fill).
///
/// `None` for a row carrying no `id`: the callers DROP such a row rather than report it. A
/// `FillReport` with no `trade_id` is worse than a missing one, because `vike_exec::recon::diff`
/// decides "already known" by looking the id up in `seen_trade_ids` — an id-less row can never
/// match, so it FABRICATES a `MissingFill` divergence, and `MissingFill` is one of the two kinds the
/// `hybrid` policy AUTO-APPLIES (`vike_ops::reconcile_config::auto_applied_kinds`). It would book
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
    let last_qty = f.get("qty").and_then(json_num).unwrap_or(0.0);
    let last_px = f.get("price").and_then(json_num).unwrap_or(0.0);
    let commission = f.get("commission").and_then(json_num).unwrap_or(0.0);
    let commission_asset =
        f.get("commissionAsset").and_then(|c| c.as_str()).unwrap_or("").to_string();
    let ts = f.get("time").and_then(|t| t.as_i64()).unwrap_or(0);
    Some(FillReport {
        venue: venue.to_string(),
        symbol,
        trade_id,
        venue_order_id: venue_order_id.into(),
        client_order_id: None,
        side,
        last_qty,
        last_px,
        commission,
        commission_asset,
        liquidity_side: liquidity_side(maker),
        ts,
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
/// `vike_aster::recon_client`'s `ASTER_RECON`), so an `isBuyer`-only reader silently signs EVERY
/// aster spot fill as a SELL.
pub fn fill_side(f: &serde_json::Value) -> i32 {
    match f.get("side").and_then(|s| s.as_str()) {
        Some(s) if !s.is_empty() => side_sign(s),
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
/// `GET /api/v3/myTrades`, Aster `GET /api/v3/userTrades` (see [`ReconPaths::spot_my_trades`]) —
/// and so does the row grammar, which is why side/liquidity are read through the family union
/// [`fill_side`]/[`fill_is_maker`] rather than Binance's `isBuyer`/`isMaker` alone.
pub fn parse_spot_my_trades(body: &str, venue: &str) -> Result<Vec<FillReport>, String> {
    // `flatten` is the row DROP: an id-less row yields `None` and leaves the list — see
    // [`parse_fill_row`] for why reporting it would be worse than losing it.
    let rows = parse_rows(body, |f| parse_fill_row(f, fill_side(f), fill_is_maker(f), venue))?;
    Ok(rows.into_iter().flatten().collect())
}

/// `GET /fapi/v{1,3}/userTrades` rows -> `FillReport`. Perp spells side as the string `side`
/// (`"BUY"`/`"SELL"`) and the liquidity flag as `maker`; read through the same family union as its
/// spot twin above, so the two lanes cannot drift apart on a grammar the family shares.
pub fn parse_perp_user_trades(body: &str, venue: &str) -> Result<Vec<FillReport>, String> {
    // Same id-less row DROP as the spot twin above — see [`parse_fill_row`].
    let rows = parse_rows(body, |f| parse_fill_row(f, fill_side(f), fill_is_maker(f), venue))?;
    Ok(rows.into_iter().flatten().collect())
}

/// `GET /fapi/v{2,3}/positionRisk` rows -> `PositionStatusReport`. `positionAmt` is ALREADY SIGNED
/// (long > 0, short < 0) — never re-signed by `positionSide`. Every row is kept, including a flat
/// (`qty == 0`) leg — see the module doc for why filtering it out (as the older
/// `reconcile_positions` snapshot builder does) would hide a real divergence here.
///
/// Margin-mode step-2 (read-side only): `marginType` (`"cross"`/`"isolated"`, case-insensitive;
/// absent/unrecognized ⇒ Cross, fail-safe = pre-field behavior) and the isolated-wallet carrier
/// (`isolatedWallet`, falling back to `isolatedMargin` — positionRisk carries both; the wallet is
/// the allocated balance, `isolatedMargin` ≈ wallet + uPnL) are parsed into the report's inert
/// carrier fields. Nothing on the REQUEST side changes.
pub fn parse_perp_position_risk(
    body: &str,
    venue: &str,
) -> Result<Vec<PositionStatusReport>, String> {
    parse_rows(body, |p| {
        let symbol = p.get("symbol").and_then(|s| s.as_str()).unwrap_or("").to_string();
        let position_side =
            PositionSide::from(p.get("positionSide").and_then(|s| s.as_str()).unwrap_or("BOTH"));
        let qty = p.get("positionAmt").and_then(json_num).unwrap_or(0.0); // ALREADY SIGNED
        let avg_px = p.get("entryPrice").and_then(json_num).unwrap_or(0.0);
        let ts = p.get("updateTime").and_then(|t| t.as_i64()).unwrap_or(0);
        let (margin_mode, isolated_margin) = parse_margin_type(p);
        PositionStatusReport {
            venue: venue.to_string(),
            symbol,
            position_side,
            qty,
            avg_px,
            ts,
            margin_mode,
            isolated_margin,
            delta: None,
        }
    })
}

/// The Binance-grammar margin-mode read shared by the report parser above and the two venues'
/// `reconcile_positions` snapshot builders: `marginType` `"isolated"` (case-insensitive) ⇒
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
            .and_then(json_num)
            .or_else(|| p.get("isolatedMargin").and_then(json_num));
        (vike_model::MarginMode::Isolated, wallet)
    } else {
        (vike_model::MarginMode::Cross, None)
    }
}

/// `GET /fapi/v{2,3}/balance` rows -> the perp USDT wallet `balance` (realized cash — cross wallet
/// balance including realized PnL, EXCLUDING unrealized PnL from open positions; deliberately NOT
/// `availableBalance`, which further excludes margin currently locked by open positions/orders).
/// This is the SAME field `perp::BinancePerpRest::fetch_usdt_balance` (and its Aster twin) extract
/// for the legacy `ReconcileSnapshot.balance`. `None` when no USDT row is present (an account with
/// zero USDT never appears in this array); malformed JSON is a hard `Err`, never a panic (`json_num`
/// never unwraps). The endpoint (Binance `/fapi/v2/balance` vs Aster `/fapi/v3/balance`) lives in the
/// caller's [`ReconPaths`], not this body-only parser — the row shape is byte-identical.
pub fn parse_perp_balance(body: &str) -> Result<Option<f64>, String> {
    let v: serde_json::Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    let rows = v.as_array().ok_or_else(|| "expected a JSON array".to_string())?;
    Ok(rows
        .iter()
        .find(|r| r.get("asset").and_then(|a| a.as_str()) == Some("USDT"))
        .and_then(|r| r.get("balance"))
        .and_then(json_num))
}

/// `GET /api/v3/account` `balances[]` rows -> the spot USDT FREE balance (`free`, NOT `locked` —
/// spot has no margin, so the account's authoritative available cash is the free portion; qty
/// tied up in a resting sell order is excluded). This is a DELIBERATELY DIFFERENT choice from the
/// perp wallet `balance` above (spot vs. perp "balance" are not the same concept). `None` when no
/// USDT row is present; malformed JSON (or a missing `balances` array) is a hard `Err`, never a
/// panic.
pub fn parse_spot_balance(body: &str) -> Result<Option<f64>, String> {
    let v: serde_json::Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    let balances = v
        .get("balances")
        .and_then(|b| b.as_array())
        .ok_or_else(|| "expected a `balances` array".to_string())?;
    Ok(balances
        .iter()
        .find(|b| b.get("asset").and_then(|a| a.as_str()) == Some("USDT"))
        .and_then(|b| b.get("free"))
        .and_then(json_num))
}

/// `/api/v3/account` body -> live SPOT [`FeeSchedule`] from `commissionRates{maker,taker}`
/// (fractions). Delegates to the pure [`crate::spot::parse_commission_rates`] (the field extractor
/// that surfaces what `connect()`'s account body already carries). `Ok(None)` when the object is
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
/// (`GET /api/v3/commissionRate`). Only the PATH differs — see [`ReconPaths::perp_commission_rate`]
/// and [`ReconPaths::spot_commission_rate`]. [`parse_perp_fee_rates`] is the historical perp-lane
/// name, kept because both venues re-export it 1-arg for their golden-body fixture tests.
pub fn parse_commission_rate_body(body: &str) -> Result<Option<FeeSchedule>, String> {
    let v: serde_json::Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    let maker = v.get("makerCommissionRate").and_then(json_num);
    let taker = v.get("takerCommissionRate").and_then(json_num);
    Ok(match (maker, taker) {
        (Some(m), Some(t)) => Some(FeeSchedule::from_binance_rates(m, t)),
        _ => None,
    })
}

/// The PERP-lane name for [`parse_commission_rate_body`] — the `GET /fapi/v{1,3}/commissionRate`
/// body. Delegates verbatim; kept as its own name because both venues re-export it 1-arg and their
/// `recon_client_parse` test fixtures are written against it.
pub fn parse_perp_fee_rates(body: &str) -> Result<Option<FeeSchedule>, String> {
    parse_commission_rate_body(body)
}

// --- venue spec (the ONE real per-venue delta: endpoint paths + label) -------------------------

/// The reconcile endpoint table for one venue. The spot paths (`/api/v3/*`) happen to be identical
/// across the two venues; the perp paths are the real divergence (Binance's `/fapi/v1|v2` split vs
/// Aster's single `/fapi/v3`), which is exactly why they are passed as values rather than baked in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReconPaths {
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
    /// [`ReconClient::fetch_balance`]; used for the spot FEE lane only when
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
    /// Perp live open orders (signed).
    pub perp_open_orders: &'static str,
    /// Perp recent fills (signed, `signed_requery` history path).
    pub perp_user_trades: &'static str,
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

/// A venue's full reconcile spec: its endpoint [`ReconPaths`] plus the `venue` string stamped on
/// every report row. `Copy` + all-`&'static str`, so a reconcile thread carries it by value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReconSpec {
    /// The canonical venue key stamped on every report (`"binance"` / `"aster"`).
    pub venue: &'static str,
    /// The resolved endpoint paths (see [`ReconPaths`]).
    pub paths: ReconPaths,
}

// --- the shared client ------------------------------------------------------------------------

/// One reconcile client per venue-symbol, routed `Spot`/`Perp`, generic over the venue's
/// `Signer`/`RestTransport` seam and carrying only what a report fetch needs (signer + transport +
/// base_url + symbol + the venue [`ReconSpec`]; no order-formatting `SymbolProperties`). Each venue
/// wraps this behind its own named type (`BinanceReconClient`/`AsterReconClient`) pinned to its
/// spec.
pub enum FamilyReconClient<S: Signer, T: RestTransport> {
    Spot { spec: ReconSpec, signer: S, transport: T, base_url: String, symbol: String },
    Perp { spec: ReconSpec, signer: S, transport: T, base_url: String, symbol: String },
}

impl<S: Signer, T: RestTransport> FamilyReconClient<S, T> {
    pub fn spot(
        spec: ReconSpec,
        signer: S,
        transport: T,
        base_url: impl Into<String>,
        symbol: impl Into<String>,
    ) -> Self {
        FamilyReconClient::Spot {
            spec,
            signer,
            transport,
            base_url: base_url.into(),
            symbol: symbol.into(),
        }
    }

    pub fn perp(
        spec: ReconSpec,
        signer: S,
        transport: T,
        base_url: impl Into<String>,
        symbol: impl Into<String>,
    ) -> Self {
        FamilyReconClient::Perp {
            spec,
            signer,
            transport,
            base_url: base_url.into(),
            symbol: symbol.into(),
        }
    }

    fn spec(&self) -> &ReconSpec {
        match self {
            FamilyReconClient::Spot { spec, .. } | FamilyReconClient::Perp { spec, .. } => spec,
        }
    }

    fn venue(&self) -> &'static str {
        self.spec().venue
    }

    fn paths(&self) -> ReconPaths {
        self.spec().paths
    }

    fn symbol(&self) -> &str {
        match self {
            FamilyReconClient::Spot { symbol, .. } | FamilyReconClient::Perp { symbol, .. } => {
                symbol
            }
        }
    }

    fn parts(&self) -> (&S, &T, &str) {
        match self {
            FamilyReconClient::Spot { signer, transport, base_url, .. }
            | FamilyReconClient::Perp { signer, transport, base_url, .. } => {
                (signer, transport, base_url)
            }
        }
    }

    /// The normal signed GET path — the same one `spot::connect`/`perp::reconcile_positions`
    /// already use for account/position reads (rides the venue's order-rate gate, if any).
    fn signed(&self, path: &str, params: &[(&str, String)]) -> Result<String, String> {
        let (signer, transport, base_url) = self.parts();
        transport
            .signed(base_url, path, "GET", params, signer)
            .map(|v| v.to_string())
            .map_err(|e| e.to_string())
    }

    /// The short-timeout, rate-gate-EXEMPT path — the SAME one the crate's audit-A3 resync
    /// (`get_my_trades`/`get_user_trades`) already uses for these exact two heavier history
    /// endpoints, so a resync burst can't blow the per-IP `ORDERS` budget submit/cancel depend on.
    fn signed_requery(&self, path: &str, params: &[(&str, String)]) -> Result<String, String> {
        let (signer, transport, base_url) = self.parts();
        transport
            .signed_requery(base_url, path, "GET", params, signer)
            .map(|v| v.to_string())
            .map_err(|e| e.to_string())
    }
}

impl<S: Signer, T: RestTransport + Send> ReconClient for FamilyReconClient<S, T> {
    /// `since` is a no-op here: `openOrders` has no time filter and reports only the CURRENTLY open
    /// set — there's nothing to filter it by, and a closed order that fell out of this set is
    /// exactly what the fill/position reports (not this one) reconcile.
    fn fetch_order_status_reports(&self, _since: i64) -> Result<Vec<OrderStatusReport>, String> {
        let symbol = self.symbol().to_string();
        let venue = self.venue();
        let paths = self.paths();
        match self {
            FamilyReconClient::Spot { .. } => {
                let body = self.signed(paths.spot_open_orders, &[("symbol", symbol)])?;
                parse_spot_open_orders(&body, venue)
            }
            FamilyReconClient::Perp { .. } => {
                let body = self.signed(paths.perp_open_orders, &[("symbol", symbol)])?;
                parse_perp_open_orders(&body, venue)
            }
        }
    }

    fn fetch_fill_reports(&self, since: i64) -> Result<Vec<FillReport>, String> {
        let mut params: Vec<(&str, String)> = vec![("symbol", self.symbol().to_string())];
        if since > 0 {
            params.push(("startTime", since.to_string()));
        }
        let venue = self.venue();
        let paths = self.paths();
        match self {
            FamilyReconClient::Spot { .. } => {
                let body = self.signed_requery(paths.spot_my_trades, &params)?;
                parse_spot_my_trades(&body, venue)
            }
            FamilyReconClient::Perp { .. } => {
                let body = self.signed_requery(paths.perp_user_trades, &params)?;
                parse_perp_user_trades(&body, venue)
            }
        }
    }

    /// Spot has no venue-native position report — an empty vec is the correct, always-safe answer
    /// (never a false divergence).
    fn fetch_position_status_reports(&self) -> Result<Vec<PositionStatusReport>, String> {
        match self {
            FamilyReconClient::Spot { .. } => Ok(Vec::new()),
            FamilyReconClient::Perp { .. } => {
                let symbol = self.symbol().to_string();
                let venue = self.venue();
                let body = self.signed(self.paths().perp_positions, &[("symbol", symbol)])?;
                parse_perp_position_risk(&body, venue)
            }
        }
    }

    /// Balance meaning: PERP = the USDT wallet `balance` from the perp balance endpoint (same field
    /// `perp::fetch_usdt_balance` uses); SPOT = the USDT FREE balance from `/api/v3/account`
    /// (excludes qty locked in resting sell orders). Both endpoints are light, unfiltered account
    /// reads — ride the normal `signed` (rate-gated) path, never `signed_requery`.
    fn fetch_balance(&self) -> Result<Option<f64>, String> {
        let paths = self.paths();
        match self {
            FamilyReconClient::Spot { .. } => {
                let body = self.signed(paths.spot_account, &[])?;
                parse_spot_balance(&body)
            }
            FamilyReconClient::Perp { .. } => {
                let body = self.signed(paths.perp_balance, &[])?;
                parse_perp_balance(&body)
            }
        }
    }

    /// Live account-actual fee rates (fee model 4/5). Each lane picks ONE of two shapes from the
    /// venue's [`ReconPaths`], and both ride the normal `signed` (rate-gated) path:
    ///
    ///   - **SPOT.** [`ReconPaths::spot_commission_rate`] `Some(path)` ⇒ a per-symbol
    ///     `commissionRate` read through [`parse_commission_rate_body`] (Aster: its account body
    ///     carries no rates). `None` ⇒ `commissionRates` off `spot_account` via
    ///     [`parse_spot_fee_rates`] — the SAME body `connect()`/`fetch_balance` fetch, so it costs
    ///     no separate endpoint (Binance). The `None` arm is verbatim the pre-slot behavior.
    ///   - **PERP.** [`ReconPaths::perp_commission_rate`] `Some(path)` ⇒ the same per-symbol read;
    ///     `None` ⇒ the lane is INERT (`Ok(None)`, static default) and issues NO request at all,
    ///     which is how a venue whose commissionRate endpoint is unverified stays un-probed.
    ///
    /// Spot has no inert arm because a venue always has one of the two spot shapes; a venue with
    /// neither would set `spot_commission_rate: None` and its `spot_account` body simply carries no
    /// `commissionRates`, which `parse_spot_fee_rates` already answers `Ok(None)` for (fail-soft).
    fn fetch_fee_rates(&self) -> Result<Option<FeeSchedule>, String> {
        let paths = self.paths();
        match self {
            FamilyReconClient::Spot { .. } => match paths.spot_commission_rate {
                Some(path) => {
                    let body = self.signed(path, &[("symbol", self.symbol().to_string())])?;
                    parse_commission_rate_body(&body)
                }
                None => {
                    let body = self.signed(paths.spot_account, &[])?;
                    parse_spot_fee_rates(&body)
                }
            },
            FamilyReconClient::Perp { .. } => match paths.perp_commission_rate {
                Some(path) => {
                    let body = self.signed(path, &[("symbol", self.symbol().to_string())])?;
                    parse_commission_rate_body(&body)
                }
                None => Ok(None),
            },
        }
    }
}
