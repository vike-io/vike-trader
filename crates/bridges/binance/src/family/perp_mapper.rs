//! Pure Binance-grammar USDⓈ-M perp ORDER_TRADE_UPDATE → vike event mapper. Exact port of
//! `exec/binance/perp_mapper.py`. Shared by vike-binance (`crate::perp_mapper`) and vike-aster
//! (`vike_aster::perp_mapper`) — Aster's perp user-data stream is Binance-verbatim (same event
//! names/field letters), so both venues call straight into this, each passing its own `venue`.
//!
//! The futures event nests order fields under `o` (spot executionReport is flat); same
//! field letters (s/c/x/X/i/l/L/n/t/m/S) plus the perp-only `ps` (positionSide).
//! x=="TRADE" is the ONLY fill execType. Dual-publish on fills. `mark_price` stays None
//! (the event carries no mark). autoclose-prefixed client ids emit PositionLiquidated
//! ONLY (suppressing the FillEvent prevents a double-fold in apply_liquidation).
//! ACCOUNT_UPDATE m=="FUNDING_FEE": one FundingEvent per non-zero `bc` row (received-
//! positive, no sign flip; keyed off a['B'], never a['P']); any B rows with an EXPLICIT
//! `wb` also emit AccountState (rows without wb are skipped so a bare funding row can
//! never clobber a just-applied FundingEvent with balance=0).
//!
//! **Every instrument this mapper names is the SERIES label, `<o.s>.P`.** A fill, a partial, a
//! liquidation and the TRADE_LITE early fill all carry [`crate::family::perp_series_symbol`] of the
//! frame's own `s` — `BTCUSDT` → `BTCUSDT.P`, the catalog spelling a perp engine is mounted on.
//! Until 2026-10-03 they carried the bare `s`, and an engine mounted on `BTCUSDT.P` folds a fill
//! only when the fill names that exact string (`vike_exec::ExecutionEngine::accepts_symbol`), so
//! every live aster and binance perp execution the STREAM delivered was dropped from the position:
//! the order reached FILLED through its coid-routed wrap while position, realized PnL and commission
//! stayed put, and a streamed liquidation did not close the position. Only a later history replay
//! (labelled with the mounted symbol) could book the trade — and it would book a liquidation as an
//! ordinary closing FILL where `allOrders` lists the venue's `autoclose-` order (not measured in this
//! tree; see `crate::family::history`'s `map_perp_history` doc). The label is the FRAME's symbol,
//! never the caller's: the stream is account-wide, so a foreign `ETHUSDT` fill is `ETHUSDT.P` and
//! never `BTCUSDT.P`. The
//! caller's `symbol` (the pump passes its mounted `.P` series symbol) is used only where a frame
//! names none — a TRADE frame without `s`, and the FUNDING_FEE wallet rows, which carry no symbol at
//! all. The history replay (`crate::family::history`'s `map_perp_history`) labels the same series
//! symbol, which is what lets `seen_trade_ids` fold a streamed fill and refuse its replayed copy.
//!
//! **Broker-prefix strip (unified cross-venue attribution, task 6, INBOUND half).** Every `c` read
//! here (TRADE_LITE's flat top-level field, `o.c` on `ORDER_TRADE_UPDATE`) is passed through
//! [`crate::family::order_map::strip_broker_coid_prefix`] before it becomes an event's
//! `client_order_id` — see `crate::family::event_mapper`'s module doc for the full rationale (same
//! encode/decode pair, unconditionally safe, this mapper's spot twin).
//!
//! **Opt-in TRADE_LITE early fast-fill hint** ([`map_perp_opts`], OFF by default): Binance's
//! slimmed `TRADE_LITE` event lands BEFORE the authoritative `ORDER_TRADE_UPDATE` for the same
//! trade. With the hint enabled the mapper emits an EARLY BARE `FillEvent` carrying the same
//! `trade_id` = `t`, so inventory-skew / the fill-rate breaker react sooner; the engine's always-on
//! `seen_trade_ids` guard then collapses it with the slow authoritative twin into ONE booking (no
//! qty double-count). OFF (the default, and Aster's only path) ⇒ TRADE_LITE is dropped exactly as
//! before, byte-identical. See [`map_perp_opts`] for the dedup proof + the commission caveat.

