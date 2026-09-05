//! IG Lightstreamer trade-update decode — the delayed working-order fills the sync `/confirms`
//! call can't carry, plus the audit-A3 `/history/activity` replay.
//!
//! IG's Lightstreamer `TRADE:{accountId}` subscription carries three text fields per update —
//! `CONFIRMS` (deal confirmation, same JSON shape as `GET /confirms/{ref}`), `OPU` (open-position
//! update) and `WOU` (working-order update). This module is PURE: (1) parse one TLCP update line
//! into its field values, and (2) decode a `CONFIRMS` payload into canonical events, honoring the
//! dual-publish contract (bare `Event::Fill` for the Account fold + the `OrderFilled` wrap for the
//! FSM), so a streamed working-order fill folds identically to an inline market fill. The transport
//! (streaming create_session + control subscribe + reconnect) is the driver's job — see `stream`.

use vike_model::events::{Event, FillEvent, OrderCanceled, OrderFilled, OrderRejected, TradeId};

const VENUE: &str = "ig";

/// One parsed TLCP `U` (update) line: `U,<subId>,<item>|f1|f2|...`. Field values are already
/// un-escaped: TLCP `$` → empty string, `#` → "null"/absent (returned as `None`), a truly empty
/// segment → `None` ("unchanged", which for IG's DISTINCT trade items means "no value this
/// update"). Non-empty fields are returned as `Some(text)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LsUpdate {
    pub sub_id: u32,
    pub item: u32,
    /// Field values in schema order (`CONFIRMS OPU WOU`), `None` = empty/unchanged/null.
    pub fields: Vec<Option<String>>,
}

/// Parse one Lightstreamer TLCP text line. Returns `Some` only for a data update (`U,...`); control
/// lines (CONOK/PROBE/LOOP/SYNC/SUBOK/...) return `None` (the driver handles those separately).
pub fn parse_update_line(line: &str) -> Option<LsUpdate> {
    let rest = line.strip_prefix("U,")?;
    // `U,<subId>,<item>|f1|f2|...` — the head (subId,item) is comma-separated, then pipe fields.
    let (head, tail) = match rest.split_once('|') {
        Some((h, t)) => (h, t),
        None => (rest, ""),
    };
    let mut head_parts = head.split(',');
    let sub_id = head_parts.next()?.parse().ok()?;
    let item = head_parts.next()?.parse().ok()?;
    let fields = tail
        .split('|')
        .map(|f| match f {
            "" => None,                 // unchanged / no value this update
            "#" => None,                // TLCP null
            "$" => Some(String::new()), // TLCP empty string
            other => Some(unescape_ls(other)),
        })
        .collect();
    Some(LsUpdate { sub_id, item, fields })
}

/// Un-escape TLCP field text: `\` escapes are not used by IG's JSON payloads, but a leading `$`/`#`
/// inside a longer value is literal — only a WHOLE-segment `$`/`#` is the sentinel (handled above).
fn unescape_ls(s: &str) -> String {
    s.to_string()
}

/// The IG deal-confirm `status` values that report a position being CLOSED rather than opened.
/// `PARTIALLY_CLOSED` belongs here for the same reason `CLOSED` does — it too reports an execution
/// against a deal that already existed.
const CLOSE_STATUSES: &[&str] = &["CLOSED", "PARTIALLY_CLOSED"];

/// Whether a deal confirm (sync `/confirms/{ref}` reply or the streamed `CONFIRMS` — same JSON
/// shape) reports a CLOSE.
pub fn is_close_confirm(confirm: &serde_json::Value) -> bool {
    confirm.get("status").and_then(|s| s.as_str()).is_some_and(|s| CLOSE_STATUSES.contains(&s))
}

