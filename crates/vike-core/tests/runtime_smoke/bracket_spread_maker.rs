//! Piece 6 bracket / OCO submit-hold, and the SpreadMaker end to end (modify in place, breaker).

use super::*;
use crate::kit::handle::wait_for_snapshot;

// ---- Piece 6: bracket / OCO ----

/// Submits one long bracket (limit entry + SL + TP) on the first quote.
struct BracketOnce {
    done: Arc<AtomicBool>,
}
impl Strategy<LiveBroker> for BracketOnce {
    fn on_quote_tick(&mut self, broker: &mut LiveBroker, _q: &QuoteTick) {
        if !self.done.swap(true, Ordering::Relaxed) {
            broker.submit_bracket(1, 2.0, Some(100.0), 95.0, 110.0);
        }
    }
}

#[test]
fn submit_bracket_sends_only_the_entry_holding_the_exits() {
    // Live-runtime OTO/OCO (submit-hold): a bracket sends ONLY the OTO entry to the venue; the
    // protective stop-loss / take-profit are HELD off the venue until the entry fills, so a naked
    // exit can never trigger before the position exists. (The entry-fill RELEASE and the OCO
    // cancel-sibling drive are pinned synchronously in the runtime `apply` tests — the same
    // `dispatch` fold this spawned thread runs.)
    let done = Arc::<AtomicBool>::default();
    let mut config = test_config(0.0);
    config.strategy = Some(mount_of(
        "binance",
        "BTCUSDT",
        "1m",
        Box::new(BracketOnce { done: Arc::clone(&done) }),
    ));
    let handle = spawn_core(batch_engine(RiskLimits::new()), config);
    let cell = handle.snapshot_cell();
    let ticks = handle.tick_sender();
    ticks
        .quote(QuoteUpdate {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            quote: QuoteTick {
                ts: 1,
                local_ts: 0,
                bid: 100.0,
                ask: 100.2,
                bid_size: 1.0,
                ask_size: 1.0,
                symbol: String::new(),
            },
        })
        .unwrap();
    handle.shutdown_and_join();

    let snap = cell.load();
    assert_eq!(snap.orders.len(), 1, "only the OTO entry is live; the two exits are held");
    let entry = &snap.orders[0];
    assert_eq!(entry.side, 1, "the entry, not an exit");
    assert_eq!(entry.order_type, "limit");
    assert_eq!(entry.price, Some(100.0));
}

// ---- End-to-end: the SpreadMaker market-maker composes tick dispatch + tagged submit + modify ----

#[test]
fn spread_maker_quotes_then_modifies_in_place() {
    let engine = engine_on("binance", "BTCUSDT", ModifiableClient::default());
    let mut config = test_config(1.0);
    config.strategy =
        Some(mount_of("binance", "BTCUSDT", "1m", Box::new(vike_mm::SpreadMaker::new(1.0, 0.5))));
    let handle = spawn_core(engine, config);
    let cell = handle.snapshot_cell();
    let ticks = handle.tick_sender();
    let quote = |ts, bid, ask| QuoteUpdate {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        quote: QuoteTick {
            ts,
            local_ts: 0,
            bid,
            ask,
            bid_size: 1.0,
            ask_size: 1.0,
            symbol: String::new(),
        },
    };
    // quote 1: mid 100.1 → rest bid @ 99.6, ask @ 100.6
    ticks.quote(quote(1, 100.0, 100.2)).unwrap();
    // quote 2: mid 101.1 → MODIFY bid → 100.6, ask → 101.6 (no cancel/re-submit)
    ticks.quote(quote(2, 101.0, 101.2)).unwrap();
    handle.shutdown_and_join();

    let snap = cell.load();
    assert_eq!(snap.orders.len(), 2, "exactly one resting bid + ask (modified, not re-created)");
    let bid = snap.orders.iter().find(|o| o.side == 1).expect("resting bid");
    let ask = snap.orders.iter().find(|o| o.side == -1).expect("resting ask");
    // both re-priced in place to the SECOND quote's mid (101.1) ± half_spread(0.5)
    assert!((bid.price.unwrap() - 100.6).abs() < 1e-6, "bid modified to new mid: {:?}", bid.price);
    assert!((ask.price.unwrap() - 101.6).abs() < 1e-6, "ask modified to new mid: {:?}", ask.price);
    assert_eq!(bid.status, OrderStatus::Accepted, "still resting");
    assert_eq!(ask.status, OrderStatus::Accepted);
}

