//! IG Lightstreamer trade-update decode — the delayed working-order fills the sync `/confirms`
//! call can't carry, plus the audit-A3 `/history/activity` replay.
//!
//! IG's Lightstreamer `TRADE:{accountId}` subscription carries three text fields per update —
//! `CONFIRMS` (deal confirmation, same JSON shape as `GET /confirms/{ref}`), `OPU` (open-position
//! update) and `WOU` (working-order update). This module is PURE: (1) parse one TLCP update line
//! into its field values, and (2) decode a deal-shaped payload into canonical events, honoring the
//! dual-publish contract (bare `Event::Fill` for the Account fold + the `OrderFilled` wrap for the
//! FSM), so a streamed working-order fill folds identically to an inline market fill. The transport
//! (streaming create_session + control subscribe + reconnect) is the driver's job — see `stream`.
//! WHICH of the three fields the driver decodes, and why only one, is `stream.rs`'s `events_for_line`.

use vike_model::events::{Event, FillEvent, OrderCanceled, OrderRejected, TradeId};

use crate::feed::decode::lightstreamer::{FieldDelta, LightstreamerFrame, parse_frame};

const VENUE: &str = "ig";

/// One parsed TLCP `U` (update) line: `U,<subId>,<item>,f1|f2|...`. Field values are already
/// un-escaped: TLCP `$` → empty string, `#` → "null"/absent (returned as `None`), a truly empty
/// segment → `None` ("unchanged", which for IG's DISTINCT trade items means "no value this
/// update"), a `^n` run → n `None`s. Non-empty fields are returned as `Some(text)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LightstreamerUpdateTrade {
    pub sub_id: u32,
    pub item: u32,
    /// Field values in schema order (`CONFIRMS OPU WOU`), `None` = empty/unchanged/null.
    pub fields: Vec<Option<String>>,
}

/// Parse one Lightstreamer TLCP text line. Returns `Some` only for a data update (`U,...`); control
/// lines (CONOK/PROBE/LOOP/SYNC/SUBOK/...) return `None` (the driver handles those separately).
///
/// ⚠ **The grammar is the spec's, as captured on the demo gateway (2026-10-30): the field list is the
/// comma-separated THIRD argument.** `U,1,1,{CONFIRMS}|#|#` (a deal confirmed), `U,1,1,#|{OPU}|` (a position
/// opened or closed) and `U,1,1,||{WOU}` (a working order) are the three shapes IG sent. This parser used to split
/// on the first PIPE (`U,<sub>,<item>|f1|...`), so the first field, `CONFIRMS`, was swallowed by the head and the
/// frame's `fields[0]` was the OPU: the lane decoded every position update as if it were a deal confirm
/// (a position CLOSE became an `OrderCanceled` of its entry order).
///
/// **One decoder for the line: `crate::feed::decode::lightstreamer::parse_frame`'s**, the market-data codec's, viewed as
/// value-or-nothing. It percent-decodes (the spec escapes `|`, `%`, CR and LF) and expands a `^n` run into n
/// unchanged slots, so a field after a run keeps its schema index: this lane used to read `^n` as one text field,
/// which would have moved a `WOU` into the `OPU`'s slot. A malformed run (`^0`, not a number, wider than the
/// codec's bound) refuses the whole line, as it does on the market lane.
pub(crate) fn parse_update_line(line: &str) -> Option<LightstreamerUpdateTrade> {
    let Some(LightstreamerFrame::Update(update)) = parse_frame(line) else { return None };
    let fields = update
        .fields
        .into_iter()
        .map(|field| match field {
            FieldDelta::Value(text) => Some(text),
            FieldDelta::Unchanged | FieldDelta::Null => None,
        })
        .collect();
    Some(LightstreamerUpdateTrade { sub_id: update.sub_id, item: update.item, fields })
}

/// The IG deal-confirm `status` values that report a position being CLOSED rather than opened.
/// `PARTIALLY_CLOSED` belongs here for the same reason `CLOSED` does — it too reports an execution
/// against a deal that already existed.
const CLOSE_STATUSES: &[&str] = &["CLOSED", "PARTIALLY_CLOSED"];