/// The identity of the ONE execution a deal confirm reports — the `trade_id` **both** IG lanes must
/// agree on.
///
/// ⚠ **`dealId` is not always the execution, and reading it as one silently breaks closing.** On an
/// OPEN it is: IG mints a new deal and the confirm names it. On a CLOSE it names the position being
/// closed. Measured against `demo-api.ig.com` (2026-08-21): opening `CS.D.EURUSD.MINI.IP` answered a
/// confirm with `dealId: DIAAAAYB6YDK2A7`, and closing that position answered a confirm with
/// **the same `dealId`** and `affectedDeals: [{DIAAAAYB6YDK2A7, FULLY_CLOSED}]`. A close fill keyed
/// on it therefore collides with its own opening fill in the engine's `seen_trade_ids`, is deduped
/// away, and the position never closes locally — the exact failure a close path exists to prevent,
/// arriving through the fix rather than around it.
///
/// `dealReference` is unique per deal REQUEST — open and close alike — and rides on BOTH lanes (the
/// stream driver already reads it to route the frame), so a close is keyed on it.
///
/// ⚠ **Both lanes MUST call this.** They agree today only because both read `dealId`; a divergence
/// would stop the sync fill and its streamed twin deduping against each other and turn a silent
/// dedup into a DOUBLE-booked position. `None` = no usable key on this frame — the caller decides
/// (`exec::map_confirm` synthesizes a per-confirm id from the coid, `decode_trade_confirm` refuses
/// the frame; their doc comments carry why the two answers differ).
pub fn confirm_trade_id(confirm: &serde_json::Value) -> Option<TradeId> {
    let field = |k: &str| confirm.get(k).and_then(|v| v.as_str()).filter(|v| !v.is_empty());
    // No fallback from `dealReference` to `dealId` on a close: that IS the collision.
    let key = if is_close_confirm(confirm) { field("dealReference") } else { field("dealId") };
    key.and_then(|k| TradeId::new(k).ok())
}

