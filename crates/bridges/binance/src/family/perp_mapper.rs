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

/// `str(o.get("s", symbol) or symbol)` — frame symbol, falling back on empty/absent.
fn sym_or(frame: &Value, key: &str, fallback: &str) -> String {
    let v = s(frame, key);
    if v.is_empty() { fallback.to_string() } else { v }
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
pub fn map_perp(frame: &Value, venue: &str, symbol: &str) -> Vec<Event> {
    map_perp_opts(frame, venue, symbol, false)
}

/// Like [`map_perp`], plus the opt-in Binance USDⓈ-M **TRADE_LITE** early fast-fill hint. When
/// `early_trade_lite_fill` is `false` — every caller except Binance's own perp pump under
/// `VIKE_BINANCE_TRADE_LITE_FILL=1` — this is byte-identical to before the feature: a `TRADE_LITE`
/// frame maps to `[]`.
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
        if LIQ_COID_PREFIXES.iter().any(|p| coid.starts_with(p)) {
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
                symbol = %sym_or(frame, "s", symbol),
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
            symbol: sym_or(frame, "s", symbol).into(),
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
            if LIQ_COID_PREFIXES.iter().any(|p| coid.starts_with(p)) {
                return vec![Event::PositionLiquidated(PositionLiquidated {
                    venue: venue.to_string().into(),
                    symbol: sym_or(o, "s", symbol).into(),
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
                    symbol = %sym_or(o, "s", symbol),
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
                symbol: sym_or(o, "s", symbol).into(),
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

#[cfg(test)]
mod trade_lite_tests {
    //! The opt-in Binance USDⓈ-M TRADE_LITE early fast-fill hint ([`map_perp_opts`]): OFF is a
    //! byte-identical drop (today's behavior); ON emits ONE early BARE FillEvent built from
    //! `L`/`l`/`t`/`S`/`m` carrying the SAME `trade_id` = `t` the authoritative ORDER_TRADE_UPDATE
    //! fill uses — so the engine's `seen_trade_ids` guard collapses the two into ONE qty booking.
    use super::*;
    use serde_json::json;
    use vike_model::events::PositionSide;

    /// A representative flat TRADE_LITE frame (fields at the TOP LEVEL, not nested under `o`).
    fn trade_lite_frame() -> Value {
        json!({
            "e": "TRADE_LITE",
            "T": 199,
            "s": "BTCUSDT",
            "q": "0.5",     // orig qty — unused (the fill takes LAST filled `l`)
            "p": "0",       // orig price — unused (the fill takes LAST filled `L`)
            "m": true,      // maker
            "c": "myorder-1",
            "S": "SELL",
            "L": "64000.0", // LAST filled price
            "l": "0.5",     // LAST filled qty
            "t": 777,       // trade id
            "i": 88         // order id (FillEvent has no slot; unused, like the authoritative arm)
        })
    }

    /// The authoritative ORDER_TRADE_UPDATE twin of [`trade_lite_frame`] — SAME trade id `t`=777.
    fn order_trade_update_twin() -> Value {
        json!({
            "e": "ORDER_TRADE_UPDATE",
            "T": 200,
            "o": {
                "s": "BTCUSDT", "c": "myorder-1", "x": "TRADE", "X": "FILLED", "S": "SELL",
                "l": "0.5", "L": "64000.0", "n": "0.032", "N": "USDT", "t": 777, "m": true,
                "ps": "BOTH"
            }
        })
    }

    fn bare_fill(evs: &[Event]) -> &FillEvent {
        evs.iter()
            .find_map(|e| match e {
                Event::Fill(f) => Some(f),
                _ => None,
            })
            .expect("a bare Event::Fill")
    }

    /// OFF (the default, and Aster's only path) reproduces TODAY'S behavior exactly: a TRADE_LITE
    /// frame is dropped. Proven against BOTH the flag-off `map_perp_opts` and the 3-arg `map_perp`
    /// wrapper every non-Binance caller (including Aster) uses.
    #[test]
    fn trade_lite_dropped_when_off() {
        let frame = trade_lite_frame();
        assert!(
            map_perp_opts(&frame, "binance", "BTCUSDT", false).is_empty(),
            "flag OFF must drop TRADE_LITE (byte-identical to before this feature)"
        );
        assert!(
            map_perp(&frame, "binance", "BTCUSDT").is_empty(),
            "the 3-arg wrapper (default / Aster path) must drop TRADE_LITE"
        );
    }

    /// ON emits exactly ONE early BARE FillEvent (no OrderFilled/OrderPartiallyFilled wrap) with the
    /// fields taken from the flat TRADE_LITE payload; commission is 0 (TRADE_LITE omits `n`/`N`).
    #[test]
    fn trade_lite_emits_one_early_bare_fill_when_on() {
        let frame = trade_lite_frame();
        let evs = map_perp_opts(&frame, "binance", "BTCUSDT", true);
        assert_eq!(evs.len(), 1, "the early hint is a BARE fill only — no wrap: {evs:?}");
        let Event::Fill(fill) = &evs[0] else { panic!("expected Event::Fill, got {evs:?}") };
        assert_eq!(fill.trade_id.as_str(), "777", "trade_id = `t`");
        assert_eq!(fill.client_order_id, "myorder-1", "coid = `c`");
        assert_eq!(fill.venue.as_str(), "binance");
        assert_eq!(fill.symbol.as_str(), "BTCUSDT", "symbol = top-level `s`");
        assert_eq!(fill.side, -1, "S=SELL → -1");
        assert_eq!(fill.last_qty, 0.5, "last_qty = `l` (LAST filled qty)");
        assert_eq!(fill.last_px, 64000.0, "last_px = `L` (LAST filled price)");
        assert_eq!(fill.commission, 0.0, "TRADE_LITE omits `n` → 0");
        assert!(fill.commission_asset.is_empty(), "TRADE_LITE omits `N` → empty");
        assert_eq!(fill.liquidity_side, LiquiditySide::Maker, "m=true → Maker");
        assert_eq!(fill.ts, 199, "ts = top-level `T`");
        assert_eq!(fill.mark_price, None, "TRADE_LITE carries no mark");
        assert_eq!(fill.position_side, PositionSide::Both, "TRADE_LITE omits `ps` → BOTH");
    }

    /// The other side/liquidity branch: S=BUY → +1, m=false → Taker.
    #[test]
    fn trade_lite_buy_side_and_taker_liquidity() {
        let frame = json!({
            "e": "TRADE_LITE", "T": 5, "s": "ETHUSDT", "m": false, "c": "o2", "S": "BUY",
            "L": "3000.0", "l": "2.0", "t": 12, "i": 3
        });
        let evs = map_perp_opts(&frame, "binance", "ETHUSDT", true);
        let fill = bare_fill(&evs);
        assert_eq!(fill.side, 1, "S=BUY → 1");
        assert_eq!(fill.liquidity_side, LiquiditySide::Taker, "m=false → Taker");
        assert_eq!(fill.symbol.as_str(), "ETHUSDT");
    }

    /// DEDUP CONTRACT (documented + pinned): the early TRADE_LITE fill and its slow authoritative
    /// ORDER_TRADE_UPDATE twin carry the SAME `trade_id`. The execution engine folds a bare
    /// `Event::Fill` into `Account::apply_fill` only the FIRST time it sees a `trade_id`
    /// (`seen_trade_ids`, its reconnect-replay guard), dropping the later duplicate — so the two
    /// collapse into ONE qty booking, never double-counted. This pins the shared key.
    #[test]
    fn early_and_authoritative_fills_share_trade_id_so_they_collapse() {
        let early = map_perp_opts(&trade_lite_frame(), "binance", "BTCUSDT", true);
        let auth = map_perp_opts(&order_trade_update_twin(), "binance", "BTCUSDT", true);
        assert_eq!(
            bare_fill(&early).trade_id,
            bare_fill(&auth).trade_id,
            "early + authoritative fills must share `t` so seen_trade_ids collapses them to ONE"
        );
        assert_eq!(bare_fill(&early).trade_id.as_str(), "777");
        // The authoritative twin ALSO yields the wrap (OrderFilled) that drives the FSM; the early
        // hint deliberately does NOT (it is a bare fill only).
        assert!(
            auth.iter().any(|e| matches!(e, Event::OrderFilled(_))),
            "the authoritative ORDER_TRADE_UPDATE still emits the FSM wrap: {auth:?}"
        );
        assert!(
            early.iter().all(|e| matches!(e, Event::Fill(_))),
            "the early TRADE_LITE hint is a BARE fill only, no wrap: {early:?}"
        );
    }

    /// A liquidation autoclose fill is suppressed even when ON — its position move arrives via the
    /// authoritative ORDER_TRADE_UPDATE → PositionLiquidated path (a SEPARATE dedup set,
    /// `seen_liq_ids`), so a bare early fill would double-fold. Mirrors the authoritative arm's
    /// LIQ_COID_PREFIXES suppression.
    #[test]
    fn trade_lite_liquidation_coid_suppressed_when_on() {
        for prefix in ["autoclose-", "adl_autoclose", "settlement_autoclose-"] {
            let coid = format!("{prefix}123");
            let frame = json!({
                "e": "TRADE_LITE", "T": 9, "s": "BTCUSDT", "m": false,
                "c": coid, "S": "SELL", "L": "50000.0", "l": "1.0", "t": 42
            });
            assert!(
                map_perp_opts(&frame, "binance", "BTCUSDT", true).is_empty(),
                "a `{prefix}` autoclose TRADE_LITE must NOT emit an early bare fill"
            );
        }
    }

    /// The flag is SCOPED to TRADE_LITE: an ORDER_TRADE_UPDATE frame maps IDENTICALLY whether the
    /// hint is on or off (the authoritative path is untouched), and other event types stay dropped.
    #[test]
    fn flag_only_affects_trade_lite_frames() {
        let otu = order_trade_update_twin();
        assert_eq!(
            map_perp_opts(&otu, "binance", "BTCUSDT", true),
            map_perp_opts(&otu, "binance", "BTCUSDT", false),
            "ORDER_TRADE_UPDATE output must be identical regardless of the TRADE_LITE flag"
        );
        // A non-TRADE_LITE / non-OTU / non-ACCOUNT_UPDATE frame stays dropped even with the flag ON.
        let other = json!({ "e": "listenKeyExpired", "T": 1 });
        assert!(map_perp_opts(&other, "binance", "BTCUSDT", true).is_empty());
    }

    /// Unified cross-venue attribution (task 6) round-trip: a venue that echoes back the
    /// broker-prefixed `newClientOrderId`/`c` must decode to the BARE local coid the registry keys
    /// orders by, on ORDER_TRADE_UPDATE's `o.c` — the perp twin of the spot event_mapper's proof.
    #[test]
    fn broker_prefixed_coid_round_trips_to_bare_on_order_trade_update() {
        let prefixed = crate::family::order_map::binance_broker_coid(Some("ABC123"), "deadbeef01");
        assert_eq!(prefixed, "x-ABC123-deadbeef01");

        let frame = json!({
            "e": "ORDER_TRADE_UPDATE", "T": 1,
            "o": { "s": "BTCUSDT", "c": prefixed, "x": "NEW", "X": "NEW", "i": 9 }
        });
        let events = map_perp(&frame, "binance", "BTCUSDT");
        let Event::OrderAccepted(a) = &events[0] else {
            panic!("expected OrderAccepted: {events:?}")
        };
        assert_eq!(
            a.client_order_id, "deadbeef01",
            "must decode the prefixed `o.c` to the bare coid"
        );

        // A bare (unconfigured / Aster) coid passes through untouched.
        let unconfigured = json!({
            "e": "ORDER_TRADE_UPDATE", "T": 1,
            "o": { "s": "BTCUSDT", "c": "deadbeef01", "x": "NEW", "X": "NEW", "i": 9 }
        });
        let events = map_perp(&unconfigured, "binance", "BTCUSDT");
        let Event::OrderAccepted(a) = &events[0] else {
            panic!("expected OrderAccepted: {events:?}")
        };
        assert_eq!(a.client_order_id, "deadbeef01");
    }
}