use serde_json::Value;
// `get_str_boolless as s` — the shared Bool-LESS `str(x)` coercion (a bool yields `""`, never
// Python's `"True"`), byte-identical to the local `fn s` this module used to declare. NOT
// `get_str`, which carries `json_str`'s `Bool` arm — see `get_str_boolless`'s doc.
use vike_bridge_core::json::{get_f64 as f, get_i64 as i, get_str_boolless as s};
use vike_model::events::LiquiditySide;
use vike_model::events::{
    AccountState, Event, FillEvent, FundingEvent, OrderAccepted, OrderCanceled, OrderExpired,
    PositionLiquidated, TradeId,
};

const LIQ_COID_PREFIXES: [&str; 3] = ["autoclose-", "adl_autoclose", "settlement_autoclose-"];

/// Whether a (broker-prefix-stripped) client order id is the VENUE's liquidation order — an
/// `autoclose-` / `adl_autoclose` / `settlement_autoclose-` id. Such an execution is a
/// `PositionLiquidated`, never a fill: its dedup set is the engine's `seen_liq_ids`, not the
/// account's fill ledger, so emitting it as a fill as well would close the position twice. Shared
/// with the history replay (`crate::family::history`'s `map_perp_history`) so the two lanes cannot
/// disagree about which executions are liquidations.
pub(crate) fn is_liquidation_coid(coid: &str) -> bool {
    LIQ_COID_PREFIXES.iter().any(|p| coid.starts_with(p))
}

/// The SERIES label of the instrument `frame` names: its own `s` through
/// [`crate::family::perp_series_symbol`] (`BTCUSDT` → `BTCUSDT.P`), or the caller's
/// `series_symbol` — already the `.P` label — when the frame carries no `s`. See the module doc.
fn frame_series_symbol(frame: &Value, series_symbol: &str) -> String {
    let exchange = s(frame, "s");
    if exchange.is_empty() {
        series_symbol.to_string()
    } else {
        crate::family::perp_series_symbol(&exchange)
    }
}

/// `str(o.get("ps", "BOTH"))` — the Binance grammar uses BOTH/LONG/SHORT literally.
fn ps(frame: &Value) -> String {
    match frame.get("ps") {
        Some(Value::String(v)) => v.clone(),
        Some(other) => s_from(other),
        None => "BOTH".to_string(),
    }
}

fn s_from(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}

/// Re-exported as `map_binance_perp` / `map_aster_perp` by the two venues. A byte-identical
/// delegating wrapper over [`map_perp_opts`] with the opt-in TRADE_LITE early-fill hint OFF — the
/// default path, and Aster's ONLY path: a `TRADE_LITE` frame is dropped exactly as before.
///
/// `symbol` is the caller's mounted SERIES symbol (`BTCUSDT.P` — the pump passes it): the label of
/// a funding row, which names no instrument, and of a trade frame that lacks `s`. Every frame that
/// names its instrument is labelled from that name instead — see the module doc.
pub fn map_perp(frame: &Value, venue: &str, symbol: &str) -> Vec<Event> {
    map_perp_opts(frame, venue, symbol, false)
}

