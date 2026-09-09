//! Pure Alpaca ↔ vike mapping (no I/O; the second oracle for the exec path). Order-body
//! construction, POST-response → events, and `/v2/events/trades` SSE object → events. `format_to_step`
//! is the pinned Decimal wire site.

use vike_bridge_core::format::format_to_step;
use vike_model::events::{
    Event, FillEvent, OrderAccepted, OrderCanceled, OrderFilled, OrderRejected, TradeId,
};
use vike_model::{OrderRequest, TimeInForce};

use crate::data::parse_rfc3339_ms;

const VENUE: &str = "alpaca";

/// vike symbol → Alpaca symbol. Equities pass through (`AAPL`). Crypto pairs get a slash
/// (`BTCUSD` → `BTC/USD`): a bare all-alpha symbol ending in a known quote asset.
/// Bare-symbol auto-slashing only covers USD/USDT/USDC quotes; BTC-quoted crypto pairs
/// (e.g. `ETH/BTC`) must be passed already-slashed. This is deliberate: several real
/// US-listed equity tickers end in `BTC` (`GBTC` Grayscale Bitcoin Trust, `FBTC` Fidelity
/// Wise Origin Bitcoin Fund), so a bare-suffix `BTC` match would mangle them into a
/// nonexistent crypto pair — those tickers are left untouched.
pub fn to_alpaca_symbol(sym: &str) -> String {
    const QUOTES: [&str; 3] = ["USD", "USDT", "USDC"];
    if sym.contains('/') {
        return sym.to_string();
    }
    for q in QUOTES {
        if sym.len() > q.len() && sym.ends_with(q) && sym.chars().all(|c| c.is_ascii_alphabetic()) {
            let base = &sym[..sym.len() - q.len()];
            return format!("{base}/{q}");
        }
    }
    sym.to_string()
}

/// Trim trailing fractional zeros (and a bare trailing `.`) from a `format_to_step` output.
/// `format_to_step` always pads to the step's full scale (e.g. `"10.000000000"` for a
/// 9-decimal step), but Alpaca (and the test fixture) expect whole-share quantities as bare
/// integers (`"10"`) while still preserving fractional-share/crypto precision (`"0.01"`).
fn trim_trailing_zeros(s: String) -> String {
    if !s.contains('.') {
        return s;
    }
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// This venue's row of the ONE cross-venue TIF authority ([`vike_bridge_core::tif::venue_tif`]),
/// consumed — lowercase wire; Alpaca has no GTD, so the row coerces `Gtd -> "gtc"`. A future TIF
/// flip is a one-line row edit there.
fn tif_str(tif: TimeInForce) -> &'static str {
    vike_bridge_core::tif::venue_tif(VENUE, tif).wire().unwrap_or("gtc")
}

/// Build the `/v1/trading/accounts/{id}/orders` POST body from a vike `OrderRequest`.
pub fn build_order_body(req: &OrderRequest) -> serde_json::Value {
    let side = if req.side >= 0 { "buy" } else { "sell" };
    let mut body = serde_json::json!({
        "symbol": to_alpaca_symbol(&req.symbol),
        "qty": trim_trailing_zeros(format_to_step(req.qty.abs(), "0.000000001")),
        "side": side,
        "time_in_force": tif_str(req.time_in_force),
        "client_order_id": req.client_order_id,
    });
    match req.order_type.as_str() {
        "limit" => {
            body["type"] = serde_json::json!("limit");
            if let Some(p) = req.price {
                body["limit_price"] = serde_json::json!(format_to_step(p, "0.01"));
            }
        }
        "stop" => {
            body["type"] = serde_json::json!("stop");
            if let Some(p) = req.trigger_price {
                body["stop_price"] = serde_json::json!(format_to_step(p, "0.01"));
            }
        }
        "stop_limit" => {
            body["type"] = serde_json::json!("stop_limit");
            if let Some(p) = req.price {
                body["limit_price"] = serde_json::json!(format_to_step(p, "0.01"));
            }
            if let Some(p) = req.trigger_price {
                body["stop_price"] = serde_json::json!(format_to_step(p, "0.01"));
            }
        }
        _ => {
            body["type"] = serde_json::json!("market");
        }
    }
    body
}

/// Map an order-POST response to the events it implies. Alpaca returns the created order object on
/// success (`id`, `status`), or an error object (`code`/`message`) on failure.
pub fn map_order_response(coid: &str, ts: i64, resp: &serde_json::Value) -> Vec<Event> {
    if resp.get("id").is_none() {
        let reason = resp.get("message").and_then(|m| m.as_str()).unwrap_or("rejected");
        return vec![Event::OrderRejected(OrderRejected {
            client_order_id: coid.to_string(),
            reason: reason.to_string().into(),
            ts,
        })];
    }
    vec![Event::OrderAccepted(OrderAccepted {
        client_order_id: coid.to_string(),
        venue_order_id: resp.get("id").and_then(|i| i.as_str()).map(Into::into),
        ts,
    })]
}

