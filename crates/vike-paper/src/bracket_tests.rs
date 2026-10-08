use super::*;
use vike_model::{BracketSpec, build_bracket};

fn bar(ts: i64, o: f64, h: f64, l: f64, c: f64) -> Bar {
    Bar {
        ts,
        open: o,
        high: h,
        low: l,
        close: c,
        volume: 0.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}

fn filled(evs: &[Event]) -> Vec<String> {
    evs.iter()
        .filter_map(|e| match e {
            Event::OrderFilled(f) => Some(f.client_order_id.clone()),
            _ => None,
        })
        .collect()
}
fn canceled(evs: &[Event]) -> Vec<String> {
    evs.iter()
        .filter_map(|e| match e {
            Event::OrderCanceled(x) => Some(x.client_order_id.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn bracket_oto_arms_entry_then_oco_cancels_sibling() {
    let mut c = PaperExecutionClient::new("sim", "BTCUSDT", 0.0, 0.0, 0.0);
    // long bracket: market entry, SL 90, TP 110
    let spec = BracketSpec {
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 1.0,
        entry_price: None,
        stop_loss: 90.0,
        take_profit: 110.0,
    };
    for leg in build_bracket(&spec, "entry", "sl", "tp") {
        c.submit(&leg);
    }
    // bar 1: entry (market) fills at open; the exits are HELD (armed by this fill, not filled).
    c.on_bar(&bar(1, 100.0, 105.0, 95.0, 100.0));
    // bar 2: rally to 112 → TP (limit sell 110) fills; SL must be OCO-canceled.
    c.on_bar(&bar(2, 105.0, 112.0, 104.0, 110.0));

    let evs: Vec<Event> = std::iter::from_fn(|| c.poll_events()).collect();
    let f = filled(&evs);
    assert!(f.contains(&"entry".to_string()), "entry fills first");
    assert!(f.contains(&"tp".to_string()), "TP fills on the rally");
    assert!(!f.contains(&"sl".to_string()), "SL never fills");
    assert!(canceled(&evs).contains(&"sl".to_string()), "SL is OCO-canceled when TP fills");
}

#[test]
fn held_exit_does_not_fill_before_entry() {
    // a limit entry that never fills → the exits stay held even where price would trigger them.
    let mut c = PaperExecutionClient::new("sim", "BTCUSDT", 0.0, 0.0, 0.0);
    let spec = BracketSpec {
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 1.0,
        entry_price: Some(50.0), // far below the bar → entry never fills
        stop_loss: 90.0,
        take_profit: 110.0,
    };
    for leg in build_bracket(&spec, "e2", "sl2", "tp2") {
        c.submit(&leg);
    }
    // range 95..112 would trigger TP(110) if active, but the entry (limit 50) never fills.
    c.on_bar(&bar(1, 100.0, 112.0, 95.0, 108.0));
    let evs: Vec<Event> = std::iter::from_fn(|| c.poll_events()).collect();
    assert!(filled(&evs).is_empty(), "no leg fills while the entry is unfilled");
}