/// Like [`map_perp`], plus the opt-in Binance USDⓈ-M **TRADE_LITE** early fast-fill hint. When
/// `early_trade_lite_fill` is `false` — every caller except Binance's own perp pump under
/// `venue.binance.trade_lite_fill = 1` — this is byte-identical to before the feature: a
/// `TRADE_LITE` frame maps to `[]`.
///
/// TRADE_LITE is Binance's slimmed, EARLY fill notification on the USDⓈ-M user stream; it lands
/// before the authoritative `ORDER_TRADE_UPDATE` for the same trade. Its payload is FLAT — the
/// fields sit at the frame TOP LEVEL, NOT nested under `o`: `e,E,T,s,q,p,m,c,S,L,l,t,i`. It OMITS
/// commission (`n`/`N`), order status (`X`), position side (`ps`) and realized PnL, so it CANNOT be
/// the authoritative fold — only an early skew hint. When enabled we emit a BARE [`FillEvent`] (no
/// `OrderFilled`/`OrderPartiallyFilled` wrap) built from `L`/`l`/`t`/`S`/`m`, carrying the same
/// `trade_id` = `t` the authoritative fill uses (the perp fill arm below reads `s(o,"t")`).
///
/// **Dedup / no double-count (why this is safe):** that shared `t` is the point. The execution
/// engine's always-on `seen_trade_ids` guard folds a bare `Event::Fill` into `Account::apply_fill`
/// only the FIRST time it sees a given `trade_id`, dropping any later fill on the same id (its
/// reconnect-replay guard). So the early bare fill and the slow authoritative twin — same `t` —
/// collapse into exactly ONE `apply_fill`: the qty is booked once. The later `ORDER_TRADE_UPDATE`
/// still drives the FSM, because its `OrderFilled`/`OrderPartiallyFilled` wrap is deduped through a
/// SEPARATE `seen_fsm_trade_ids` set — order lifecycle stays authoritative.
///
/// **Commission CAVEAT (why this is opt-in, OFF by default):** because the early fill wins the
/// `seen_trade_ids` race, the authoritative fill's own bare `Event::Fill` — the one carrying the
/// real commission `n`/`N` — is the duplicate the engine DROPS. So under this mode commission is
/// booked as ZERO for early-hinted fills (`Account` balance is off by the fee; position qty and
/// order state stay correct). Enabling it trades commission-attribution accuracy for earlier
/// inventory-skew / fill-rate-breaker reaction. Liquidation autoclose fills are suppressed here (as
/// in the authoritative arm) so the early bare fill can't double-fold against `PositionLiquidated`.
pub fn map_perp_opts(
    frame: &Value,
    venue: &str,
    symbol: &str,
    early_trade_lite_fill: bool,
) -> Vec<Event> {
    if !frame.is_object() {
        return Vec::new();
    }
    if frame.get("e").and_then(|e| e.as_str()) == Some("ACCOUNT_UPDATE") {
        let empty = serde_json::json!({});
        let a = frame.get("a").filter(|a| a.is_object()).unwrap_or(&empty);
        let ts = i(frame, "T");
        let no_rows = vec![];
        let wallet_rows = a.get("B").and_then(|b| b.as_array()).unwrap_or(&no_rows);
        let mut out = Vec::new();
        if a.get("m").and_then(|m| m.as_str()) == Some("FUNDING_FEE") {
            for b in wallet_rows {
                let bc = f(b, "bc");
                if bc == 0.0 {
                    continue;
                }
                out.push(Event::Funding(FundingEvent {
                    venue: venue.to_string().into(),
                    symbol: symbol.to_string().into(),
                    position_side: "BOTH".to_string().into(),
                    funding_rate: 0.0,
                    amount: bc,
                    mark_price: None,
                    ts,
                    // A bridge holds ONE credential set and knows nothing about accounts — the
                    // MOUNT stamps this (`vike_exec::EventSender::routed`), exactly as it does for
                    // `AccountState`. See `vike_model::events::FundingEvent::route_key`.
                    route_key: None,
                }));
            }
        }
        let mut balances: Vec<(String, f64)> = Vec::new();
        for b in wallet_rows {
            let Some(wb_val) = b.get("wb") else {
                continue; // no total-balance snapshot in this row — skip it
            };
            // Python float(b["wb"] or 0) inside try/except — unparseable skips the row
            let wb = match wb_val {
                Value::Number(n) => n.as_f64(),
                Value::String(v) => v.parse::<f64>().ok(),
                Value::Null => Some(0.0),
                _ => None,
            };
            let (Some(wb), asset) = (wb, s(b, "a")) else { continue };
            if !asset.is_empty() {
                balances.push((asset, wb));
            }
        }
        if !balances.is_empty() {
            out.push(Event::AccountState(AccountState {
                venue: venue.to_string().into(),
                balances,
                ts,
                // A bridge holds ONE credential set and knows no account labels: the MOUNT stamps
                // the route key (`vike_mount::account_event_sender`), never a venue adapter.
                route_key: None,
            }));
        }
        return out;
    }
    // Opt-in EARLY fast-fill hint from Binance's slimmed TRADE_LITE event (see this fn's doc). OFF
    // (the default, and Aster's only path) ⇒ the condition is false, we fall through to the guard
    // below, and the frame is dropped — byte-identical to before this feature.
    if early_trade_lite_fill && frame.get("e").and_then(|e| e.as_str()) == Some("TRADE_LITE") {
        // TRADE_LITE is FLAT: its fields sit at the frame TOP LEVEL, not nested under `o`. Broker-
        // prefix stripped (task 6) — see this module's doc.
        let coid = crate::family::order_map::strip_broker_coid_prefix(&s(frame, "c")).to_string();
        // Mirror the authoritative arm's liquidation suppression: an autoclose fill's position move
        // arrives via ORDER_TRADE_UPDATE → PositionLiquidated, which folds through a SEPARATE dedup
        // set (`seen_liq_ids`), so a bare early fill on the same trade would double-fold. Drop it
        // and let the authoritative liquidation path own the move.
        if is_liquidation_coid(&coid) {
            return Vec::new();
        }
        let ts = i(frame, "T");
        // The early hint's ENTIRE correctness argument is that its `t` equals the authoritative
        // `ORDER_TRADE_UPDATE`'s `t`, so `seen_trade_ids` collapses the two into one booking. With
        // no `t` there is no such argument left: the hint would fold once here and AGAIN off the
        // slow twin. Drop it — the authoritative arm below still books this trade, so nothing is
        // lost but the few-ms head start.
        let Ok(trade_id) = TradeId::new(s(frame, "t")) else {
            tracing::warn!(
                venue,
                symbol = %frame_series_symbol(frame, symbol),
                client_order_id = %coid,
                "TRADE_LITE carries no `t` (tradeId) — dropping the early fill hint; without the \
                 shared dedup key it would double-fold against its ORDER_TRADE_UPDATE twin"
            );
            return Vec::new();
        };
        let fill = FillEvent {
            // SAME key the authoritative fill reads (`t`, perp fill arm below) — the engine's
            // always-on `seen_trade_ids` guard collapses this early fill and its slow twin into ONE
            // `Account::apply_fill`, so the qty is booked exactly once (no double-count).
            trade_id,
            client_order_id: coid,
            venue: venue.to_string().into(),
            // The series label — the same one its authoritative twin carries, so the two agree on
            // `Account::apply_fill`'s fingerprint as well as on the id.
            symbol: frame_series_symbol(frame, symbol).into(),
            side: if frame.get("S").and_then(|v| v.as_str()) == Some("BUY") { 1 } else { -1 },
            last_qty: f(frame, "l"), // `l` = LAST filled qty
            last_px: f(frame, "L"),  // `L` = LAST filled price
            // TRADE_LITE omits `n`. The authoritative fill's real commission rides its OWN bare
            // Event::Fill, which the engine DROPS as a seen_trade_ids duplicate — so commission is
            // booked as 0 under this opt-in mode (the documented cost; see this fn's doc CAVEAT).
            commission: 0.0,
            commission_asset: "".into(), // TRADE_LITE omits `N`
            liquidity_side: if frame.get("m").and_then(|m| m.as_bool()).unwrap_or(false) {
                LiquiditySide::Maker
            } else {
                LiquiditySide::Taker
            },
            ts,
            mark_price: None,                // TRADE_LITE carries no mark
            position_side: ps(frame).into(), // TRADE_LITE omits `ps` → defaults "BOTH"
        };
        // BARE fill only — NO OrderFilled/OrderPartiallyFilled wrap. The FSM stays driven by the
        // authoritative ORDER_TRADE_UPDATE's wrap (deduped via seen_fsm_trade_ids).
        return vec![Event::Fill(fill)];
    }
    if frame.get("e").and_then(|e| e.as_str()) != Some("ORDER_TRADE_UPDATE") {
        return Vec::new();
    }
    let Some(o) = frame.get("o").filter(|o| o.is_object()) else {
        return Vec::new();
    };
    // Broker-prefix stripped (task 6) — see this module's doc.
    let coid = crate::family::order_map::strip_broker_coid_prefix(&s(o, "c")).to_string();
    let ts = i(frame, "T");
    match o.get("x").and_then(|x| x.as_str()) {
        Some("NEW") => vec![Event::OrderAccepted(OrderAccepted {
            client_order_id: coid,
            venue_order_id: Some(s(o, "i").into()),
            ts,
        })],
        Some("CANCELED") => vec![Event::OrderCanceled(OrderCanceled {
            client_order_id: coid,
            reason: String::new().into(),
            ts,
        })],
        Some("EXPIRED") => vec![Event::OrderExpired(OrderExpired { client_order_id: coid, ts })],
        Some("TRADE") => {
            if is_liquidation_coid(&coid) {
                return vec![Event::PositionLiquidated(PositionLiquidated {
                    venue: venue.to_string().into(),
                    // The series label, or the engine's liquidation arm drops it at
                    // `accepts_symbol` and the position is never closed.
                    symbol: frame_series_symbol(o, symbol).into(),
                    position_side: ps(o).into(),
                    qty: f(o, "l"),
                    liq_price: f(o, "L"),
                    fee: f(o, "n"),
                    ts,
                    trade_id: s(o, "t").into(), // OTU trade id — same 't' the fill path reads
                    route_key: None,            // stamped at the mount — see the funding arm above
                })]; // liquidation -> PositionLiquidated ONLY
            }
            // `o.t` (tradeId) is the fill DEDUP key and Binance documents it on every
            // ORDER_TRADE_UPDATE whose `o.x` is TRADE, so an absent/empty one is a malformed frame.
            // Same verdict as the spot twin (`crate::family::event_mapper`'s `map_execution_report`):
            // DROP the fill and its wrap together, because the audit-A3 resync replays this fill
            // from `userTrades` (`crate::family::history`'s `map_perp_history`) and an id-less fill
            // escapes `seen_trade_ids`, double-booking commission and realized PnL. Nothing is
            // synthesized — a clock/counter id differs on replay and so defeats the dedup entirely.
            let Ok(trade_id) = TradeId::new(s(o, "t")) else {
                tracing::warn!(
                    venue,
                    symbol = %frame_series_symbol(o, symbol),
                    client_order_id = %coid,
                    "ORDER_TRADE_UPDATE x=TRADE carries no `t` (tradeId) — dropping the fill and \
                     its wrap; an un-dedupable fill double-books on resync"
                );
                return Vec::new();
            };
            let fill = FillEvent {
                trade_id,
                client_order_id: coid.clone(),
                venue: venue.to_string().into(),
                // The series label — see the module doc for what the bare `o.s` cost.
                symbol: frame_series_symbol(o, symbol).into(),
                side: if o.get("S").and_then(|v| v.as_str()) == Some("BUY") { 1 } else { -1 },
                last_qty: f(o, "l"),
                last_px: f(o, "L"),
                commission: f(o, "n"),
                commission_asset: s(o, "N").into(),
                liquidity_side: if o.get("m").and_then(|m| m.as_bool()).unwrap_or(false) {
                    LiquiditySide::Maker
                } else {
                    LiquiditySide::Taker
                },
                ts,
                mark_price: None, // ORDER_TRADE_UPDATE has no mark
                position_side: ps(o).into(),
            };
            let is_filled = o.get("X").and_then(|x| x.as_str()) == Some("FILLED");
            vike_bridge_core::terminal_events(coid, fill, ts, is_filled)
        }
        _ => Vec::new(), // CALCULATED / AMENDMENT / unknown
    }
}

#[path = "trade_lite_tests.rs"]
#[cfg(test)]
mod trade_lite_tests;
