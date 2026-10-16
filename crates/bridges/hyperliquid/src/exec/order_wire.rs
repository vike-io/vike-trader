//! Order-wire helpers: the TIF map, the trigger kind, the reply's new oid, the `/exchange` seam.

use serde_json::Value;

use vike_bridge_core::transport::VenueApiError;
use vike_model::{OrderRequest, OrderType, TimeInForce};

use crate::base::px;
use crate::signing::action::{Action, OrderKind, TriggerParams};

#[cfg(doc)]
use super::market_slippage_for;

/// Map a vike [`TimeInForce`] to HL's case-sensitive wire TIF — this venue's row of the ONE
/// cross-venue TIF authority ([`vike_venues::venue_tif::venue_tif`]), consumed. HL exposes only
/// `Gtc`/`Ioc`/`Alo` (the declared caps are `Gtc`+`Ioc`); `Alo` is post-only and is not expressible
/// via [`OrderRequest`] today, so `Fok` folds to the aggressive `Ioc` — the OPPOSITE direction of
/// polymarket's `Ioc`→`FOK` fold (same pair!); step 2 resolves that deliberately, behind demo
/// smokes — and `Gtc`/`Gtd`/`Day` fold to the resting `Gtc`.
pub(super) fn hl_tif(tif: TimeInForce) -> &'static str {
    // `wire()` is Some for every hyperliquid row (Mapped/Coerced only) — the fallback is
    // unreachable, kept so the exec thread can never panic.
    vike_venues::venue_tif::venue_tif(crate::base::consts::VENUE, tif).wire().unwrap_or("Gtc")
}

/// Build a stop/trigger order's `(limit_px, OrderKind::Trigger)` (research §6: the wire is
/// `t: { trigger: { isMarket, triggerPx, tpsl } }`; the HL docs confirm `tpsl="sl"` = stop-loss).
///
/// - `trigger_px` = [`px::clamp_price`] of the request's `trigger_price` (the activation level).
/// - `is_market = req.price.is_none()`: a stop-MARKET fires aggressively on trip — its protective
///   `limit_px` is the trigger price shifted ±`slippage` (the SAME band the emulated market uses, so
///   a triggered stop actually fills; the current mid is far from the trigger at placement time, so
///   the trigger — not the mid — is the reference). A stop-LIMIT (`price` present) rests at
///   `clamp_price(req.price)` once triggered and never chases.
///
/// `slippage` is the mount-resolved band ([`market_slippage_for`]), passed in rather than read from
/// a global precisely because it is the number that decides how much worse than the trigger a
/// protective exit may fill.
/// - `tpsl` is `"tp"` (take-profit — fires on a FAVORABLE move) when `req.order_type` is
///   [`OrderType::TakeProfit`], else `"sl"` (stop-loss — order_type `"stop"`, or any bare
///   `trigger_price`). The market/limit axis is orthogonal: a take-profit is stop-MARKET when
///   `price` is absent and stop-LIMIT when present, exactly like a stop-loss.
///
/// `Err` for a trigger without a `trigger_price`, or a trigger price that rounds below the grid.
pub(super) fn build_trigger_kind(
    req: &OrderRequest,
    sz_decimals: u32,
    is_spot: bool,
    is_buy: bool,
    slippage: f64,
) -> Result<(String, OrderKind), String> {
    let trigger = req
        .trigger_price
        .ok_or_else(|| format!("stop order for {} without a trigger_price", req.symbol))?;
    let trigger_px = px::clamp_price(trigger, sz_decimals, is_spot);
    if trigger_px == "0" {
        return Err("trigger price rounds to zero".to_string());
    }
    let is_market = req.price.is_none();
    let limit_px = match req.price {
        // stop-LIMIT: rest at the requested limit once triggered.
        Some(price) => px::clamp_price(price, sz_decimals, is_spot),
        // stop-MARKET: no limit given — bound the market fill with an aggressive price off the
        // trigger (buy stop: trigger*(1+slip); sell stop: trigger*(1-slip)), mirroring the emulated
        // market order so the fill isn't stranded when the stop trips.
        None => {
            let slip = if is_buy { 1.0 + slippage } else { 1.0 - slippage };
            px::clamp_price(trigger * slip, sz_decimals, is_spot)
        }
    };
    // Take-profit vs stop-loss: an explicit `"take_profit"` order_type fires on a favorable move
    // (`"tp"`); everything else routed here (order_type `"stop"`, or a bare `trigger_price`) is a
    // stop-loss (`"sl"`). HL uses `tpsl` + side to decide the trigger comparison direction.
    let tpsl = if req.order_type == OrderType::TakeProfit { "tp" } else { "sl" };
    let kind = OrderKind::Trigger(TriggerParams { is_market, trigger_px, tpsl: tpsl.to_string() });
    Ok((limit_px, kind))
}

