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
    let Event::OrderAccepted(a) = &events[0] else { panic!("expected OrderAccepted: {events:?}") };
    assert_eq!(a.client_order_id, "deadbeef01", "must decode the prefixed `o.c` to the bare coid");

    // A bare (unconfigured / Aster) coid passes through untouched.
    let unconfigured = json!({
        "e": "ORDER_TRADE_UPDATE", "T": 1,
        "o": { "s": "BTCUSDT", "c": "deadbeef01", "x": "NEW", "X": "NEW", "i": 9 }
    });
    let events = map_perp(&unconfigured, "binance", "BTCUSDT");
    let Event::OrderAccepted(a) = &events[0] else { panic!("expected OrderAccepted: {events:?}") };
    assert_eq!(a.client_order_id, "deadbeef01");
}