/// End-to-end: the per-side fill-rate breaker (audit mm1) PULLS the over-hit side through the real
/// runtime. A SpreadMaker rests both quotes, then a run of same-side (bid/buy) fills with no
/// offsetting asks nets past the threshold → the next quote cancels the bid (venue emits
/// OrderCanceled) while the ask keeps resting. Proves `on_fill` → suppression → `cancel_tagged` →
/// `engine.cancel_order` compose over the live path, and that EVENT-time (fill/quote ts) drives it.
#[test]
fn fill_rate_breaker_pulls_over_hit_side_end_to_end() {
    // BatchTestClient accepts every order and emits OrderCanceled on cancel (so a pulled side shows
    // Canceled); the default no-op modify keeps the un-pulled ask resting as Accepted.
    let mut config = test_config(1000.0);
    config.strategy = Some(mount_of(
        "binance",
        "BTCUSDT",
        "1m",
        // window 1000ms, trip at net size 2.5, long cooldown so it stays suppressed to the assert
        Box::new(vike_mm::SpreadMaker::new(1.0, 0.5).with_fill_breaker(1000, 2.5, 100_000)),
    ));
    let handle = spawn_core(batch_engine(RiskLimits::new()), config);
    let cell = handle.snapshot_cell();
    let ticks = handle.tick_sender();
    let events = handle.event_sender();
    let quote = |ts, bid, ask| QuoteUpdate {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        quote: QuoteTick {
            ts,
            local_ts: 0,
            bid,
            ask,
            bid_size: 1.0,
            ask_size: 1.0,
            symbol: String::new(),
        },
    };
    // tick 1: rest a tagged bid + ask (both Accepted by the client)
    ticks.quote(quote(1, 100.0, 101.0)).unwrap();
    // the fills belong to the BID the SpreadMaker minted: a fill only reaches the mount that minted
    // its order (decision 0116), so read that coid off the snapshot once both quotes rest
    wait_for_snapshot(&cell, 10, "the bid and the ask to rest", |s| s.orders.len() == 2);
    let bid = cell.load_full().orders.iter().find(|o| o.side == 1).unwrap().client_order_id.clone();
    // three BID (buy, side +1) fills, distinct trade_ids so none dedup, no offsetting asks →
    // net +3 ≥ 2.5 arms the bid-side cooldown inside on_fill
    events.blocking_send(ext_fill(&bid, "bf1", "BTCUSDT", 1.0, 100.0)).unwrap();
    events.blocking_send(ext_fill(&bid, "bf2", "BTCUSDT", 1.0, 100.0)).unwrap();
    events.blocking_send(ext_fill(&bid, "bf3", "BTCUSDT", 1.0, 100.0)).unwrap();
    // tick 2 (still inside the cooldown): the bid is PULLED, the ask keeps quoting
    ticks.quote(quote(40, 99.0, 100.0)).unwrap();
    handle.shutdown_and_join();

    let snap = cell.load();
    let side = |s: i32| snap.orders.iter().find(|o| o.side == s).expect("an order for the side");
    assert_eq!(snap.orders.len(), 2, "still the one bid + one ask (pulled/kept, not re-created)");
    assert_eq!(
        side(1).status,
        OrderStatus::Canceled,
        "the over-hit bid was pulled via the runtime"
    );
    assert_eq!(side(-1).status, OrderStatus::Accepted, "the un-hit ask still rests");
}