/// Decode an IG Lightstreamer `CONFIRMS` payload (JSON, same shape as `/confirms/{ref}`) into the
/// events it implies for `coid`. A streamed fill is a working order executing AFTER its resting
/// accept, so — unlike the sync confirm — it emits NO second `OrderAccepted`: just the dual-publish
/// pair on a fill, `OrderCanceled` on a delete/close, or `OrderRejected` on a rejection.
pub fn decode_trade_confirm(confirm: &serde_json::Value, coid: &str, ts: i64) -> Vec<Event> {
    let s = |k: &str| confirm.get(k).and_then(|v| v.as_str());
    let deal_status = s("dealStatus").unwrap_or("");
    if deal_status == "REJECTED" {
        return vec![Event::OrderRejected(OrderRejected {
            client_order_id: coid.to_string(),
            reason: s("reason").unwrap_or("REJECTED").to_string().into(),
            ts,
        })];
    }
    let status = s("status").unwrap_or("");
    // A working order that was deleted/closed without executing → cancel.
    if status == "DELETED" {
        return vec![Event::OrderCanceled(OrderCanceled {
            client_order_id: coid.to_string(),
            reason: s("reason").unwrap_or("").to_string().into(),
            ts,
        })];
    }
    // Otherwise this is an execution: OPEN / AMENDED / PARTIALLY_CLOSED with a level+size.
    let level = confirm.get("level").and_then(serde_json::Value::as_f64).unwrap_or(0.0);
    let size = confirm.get("size").and_then(serde_json::Value::as_f64).unwrap_or(0.0);
    if size <= 0.0 {
        return Vec::new(); // nothing executable to report
    }
    let side = if s("direction") == Some("SELL") { -1 } else { 1 };
    let symbol = s("epic").unwrap_or_default().to_string();
    // DROPPED, not synthesized — and the asymmetry with `exec.rs`'s `map_confirm` is deliberate.
    // That one is the SYNCHRONOUS confirm of one deal, where `coid` alone identifies the single fill
    // it can emit, so a coid-derived id is a true per-fill key. This is the Lightstreamer
    // trade-update STREAM, which delivers OPEN/AMENDED/CLOSED/PARTIALLY_CLOSED updates — several
    // distinct executions can arrive for one coid, so a coid-derived id here would collapse
    // genuinely different fills into one (the mirror-image defect: not a double-book, a LOST fill).
    // `confirm_trade_id` is the only per-execution identity on this wire, so without it the frame is
    // refused — and it is SHARED with `map_confirm` precisely so the two lanes cannot disagree.
    let Some(trade_id) = confirm_trade_id(confirm) else {
        tracing::warn!(
            venue = VENUE,
            %coid,
            status,
            "trade-update carries no usable execution id — dropping it; a coid-derived id would \
             collapse distinct executions on this stream into one"
        );
        return Vec::new();
    };
    let fill = FillEvent {
        trade_id,
        client_order_id: coid.to_string(),
        venue: VENUE.to_string().into(),
        symbol: symbol.into(),
        side,
        last_qty: size,
        last_px: level,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: String::new().into(),
        ts,
        mark_price: None,
        position_side: "BOTH".to_string().into(),
    };
    // Dual-publish: bare Fill (Account folds position/PnL) then the OrderFilled wrap (FSM).
    vec![
        Event::Fill(fill.clone()),
        Event::OrderFilled(OrderFilled { client_order_id: coid.to_string(), fill, ts }),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_update_line_splits_head_and_fields() {
        // subscription 1, item 1, three schema fields: CONFIRMS present, OPU empty, WOU null.
        let u = parse_update_line("U,1,1|{\"dealStatus\":\"ACCEPTED\"}||#").unwrap();
        assert_eq!(u.sub_id, 1);
        assert_eq!(u.item, 1);
        assert_eq!(u.fields.len(), 3);
        assert_eq!(u.fields[0].as_deref(), Some("{\"dealStatus\":\"ACCEPTED\"}"));
        assert_eq!(u.fields[1], None); // empty = unchanged
        assert_eq!(u.fields[2], None); // # = null
    }

    #[test]
    fn parse_update_line_ignores_control_lines() {
        assert!(parse_update_line("CONOK,S1a2b3c,50000,5000,*").is_none());
        assert!(parse_update_line("PROBE").is_none());
        assert!(parse_update_line("SUBOK,1,1,3").is_none());
    }

    #[test]
    fn confirm_fill_dual_publishes_without_reaccept() {
        let c: serde_json::Value = serde_json::from_str(
            r#"{"dealStatus":"ACCEPTED","status":"OPEN","dealId":"DIAAA9","epic":"CS.D.EURUSD.MINI.IP",
                "direction":"SELL","size":3.0,"level":1.0955,"reason":"SUCCESS"}"#,
        )
        .unwrap();
        let evs = decode_trade_confirm(&c, "coid-7", 99);
        assert_eq!(evs.len(), 2, "streamed fill = bare Fill + wrap, no re-Accept");
        match &evs[0] {
            Event::Fill(f) => {
                assert_eq!(f.client_order_id, "coid-7");
                assert_eq!(f.trade_id, "DIAAA9");
                assert_eq!(f.side, -1);
                assert_eq!(f.last_qty, 3.0);
                assert_eq!(f.last_px, 1.0955);
            }
            other => panic!("expected bare Fill first, got {other:?}"),
        }
        assert!(matches!(&evs[1], Event::OrderFilled(w) if w.fill.trade_id == "DIAAA9"));
    }

    #[test]
    fn confirm_rejected_and_deleted() {
        let rej: serde_json::Value =
            serde_json::from_str(r#"{"dealStatus":"REJECTED","reason":"INSUFFICIENT_BALANCE"}"#)
                .unwrap();
        let evs = decode_trade_confirm(&rej, "c1", 1);
        assert!(matches!(&evs[0], Event::OrderRejected(r) if r.reason == "INSUFFICIENT_BALANCE"));

        let del: serde_json::Value = serde_json::from_str(
            r#"{"dealStatus":"ACCEPTED","status":"DELETED","reason":"CANCELLED","size":1.0}"#,
        )
        .unwrap();
        let evs = decode_trade_confirm(&del, "c2", 2);
        assert_eq!(evs.len(), 1);
        assert!(matches!(&evs[0], Event::OrderCanceled(c) if c.client_order_id == "c2"));
    }
}