/// Pull the first `statuses[i].resting.oid` out of an `/exchange` response — the NEW oid a
/// cancel-replace `modify` produces (research §6). `None` when the response has no resting oid
/// (filled/error/malformed).
pub(super) fn parse_first_resting_oid(resp: &Value) -> Option<u64> {
    resp.get("response")?
        .get("data")?
        .get("statuses")?
        .as_array()?
        .first()?
        .get("resting")?
        .get("oid")?
        .as_u64()
}

/// What the venue answered to a native `modify` (cancel-replace) action.
#[derive(Debug, PartialEq)]
pub(super) enum ModifyReply {
    /// The replace landed. `new_oid` is the (possibly new) venue oid, `None` when the ok-envelope
    /// carried none (the resting order is then still keyed by the oid we sent).
    Applied { new_oid: Option<u64> },
    /// The venue refused: a top-level `{status:"err",response:"<str>"}` or a `statuses[0].error`.
    /// `/exchange` answers HTTP 200 for BOTH — the transport cannot tell them apart.
    Refused(String),
}

/// Classify a `modify` reply. `place` returns `Ok` for any HTTP 2xx and the body is not inspected
/// there, so a refusal arrives as an `Ok(resp)` that must be read here (the submit/cancel twins are
/// `map_order_response`/`map_cancel_response`). A `filled` status (the replacement crossed the book)
/// is an applied replace whose new oid rides on the `filled` object; the fill itself arrives through
/// the WS `userFills`/`orderUpdates` lanes. An ok-envelope with no recognizable status is treated as
/// applied (the venue said `ok`), exactly as before this classifier existed.
pub(super) fn parse_modify_reply(resp: &Value) -> ModifyReply {
    if resp.get("status").and_then(|s| s.as_str()) == Some("err") {
        let reason =
            resp.get("response").and_then(|r| r.as_str()).unwrap_or("modify request failed");
        return ModifyReply::Refused(reason.to_string());
    }
    let first = resp
        .get("response")
        .and_then(|r| r.get("data"))
        .and_then(|d| d.get("statuses"))
        .and_then(|s| s.as_array())
        .and_then(|s| s.first());
    if let Some(err) = first.and_then(|st| st.get("error")) {
        return ModifyReply::Refused(err.as_str().unwrap_or("modify rejected").to_string());
    }
    let new_oid = parse_first_resting_oid(resp).or_else(|| {
        first.and_then(|st| st.get("filled")).and_then(|f| f.get("oid")).and_then(|o| o.as_u64())
    });
    ModifyReply::Applied { new_oid }
}

/// Seam over the signed `/exchange` POST + the `allMids` read, so the command handling below is
/// unit-testable with a fake (no network, no real orders) — the twin of the Bybit/OKX `VenueRest`
/// mock seam. `place` owns nonce assignment + signing; `mid` backs the emulated-market price.
pub(super) trait HlExchange {
    /// Sign + POST one `/exchange` action (assigning the next monotonic nonce). Returns the raw
    /// `{status,response}` body, or a transport error the caller classifies (`must_requery` vs dead).
    fn place(&self, action: &Action) -> Result<Value, VenueApiError>;
    /// Current mid for a venue `coin` (from `allMids`) — the reference for the emulated-market px.
    fn mid(&self, coin: &str) -> Option<f64>;
}
