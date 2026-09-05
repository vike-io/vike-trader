//! OKX `ReconClient` (Task 11 of the reconciliation-engine feature) — the venue-facing report
//! seam (`vike_exec::recon::ReconClient`) for the V5 SWAP perp, reconciling against:
//!
//!   `GET /api/v5/trade/orders-pending` (order reports — CURRENTLY open only, no time filter;
//!   mirrors `perp::OkxPerpRest`'s own `PATH_POSITIONS`/history endpoints).
//!   `GET /api/v5/trade/fills` (fill reports; `begin` is the Unix-ms lower bound when `since > 0`).
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
//! `OkxReconClient` reuses the SAME `OkxV5Signer`/`OkxTransport` seam as `perp::OkxPerpRest` (no
//! new HTTP/signing stack): `orders-pending`/`positions` ride the normal `signed` path (the same
//! one `perp::OkxPerpRest::call`/`reconcile_positions` already use for order/position reads),
//! while `fills` — a history-shaped scan, same class as Binance's `myTrades`/`userTrades` — rides
//! `signed_requery`, the SAME short-timeout, rate-gate-EXEMPT path `perp::OkxPerpRest::
//! get_orders_history`/`get_fills_history` already use for the crate's audit-A3 resync, so a
//! reconcile pass can't blow the per-IP order-rate budget submit/cancel depend on.

use serde_json::{Value, json};

use vike_bridge_core::json::{json_int, json_num, parse_rows};
use vike_bridge_core::signer::OkxV5Signer;
use vike_exec::recon::ReconClient;
use vike_model::events::{LiquiditySide, PositionSide, TradeId};
use vike_model::{FeeSchedule, FillReport, MarginMode, OrderStatusReport, PositionStatusReport};

use crate::perp;
use crate::transport::{OkxTransport, unwrap_okx};

const VENUE: &str = "okx";

pub const PATH_ORDERS_PENDING: &str = "/api/v5/trade/orders-pending";
pub const PATH_FILLS: &str = "/api/v5/trade/fills";
pub const PATH_TRADE_FEE: &str = "/api/v5/account/trade-fee";

// --- pure parsers --------------------------------------------------------------------------

fn side_sign(s: &str) -> i32 {
    if s.eq_ignore_ascii_case("buy") { 1 } else { -1 }
}

fn liquidity_side(exec_type: &str) -> LiquiditySide {
    if exec_type == "M" { LiquiditySide::Maker } else { LiquiditySide::Taker }
}

/// OKX's hedge-mode `posSide` is lowercase (`"long"`/`"short"`/`"net"`) — NOT the uppercase
/// `PositionSide::from` vocabulary (`"LONG"`/`"SHORT"`), so a naive `PositionSide::from(raw)`
/// would silently fall through every hedge leg to `Both`. Mirrors
/// `perp::OkxPerpRest::reconcile_positions`'s own `match posSide { "long" => "LONG", ... }`.
fn position_side(raw: &str) -> PositionSide {
    match raw {
        "long" => PositionSide::Long,
        "short" => PositionSide::Short,
        _ => PositionSide::Both, // "net" (one-way mode) and any unknown value
    }
}

/// Normalize a raw OKX order `state` to the `OrderStatus::parse` FSM vocabulary that
/// `recon::diff::diff` reads. OKX's `live` has no direct twin other than `ACCEPTED`; every other
/// state (`partially_filled`, `filled`, `canceled`, `mmp_canceled`) already matches our spelling
/// once uppercased (`PARTIALLY_FILLED`, `FILLED`, `CANCELED`, `MMP_CANCELED`).
fn normalize_order_status(raw: &str) -> String {
    match raw {
        "live" => "ACCEPTED".to_string(),
        other => other.to_ascii_uppercase(),
    }
}

fn client_order_id(v: &Value) -> Option<String> {
    v.get("clOrdId").and_then(|c| c.as_str()).filter(|c| !c.is_empty()).map(|c| c.to_string())
}