fn parse_ts_ms(v: &serde_json::Value, key: &str) -> i64 {
    // Alpaca SSE carries a real RFC3339 timestamp (e.g. "2026-07-14T05:39:31.4Z") — try that first
    // via the shared parser (crate::data::parse_rfc3339_ms, also used by the market-data WS decode).
    // Fall back to a bare numeric-epoch-ms string (defensive), else 0 (the core stamps its own
    // receive time).
    let Some(s) = v.get(key).and_then(|t| t.as_str()) else { return 0 };
    parse_rfc3339_ms(s).or_else(|| s.parse::<i64>().ok()).unwrap_or(0)
}

/// Decode one `/v2/events/trades` SSE JSON object into vike events.
pub fn decode_trade_event(v: &serde_json::Value) -> Vec<Event> {
    let event = v.get("event").and_then(|e| e.as_str()).unwrap_or("");
    let order = v.get("order").cloned().unwrap_or(serde_json::Value::Null);
    let coid =
        order.get("client_order_id").and_then(|c| c.as_str()).unwrap_or_default().to_string();
    let ts = parse_ts_ms(v, "timestamp");
    match event {
        "fill" | "partial_fill" => {
            let s = |k: &str| {
                v.get(k).and_then(|x| x.as_str()).or_else(|| order.get(k).and_then(|x| x.as_str()))
            };
            let side =
                if order.get("side").and_then(|x| x.as_str()) == Some("sell") { -1 } else { 1 };
            let last_qty: f64 = s("qty").and_then(|x| x.parse().ok()).unwrap_or(0.0);
            let last_px: f64 = s("price")
                .and_then(|x| x.parse().ok())
                .or_else(|| {
                    order
                        .get("filled_avg_price")
                        .and_then(|x| x.as_str())
                        .and_then(|x| x.parse().ok())
                })
                .unwrap_or(0.0);
            // `execution_id` is Alpaca's per-fill identity on the trade-update stream and the key
            // the engine dedups reconnect replays on. Absent ⇒ refuse the frame: there is no other
            // per-execution field here (`order.id` is shared by every fill of a partially-filled
            // order), so nothing replay-stable to synthesize from.
            let trade_id = match TradeId::new(
                v.get("execution_id").and_then(|x| x.as_str()).unwrap_or(""),
            ) {
                Ok(t) => t,
                Err(_) => {
                    tracing::warn!(
                        venue = VENUE,
                        %coid,
                        "trade update carries no `execution_id` — dropping the fill rather than \
                         folding one that cannot be deduplicated"
                    );
                    return Vec::new();
                }
            };
            let fill = FillEvent {
                trade_id,
                client_order_id: coid.clone(),
                venue: VENUE.to_string().into(),
                symbol: order
                    .get("symbol")
                    .and_then(|x| x.as_str())
                    .unwrap_or_default()
                    .to_string()
                    .into(),
                side,
                last_qty,
                last_px,
                commission: 0.0,
                commission_asset: String::new().into(),
                // Alpaca's fill event carries no maker/taker liquidity flag (confirmed live
                // 2026-07-14 against a real equity fill: the payload has execution_id/price/qty/
                // timestamp but no liquidity field) — left empty, like the data-tick side flag.
                liquidity_side: String::new().into(),
                ts,
                mark_price: None,
                position_side: "BOTH".to_string().into(),
            };
            // Dual-publish (crypto contract): bare Fill (Account folds pnl) then OrderFilled (FSM).
            vec![
                Event::Fill(fill.clone()),
                Event::OrderFilled(OrderFilled { client_order_id: coid, fill, ts }),
            ]
        }
        "canceled" | "expired" => vec![Event::OrderCanceled(OrderCanceled {
            client_order_id: coid,
            reason: event.to_string().into(),
            ts,
        })],
        "rejected" => vec![Event::OrderRejected(OrderRejected {
            client_order_id: coid,
            reason: "rejected".to_string().into(),
            ts,
        })],
        "new" | "accepted" => vec![Event::OrderAccepted(OrderAccepted {
            client_order_id: coid,
            venue_order_id: order.get("id").and_then(|i| i.as_str()).map(Into::into),
            ts,
        })],
        _ => vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_alpaca_symbol_equity_passthrough() {
        assert_eq!(to_alpaca_symbol("AAPL"), "AAPL");
    }

    #[test]
    fn to_alpaca_symbol_crypto_gets_slashed() {
        assert_eq!(to_alpaca_symbol("BTCUSD"), "BTC/USD");
        assert_eq!(to_alpaca_symbol("ETHUSDT"), "ETH/USDT");
    }

    #[test]
    fn to_alpaca_symbol_already_slashed_passthrough() {
        assert_eq!(to_alpaca_symbol("BTC/USD"), "BTC/USD");
    }

    /// Equivalence gate for the `venue_tif` routing: the five recorded lowercase wire strings
    /// (Gtd coerced to "gtc" — Alpaca has no GTD), asserted BOTH through `tif_str` and against
    /// this venue's row of the cross-venue table (byte-for-byte).
    #[test]
    fn tif_str_matches_the_venue_tif_row() {
        use vike_model::TimeInForce::{Day, Fok, Gtc, Gtd, Ioc};
        for (tif, want) in [(Gtc, "gtc"), (Ioc, "ioc"), (Fok, "fok"), (Gtd, "gtc"), (Day, "day")] {
            assert_eq!(tif_str(tif), want, "{tif:?}");
            assert_eq!(
                vike_bridge_core::tif::venue_tif(VENUE, tif).wire(),
                Some(want),
                "table row {tif:?}"
            );
        }
    }
}