/// Whether a deal confirm (sync `/confirms/{ref}` reply or the streamed `CONFIRMS` — same JSON
/// shape) reports a CLOSE.
pub(crate) fn is_close_confirm(confirm: &serde_json::Value) -> bool {
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
/// (`execution::map_confirm` synthesizes a per-confirm id from the order client id, `decode_trade_confirm` refuses
/// the frame; their doc comments carry why the two answers differ).
pub(crate) fn confirm_trade_id(confirm: &serde_json::Value) -> Option<TradeId> {
    let field = |k: &str| confirm.get(k).and_then(|v| v.as_str()).filter(|v| !v.is_empty());
    // No fallback from `dealReference` to `dealId` on a close: that IS the collision.
    let key = if is_close_confirm(confirm) { field("dealReference") } else { field("dealId") };
    key.and_then(|k| TradeId::new(k).ok())
}

/// A rejected deal's reason as the operator reads it: IG's own code FIRST and unchanged (what to search IG's
/// documentation for), then, for the codes that say the MARKET's state or its DEALING RULES refused the deal, the
/// field of `GET /markets/{epic}` that answers it, in words. IG's code alone sent a trader to the platform to learn
/// that `MARKET_CLOSED` meant `snapshot.marketStatus` and `MINIMUM_ORDER_SIZE_ERROR` meant `dealingRules.minDealSize`.
/// Any other code (a balance, a level, a position, one this table has not met) comes back as it was.
pub(crate) fn rejection_words(code: &str) -> String {
    let words = match code {
        "MARKET_CLOSED" | "MARKET_CLOSED_WITH_EDITS" | "MARKET_OFFLINE" => {
            "the market takes no deal now: its snapshot.marketStatus is not TRADEABLE (CLOSED, EDITS_ONLY or OFFLINE); try again when IG reports TRADEABLE"
        }
        "CLOSING_ONLY_TRADES_ACCEPTED_ON_THIS_MARKET" => {
            "the market accepts closings only now (marketStatus not TRADEABLE; the stream's DLG_FLAG says CLOSINGSONLY): an order that opens exposure is refused, one that closes is not"
        }
        "MINIMUM_ORDER_SIZE_ERROR" => "the size is below the market's dealingRules.minDealSize",
        "MARKET_ORDERS_NOT_ALLOWED_ON_INSTRUMENT" => {
            "dealingRules.marketOrderPreference does not allow a market order on this market"
        }
        _ => return code.to_string(),
    };
    format!("{code} ({words})")
}

/// **A `REJECTED` confirm whose `reason` says IG itself does not know the deal's result** (IG-3): the confirms page
/// (`https://labs.ig.com/reference/confirms-deal-reference.html`) words `UNKNOWN` "The operation resulted in an unknown
/// result condition. Check transaction history" and `GENERAL_ERROR` "an error has occurred but no detailed information
/// is available. Check transaction history". The deal may have been done, so such a confirm refuses nothing: the write's
/// outcome is UNKNOWN (decision 0303) and is left to what reads the deal (the `/confirms` re-ask, the trade stream's
/// `OPU` of the position, the watchdog, reconcile), never ended here. Neither word was ever captured; both are the page's.
pub(crate) fn rejected_with_unknown_result(confirm: &serde_json::Value) -> bool {
    let s = |k: &str| confirm.get(k).and_then(|v| v.as_str());
    s("dealStatus") == Some("REJECTED") && matches!(s("reason"), Some("UNKNOWN" | "GENERAL_ERROR"))
}

/// Decode an IG Lightstreamer `CONFIRMS` payload (JSON, same shape as `/confirms/{ref}`) into the
/// events it implies for `order_client_id`. A streamed fill is a working order executing AFTER its resting
/// accept, so — unlike the sync confirm — it emits NO second `OrderAccepted`: just the dual-publish
/// pair on a fill, `OrderCanceled` on a delete/close, or `OrderRejected` on a rejection.
pub fn decode_trade_confirm(
    confirm: &serde_json::Value,
    order_client_id: &str,
    ts: i64,
) -> Vec<Event> {
    let s = |k: &str| confirm.get(k).and_then(|v| v.as_str());
    let deal_status = s("dealStatus").unwrap_or("");
    if rejected_with_unknown_result(confirm) {
        return Vec::new();
    }
    if deal_status == "REJECTED" {
        return vec![Event::OrderRejected(OrderRejected {
            cause: None,
            order_client_id: order_client_id.to_string(),
            reason: rejection_words(s("reason").unwrap_or("REJECTED")).into(),
            timestamp: ts,
            refusal_id: vike_model::events::refusal_id_of(
                vike_model::OrderRefusalId::VenueRefused.word(),
            ),
        })];
    }
    let status = s("status").unwrap_or("");
    // A working order that was deleted/closed without executing → cancel.
    if status == "DELETED" {
        return vec![Event::OrderCanceled(OrderCanceled {
            order_client_id: order_client_id.to_string(),
            reason: s("reason").unwrap_or("").to_string().into(),
            timestamp: ts,
            post_only_would_cross: false,
            refusal_id: None,
        })];
    }
    // An AMENDED confirm reports a working order's level (or a position's stops) being CHANGED. It
    // carries a `level` and a `size` like an execution does, and reading it as one would book a
    // fill nobody traded — so it is not one. (`execution/mod.rs` does not record an amend's `dealReference`,
    // so none should arrive; this is the second belt.)
    if status == "AMENDED" {
        return Vec::new();
    }
    // Otherwise this is an execution: OPEN / PARTIALLY_CLOSED / CLOSED with a level+size.
    let level = confirm.get("level").and_then(serde_json::Value::as_f64).filter(|l| *l > 0.0);
    let size = confirm.get("size").and_then(serde_json::Value::as_f64).unwrap_or(0.0);
    if size <= 0.0 {
        return Vec::new(); // nothing executable to report
    }
    let side = if s("direction") == Some("SELL") { -1 } else { 1 };
    // The user name of the kept row whose epic it states (decision 0290), else the epic, said.
    let symbol = crate::epic::user_name_or_epic(s("epic").unwrap_or_default());
    // DROPPED, not synthesized — and the asymmetry with `execution/mod.rs`'s `map_confirm` is deliberate.
    // That one is the SYNCHRONOUS confirm of one deal, where `order_client_id` alone identifies the single fill
    // it can emit, so an order-client-id-derived id is a true per-fill key. This is the Lightstreamer
    // trade-update STREAM, which delivers OPEN/CLOSED/PARTIALLY_CLOSED executions — several
    // distinct executions can arrive for one order client id, so an order-client-id-derived id here would collapse
    // genuinely different fills into one (the mirror-image defect: not a double-book, a LOST fill).
    // `confirm_trade_id` is the only per-execution identity on this wire, so without it the frame is
    // refused — and it is SHARED with `map_confirm` precisely so the two lanes cannot disagree.
    let Some(trade_id) = confirm_trade_id(confirm) else {
        tracing::warn!(
            target: "vike_ig::event_mapper",
            venue = VENUE,
            %order_client_id,
            status,
            "trade-update carries no usable execution id — dropping it; an order-client-id-derived id would \
             collapse distinct executions on this stream into one"
        );
        return Vec::new();
    };
    // The engine drops only a non-finite fill, so a defaulted 0 would open a position at price 0.
    let Some(level) = level else {
        tracing::error!(
            target: "vike_ig::event_mapper",
            venue = VENUE,
            %order_client_id,
            %trade_id,
            status,
            "trade-update execution states no positive `level` — refusing the fill rather than booking it at 0"
        );
        return Vec::new();
    };
    let fill = FillEvent {
        trade_id,
        order_client_id: order_client_id.to_string(),
        venue: VENUE.to_string().into(),
        symbol: symbol.into(),
        side,
        last_qty: size,
        last_price: level,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: String::new().into(),
        timestamp: ts,
        mark_price: None,
        position_side: vike_model::events::PositionSide::Both,
    };
    // Dual-publish: bare Fill (Account folds position/PnL) then the OrderFilled wrap (FSM).
    vike_bridge_core::terminal_events(order_client_id.to_string(), fill, ts, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::assert_matches;

    #[test]
    fn parse_update_line_splits_head_and_fields() {
        // subscription 1, item 1, three schema fields: CONFIRMS present, OPU empty, WOU null.
        let u = parse_update_line("U,1,1,{\"dealStatus\":\"ACCEPTED\"}||#").unwrap();
        assert_eq!(u.sub_id, 1);
        assert_eq!(u.item, 1);
        assert_eq!(u.fields.len(), 3);
        assert_eq!(u.fields[0].as_deref(), Some("{\"dealStatus\":\"ACCEPTED\"}"));
        assert_eq!(u.fields[1], None); // empty = unchanged
        assert_eq!(u.fields[2], None); // # = null
    }

    /// **The three shapes IG sent on 2026-10-30 (demo gateway, a raw capture of the TRADE item), each JSON in its
    /// own field**: a deal confirmed is field 0 (`CONFIRMS`), a position opened or closed is field 1 (`OPU`), a
    /// working order is field 2 (`WOU`). The old parse split on the first pipe, so field 0 was lost and the OPU
    /// came out as field 0: RED on it for all three.
    #[test]
    fn the_three_captured_shapes_put_each_json_in_its_own_field() {
        let json = |u: &LightstreamerUpdateTrade, field: usize| -> Option<serde_json::Value> {
            u.fields[field].as_deref().map(|text| serde_json::from_str(text).expect("JSON"))
        };
        // a confirm of an open: CONFIRMS | null | null
        let u = parse_update_line(
            r##"U,1,1,{"dealReference":"vk1","dealId":"DIAAAAYR82EEVBM","status":"OPEN","dealStatus":"ACCEPTED"}|#|#"##,
        )
        .unwrap();
        assert_eq!((u.sub_id, u.item, u.fields.len()), (1, 1, 3));
        assert_eq!(json(&u, 0).unwrap()["dealReference"], "vk1");
        assert_eq!((u.fields[1].as_deref(), u.fields[2].as_deref()), (None, None));
        // the position that open made: null | OPU | (nothing)
        let u = parse_update_line(
            r##"U,1,1,#|{"dealReference":"vk1","status":"OPEN","timestamp":"2026-10-30T19:47:04.546"}|"##,
        )
        .unwrap();
        assert_eq!(u.fields[0], None);
        assert_eq!(json(&u, 1).unwrap()["status"], "OPEN");
        assert_eq!(u.fields[2], None);
        // the close of that position: the OPU again, and the first field EMPTY rather than `#`
        let u =
            parse_update_line(r##"U,1,1,|{"dealReference":"vk1","status":"DELETED","size":0}|"##)
                .unwrap();
        assert_eq!(json(&u, 1).unwrap()["status"], "DELETED");
        // a working order: (nothing) | (nothing) | WOU
        let u = parse_update_line(
            r##"U,1,1,||{"dealReference":"vk1","status":"OPEN","orderType":"LIMIT"}"##,
        )
        .unwrap();
        assert_eq!((u.fields[0].as_deref(), u.fields[1].as_deref()), (None, None));
        assert_eq!(json(&u, 2).unwrap()["orderType"], "LIMIT");
    }

    /// The spec escapes `%` (and `|`, CR, LF) in a field value; a value with none reads as it came.
    #[test]
    fn a_field_value_is_percent_decoded() {
        let u = parse_update_line("U,1,1,{\"reason\":\"100%25 a%7Cb\"}||").unwrap();
        assert_eq!(u.fields[0].as_deref(), Some("{\"reason\":\"100% a|b\"}"));
    }

    /// A `^n` run is n unchanged slots (the spec's, and the market lane's reading), so the `WOU` after a `^2`
    /// stays field 2 and is never read as field 1's `OPU`. A malformed run refuses the line, as on the market lane.
    #[test]
    fn an_unchanged_run_keeps_every_later_field_at_its_schema_index() {
        let u = parse_update_line(r#"U,1,1,^2|{"status":"OPEN","orderType":"LIMIT"}"#).unwrap();
        assert_eq!(u.fields.len(), 3);
        assert_eq!((u.fields[0].as_deref(), u.fields[1].as_deref()), (None, None));
        assert_eq!(u.fields[2].as_deref(), Some(r#"{"status":"OPEN","orderType":"LIMIT"}"#));
        assert!(parse_update_line("U,1,1,^0|{}").is_none());
        assert!(parse_update_line("U,1,1,^x|{}").is_none());
    }

    /// The old shape, a pipe straight after the item, is not the wire: it names no item.
    #[test]
    fn a_pipe_after_the_item_is_not_an_update() {
        assert!(parse_update_line("U,1,1|{\"a\":1}||").is_none());
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
        let evs = decode_trade_confirm(&c, "order-client-id-7", 99);
        assert_eq!(evs.len(), 2, "streamed fill = bare Fill + wrap, no re-Accept");
        match &evs[0] {
            Event::Fill(f) => {
                assert_eq!(f.order_client_id, "order-client-id-7");
                assert_eq!(f.trade_id, "DIAAA9");
                assert_eq!(f.side, -1);
                assert_eq!(f.last_qty, 3.0);
                assert_eq!(f.last_price, 1.0955);
            }
            other => panic!("expected bare Fill first, got {other:?}"),
        }
        assert_matches!(&evs[1], Event::OrderFilled(w) if w.fill.trade_id == "DIAAA9");
    }

    /// An execution with no positive `level` is refused, never booked at price 0; the same frame with its level
    /// still fills at that level.
    #[test]
    fn an_execution_without_a_level_is_not_a_fill() {
        let mut c = serde_json::json!({"dealStatus":"ACCEPTED","status":"OPEN","dealId":"DIAAA9",
            "epic":"CS.D.EURUSD.MINI.IP","direction":"BUY","size":3.0});
        assert!(decode_trade_confirm(&c, "order-client-id-7", 99).is_empty(), "no level");
        for refused in [serde_json::Value::Null, 0.0.into(), (-1.0).into(), "1.0955".into()] {
            c["level"] = refused;
            assert!(decode_trade_confirm(&c, "order-client-id-7", 99).is_empty(), "{c}");
        }
        c["level"] = 1.0955.into();
        assert_matches!(
            decode_trade_confirm(&c, "order-client-id-7", 99).as_slice(),
            [Event::Fill(f), Event::OrderFilled(_)] if f.last_price == 1.0955 && f.last_qty == 3.0
        );
    }

    /// An amend's confirm has a `level` and a `size` and is NOT an execution: reading it as one
    /// would book a fill for a working order that merely moved.
    #[test]
    fn an_amended_confirm_is_not_a_fill() {
        let c: serde_json::Value = serde_json::from_str(
            r#"{"dealStatus":"ACCEPTED","status":"AMENDED","dealId":"DIAAA9","dealReference":"R9",
                "epic":"CS.D.EURUSD.MINI.IP","direction":"BUY","size":1.0,"level":1.0955}"#,
        )
        .unwrap();
        assert!(decode_trade_confirm(&c, "order-client-id-9", 1).is_empty());
    }

    /// IG-3, the streamed twin: a rejection whose reason says IG does not know the result is no refusal here either.
    #[test]
    fn a_streamed_rejection_that_does_not_know_the_result_ends_nothing() {
        for reason in ["UNKNOWN", "GENERAL_ERROR"] {
            let c = serde_json::json!({"dealStatus": "REJECTED", "reason": reason});
            assert!(rejected_with_unknown_result(&c));
            assert!(decode_trade_confirm(&c, "c1", 1).is_empty(), "{reason}");
        }
        assert!(!rejected_with_unknown_result(
            &serde_json::json!({"dealStatus": "ACCEPTED", "reason": "UNKNOWN"})
        ));
        assert!(!rejected_with_unknown_result(
            &serde_json::json!({"dealStatus": "REJECTED", "reason": "MARKET_CLOSED"})
        ));
    }

    /// The streamed twin words a market-state rejection like the synchronous mapper does (`execution/mod.rs`'s
    /// `map_confirm`): the code first, then the market details field that answers it.
    #[test]
    fn a_streamed_rejection_for_the_markets_state_names_the_field_that_answers_it() {
        let reject: serde_json::Value =
            serde_json::from_str(r#"{"dealStatus":"REJECTED","reason":"MARKET_OFFLINE"}"#).unwrap();
        let evs = decode_trade_confirm(&reject, "c1", 1);
        assert_matches!(
            &evs[0],
            Event::OrderRejected(r) if r.reason.to_string().starts_with("MARKET_OFFLINE ")
                && r.reason.to_string().contains("marketStatus")
        );
    }

    #[test]
    fn confirm_rejected_and_deleted() {
        let reject: serde_json::Value =
            serde_json::from_str(r#"{"dealStatus":"REJECTED","reason":"INSUFFICIENT_BALANCE"}"#)
                .unwrap();
        let evs = decode_trade_confirm(&reject, "c1", 1);
        assert_matches!(&evs[0], Event::OrderRejected(r) if r.reason == "INSUFFICIENT_BALANCE");

        let del: serde_json::Value = serde_json::from_str(
            r#"{"dealStatus":"ACCEPTED","status":"DELETED","reason":"CANCELLED","size":1.0}"#,
        )
        .unwrap();
        let evs = decode_trade_confirm(&del, "c2", 2);
        assert_eq!(evs.len(), 1);
        assert_matches!(&evs[0], Event::OrderCanceled(c) if c.order_client_id == "c2");
    }
}