/// `GET /api/v5/trade/orders-pending` rows -> `OrderStatusReport`. `sz`/`accFillSz` are CONTRACTS
/// — rescaled to base via `ct_val` (see module doc). `avgPx` is already quote-per-base.
pub fn parse_orders_pending(body: &str, ct_val: f64) -> Result<Vec<OrderStatusReport>, String> {
    parse_rows(body, |o| {
        let symbol = o.get("instId").and_then(|s| s.as_str()).unwrap_or("").to_string();
        let venue_order_id = o.get("ordId").and_then(|s| s.as_str()).unwrap_or("").to_string();
        let side = side_sign(o.get("side").and_then(|s| s.as_str()).unwrap_or(""));
        let order_type =
            o.get("ordType").and_then(|t| t.as_str()).unwrap_or("").to_ascii_lowercase();
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

/// `GET /api/v5/trade/fills` rows -> `FillReport`. `fillSz` is CONTRACTS — rescaled to base via
/// `ct_val`. Commission: OKX `fee` is NEGATIVE for a charge, so `commission = -fee` (positive
/// cost / negative rebate) — same sign convention `event_mapper::map_okx_order` already applies
/// to the live WS fill stream.
///
/// A row with no `tradeId` is DROPPED, not reported. A `FillReport` with no `trade_id` is worse than
/// a missing one: `vike_exec::recon::diff` decides "already known" by looking the id up in
/// `seen_trade_ids`, so an id-less row can never match and instead FABRICATES a `MissingFill`
/// divergence — one of the two kinds the `hybrid` policy AUTO-APPLIES
/// (`vike_ops::reconcile_config::auto_applied_kinds`), i.e. it would book the fill a second time with
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
        let side = position_side(p.get("posSide").and_then(|s| s.as_str()).unwrap_or("net"));
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
/// `vike_mount::build_recon_client`'s `"okx"` match arm: a FRESH stateless-HMAC `ureq` REST client
/// dedicated to reconcile reads, reusing the SAME `Credentials` the venue's `ExecutionClient`/
/// `spawn_with_recorder` already builds from. `ct_val` is the caller's already-fetched (or
/// fallback) contracts->base scale factor, threaded in rather than re-fetched here — see
/// `vike_mount::make_engine`'s okx arm, which fetches it once and reuses it for both the exec grid
/// and this recon client. Uses the shared host (`perp::REST` — demo and mainnet share it), with the
/// `x-simulated-trading` header selected by `mainnet` — the SAME already-resolved `OKX_MAINNET`
/// verdict the mount threaded into the exec path (`vike_mount::make_engine` resolves it ONCE via
/// [`crate::perp::mainnet_enabled`]), so reconcile stays in lockstep with exec instead of re-reading
/// global env for itself. Always `Some` — construction here is pure/infallible (no network);
/// `Option` is kept so the return type matches every other bridge's `recon_client` (deribit's
/// genuinely can fail).
pub fn recon_client(
    creds: &vike_bridge_core::Credentials,
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
/// no order-formatting `SymbolProperties`/leverage).
pub struct OkxReconClient<T: OkxTransport> {
    signer: OkxV5Signer,
    transport: T,
    base_url: String,
    /// instId form: BTC-USDT-SWAP
    symbol: String,
    /// base units per contract — the contracts→base rescale factor (see module doc).
    ct_val: f64,
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
        }
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
}

impl<T: OkxTransport + Send> ReconClient for OkxReconClient<T> {
    /// `since` is a no-op here: `orders-pending` has no time filter and reports only the
    /// CURRENTLY open set — a closed order that fell out of this set is exactly what the fill/
    /// position reports (not this one) reconcile (mirrors Binance's `openOrders`).
    fn fetch_order_status_reports(&self, _since: i64) -> Result<Vec<OrderStatusReport>, String> {
        let body = self.signed(PATH_ORDERS_PENDING, &[("instId", json!(self.symbol))])?;
        parse_orders_pending(&body, self.ct_val)
    }

    fn fetch_fill_reports(&self, since: i64) -> Result<Vec<FillReport>, String> {
        let mut params: Vec<(&str, Value)> = vec![("instId", json!(self.symbol))];
        if since > 0 {
            params.push(("begin", json!(since.to_string())));
        }
        let body = self.signed_requery(PATH_FILLS, &params)?;
        parse_fills(&body, self.ct_val)
    }

    fn fetch_position_status_reports(&self) -> Result<Vec<PositionStatusReport>, String> {
        let body = self.signed(
            perp::PATH_POSITIONS,
            &[("instType", json!("SWAP")), ("instId", json!(self.symbol))],
        )?;
        parse_positions(&body, &self.symbol, self.ct_val)
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
    /// (`x-simulated-trading: 1`) while bringing up the first real tradehub CEX mount:
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
}
