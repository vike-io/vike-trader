use super::*;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use vike_model::BookUpdateKind;

// The shared capturing sink (testing-arch Phase 4d) — the canonical copy of the formatted-
// string RecordingSink that used to live inline here; `calls()` renders the same one-line
// forms (tick verbs now carry the full union field set — sizes, maker flag, book_update's
// local_ts/tick fields).
use crate::test_support::RecordingSink;

fn fake_bar(close: f64) -> Bar {
    Bar {
        ts: 1,
        open: close,
        high: close,
        low: close,
        close,
        volume: 1.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}

/// A fake venue client: `subscribe_bars` pushes one `seed_bars` + one `close_bar` through the
/// sink given at construction and returns a fresh id; quotes/trades are unsupported;
/// unsubscribe/shutdown just record that they were called. Proves the seam's wiring end to
/// end without any real venue or network.
struct FakeClient {
    sink: Arc<dyn LiveDataSink>,
    next_id: AtomicU64,
    unsubscribed: Mutex<Vec<SubscriptionId>>,
    shutdown_called: Mutex<bool>,
}

impl FakeClient {
    fn new(sink: Arc<dyn LiveDataSink>) -> Self {
        Self {
            sink,
            next_id: AtomicU64::new(0),
            unsubscribed: Mutex::new(Vec::new()),
            shutdown_called: Mutex::new(false),
        }
    }
}

impl DataClient for FakeClient {
    fn subscribe_bars(
        &mut self,
        symbol: &str,
        interval: &str,
    ) -> Result<SubscriptionId, LiveDataError> {
        self.sink.seed_bars("fake", symbol, interval, vec![fake_bar(10.0)]);
        self.sink.close_bar("fake", symbol, interval, fake_bar(11.0));
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        Ok(SubscriptionId(id))
    }
    fn subscribe_quotes(&mut self, _symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        Err(LiveDataError::Unsupported("fake client serves bars only"))
    }
    fn subscribe_trades(&mut self, _symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        Err(LiveDataError::Unsupported("fake client serves bars only"))
    }
    fn subscribe_book(&mut self, _symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        Err(LiveDataError::Unsupported("fake client serves bars only"))
    }
    fn unsubscribe(&mut self, id: SubscriptionId) {
        self.unsubscribed.lock().unwrap().push(id);
    }
    fn shutdown(&mut self) {
        *self.shutdown_called.lock().unwrap() = true;
    }
}

#[test]
fn subscribe_bars_streams_seed_and_close_through_the_sink_and_returns_fresh_ids() {
    let sink = Arc::new(RecordingSink::default());
    let mut client = FakeClient::new(sink.clone());

    let id1 = client.subscribe_bars("BTCUSDT", "1m").expect("subscribe ok");
    let id2 = client.subscribe_bars("ETHUSDT", "1m").expect("subscribe ok");
    assert_ne!(id1, id2, "distinct ids per subscribe");

    assert_eq!(
        sink.calls(),
        vec![
            "seed_bars(fake,BTCUSDT,1m,1)".to_string(),
            "close_bar(fake,BTCUSDT,1m,11)".to_string(),
            "seed_bars(fake,ETHUSDT,1m,1)".to_string(),
            "close_bar(fake,ETHUSDT,1m,11)".to_string(),
        ]
    );
}

#[test]
fn quotes_and_trades_are_unsupported_on_the_fake_bars_only_client() {
    let sink = Arc::new(RecordingSink::default());
    let mut client = FakeClient::new(sink);

    match client.subscribe_quotes("BTCUSDT") {
        Err(LiveDataError::Unsupported(_)) => {}
        other => panic!("expected Unsupported, got {other:?}"),
    }
    match client.subscribe_trades("BTCUSDT") {
        Err(LiveDataError::Unsupported(_)) => {}
        other => panic!("expected Unsupported, got {other:?}"),
    }
}

#[test]
fn unsubscribe_and_shutdown_are_recorded() {
    let sink = Arc::new(RecordingSink::default());
    let mut client = FakeClient::new(sink);

    let id = client.subscribe_bars("BTCUSDT", "1m").expect("subscribe ok");
    client.unsubscribe(id);
    client.shutdown();

    assert_eq!(*client.unsubscribed.lock().unwrap(), vec![id]);
    assert!(*client.shutdown_called.lock().unwrap());
}

#[test]
fn fake_client_book_unsupported() {
    let sink = Arc::new(RecordingSink::default());
    let mut client = FakeClient::new(sink);

    match client.subscribe_book("BTCUSDT") {
        Err(LiveDataError::Unsupported(_)) => {}
        other => panic!("expected Unsupported, got {other:?}"),
    }
}

#[test]
fn tee_fans_out_in_order() {
    let sink1 = Arc::new(RecordingSink::default());
    let sink2 = Arc::new(RecordingSink::default());
    let tee = TeeSink(vec![sink1.clone(), sink2.clone()]);

    tee.seed_bars("fake", "BTCUSDT", "1m", vec![fake_bar(10.0)]);
    tee.quote(
        "fake",
        "BTCUSDT",
        QuoteTick {
            ts: 1,
            local_ts: 0,
            bid: 100.0,
            ask: 101.0,
            bid_size: 1.0,
            ask_size: 2.0,
            symbol: String::new(),
        },
    );
    tee.trade(
        "fake",
        "BTCUSDT",
        TradeTick {
            ts: 2,
            local_ts: 0,
            price: 100.5,
            size: 0.5,
            is_buyer_maker: true,
            symbol: String::new(),
        },
    );
    let mut book = L2Book::new(0.5);
    book.apply_snapshot(1, &[BookLevel::new(100.0, 1.0)], &[BookLevel::new(101.0, 1.0)]);
    tee.book("fake", "BTCUSDT", Arc::new(book.clone()));
    // l2_snapshot MUST be fanned out too — it has a default no-op in the trait, so a missing
    // TeeSink override silently drops every depth snapshot (the DOM book store goes STALE). This
    // assertion is the regression guard for exactly that bug.
    tee.l2_snapshot(
        "fake",
        "BTCUSDT",
        0.5,
        vec![BookLevel::new(100.0, 2.0)],
        vec![BookLevel::new(101.0, 3.0)],
        7,
    );
    // bar_close_tick likewise defaults to a no-op in the trait — a missing TeeSink override
    // would silently drop every candle-close tick (the core's bar-close slot goes stale).
    tee.bar_close_tick("fake", "BTCUSDT", 100.25, 6);
    tee.book_update(
        "fake",
        "BTCUSDT",
        BookUpdate {
            ts: 8,
            local_ts: 9,
            seq: 3,
            kind: BookUpdateKind::Delta,
            tick_size: 0.5,
            bids: vec![BookLevel::new(100.0, 2.0)],
            asks: vec![],
            symbol: String::new(),
        },
    );

    let expected = vec![
        "seed_bars(fake,BTCUSDT,1m,1)".to_string(),
        "quote:fake:BTCUSDT:100/101:1x2".to_string(),
        "trade:fake:BTCUSDT:100.5/0.5:maker=true".to_string(),
        format!("book:fake:BTCUSDT:{:?}", book.mid()),
        "l2_snapshot(fake,BTCUSDT,0.5,1b/1a,7)".to_string(),
        "bar_close_tick(fake,BTCUSDT,100.25,6)".to_string(),
        "book_update:fake:BTCUSDT:Delta:seq=3:bids=1:asks=0:local_ts_pos=true:tick=0.5".to_string(),
    ];
    assert_eq!(sink1.calls(), expected, "sink1 records every verb in order");
    assert_eq!(sink2.calls(), expected, "sink2 records every verb in order");
}

#[test]
fn stream_status_disclosed_in_gap_order() {
    // The §B disclosure sequence a consumer sees across one outage: seed → close → GapStart →
    // (reconnect + re-seed) → Live. Driven directly here; the feed watchdog produces it live.
    let sink = RecordingSink::default();
    sink.seed_bars("binance", "BTCUSDT", "1m", vec![fake_bar(10.0)]);
    sink.close_bar("binance", "BTCUSDT", "1m", fake_bar(11.0));
    sink.stream_status("binance", "BTCUSDT", "1m", StreamStatus::GapStart { at_ts_ms: 500 });
    sink.seed_bars("binance", "BTCUSDT", "1m", vec![fake_bar(12.0)]);
    sink.stream_status(
        "binance",
        "BTCUSDT",
        "1m",
        StreamStatus::Live { gap_started_ts_ms: Some(500) },
    );
    assert_eq!(
        sink.calls(),
        vec![
            "seed_bars(binance,BTCUSDT,1m,1)".to_string(),
            "close_bar(binance,BTCUSDT,1m,11)".to_string(),
            "stream_status:binance:BTCUSDT:1m:GapStart { at_ts_ms: 500 }".to_string(),
            "seed_bars(binance,BTCUSDT,1m,1)".to_string(),
            "stream_status:binance:BTCUSDT:1m:Live { gap_started_ts_ms: Some(500) }".to_string(),
        ]
    );
}

/// Seam-growth guard: a sink that implements only the REQUIRED verbs (this file's oldest
/// shape) still compiles and inherits `bar_close_tick` as a no-op — the additive-trait-method
/// contract that keeps every existing sink source-compatible.
#[test]
fn bar_close_tick_defaults_to_a_noop_on_a_minimal_sink() {
    struct MinimalSink;
    impl LiveDataSink for MinimalSink {
        fn seed_bars(&self, _v: &str, _s: &str, _i: &str, _b: Vec<Bar>) {}
        fn close_bar(&self, _v: &str, _s: &str, _i: &str, _b: Bar) {}
        fn forming_bar(&self, _v: &str, _s: &str, _i: &str, _b: Bar) {}
        fn mark_tick(&self, _v: &str, _s: &str, _p: f64, _t: i64) {}
        fn quote(&self, _v: &str, _s: &str, _q: QuoteTick) {}
        fn trade(&self, _v: &str, _s: &str, _t: TradeTick) {}
        fn book(&self, _v: &str, _s: &str, _b: Arc<L2Book>) {}
    }
    // Inherited default: callable, side-effect free, never panics.
    MinimalSink.bar_close_tick("binance", "BTCUSDT.P", 100.0, 1);
}

#[test]
fn tee_fans_stream_status() {
    let s1 = Arc::new(RecordingSink::default());
    let s2 = Arc::new(RecordingSink::default());
    let tee = TeeSink(vec![s1.clone(), s2.clone()]);
    tee.stream_status("okx", "BTC-USDT-SWAP", "quotes", StreamStatus::GapStart { at_ts_ms: 7 });
    let want = vec!["stream_status:okx:BTC-USDT-SWAP:quotes:GapStart { at_ts_ms: 7 }".to_string()];
    assert_eq!(s1.calls(), want);
    assert_eq!(s2.calls(), want, "TeeSink fans status to every inner sink");
}

/// `require_live_verb` mirrors the declared `VenueCaps.live_data` matrix EXACTLY, for every
/// roster venue × verb — the drift-proof the hand-rolled feed refusals lack. Unknown venue
/// ids refuse every verb (fail-closed `UNSUPPORTED` row).
#[test]
fn require_live_verb_is_driven_by_the_declared_matrix() {
    use vike_model::LiveVerb;
    const VERBS: [LiveVerb; 5] =
        [LiveVerb::Bars, LiveVerb::Quotes, LiveVerb::Trades, LiveVerb::Book, LiveVerb::Depth];
    for &v in vike_model::VENUES {
        let declared = vike_model::caps_for(v).live_data;
        for verb in VERBS {
            let got = super::require_live_verb(v, verb);
            assert_eq!(
                got.is_ok(),
                declared.supports(verb),
                "{v}/{verb:?}: helper must mirror the declared row"
            );
            if let Err(e) = got {
                assert!(matches!(e, LiveDataError::Unsupported(_)), "{v}/{verb:?}: {e}");
            }
        }
    }
    for verb in VERBS {
        assert!(super::require_live_verb("no-such-venue", verb).is_err(), "fail-closed");
    }
    // spot-checks against the known matrix: binance serves depth but not quotes;
    // polymarket the inverse shape (book, no depth)
    assert!(super::require_live_verb("binance", LiveVerb::Depth).is_ok());
    assert!(super::require_live_verb("binance", LiveVerb::Quotes).is_err());
    assert!(super::require_live_verb("polymarket", LiveVerb::Book).is_ok());
    assert!(super::require_live_verb("polymarket", LiveVerb::Depth).is_err());
}

#[test]
fn fixed_symbols_are_one_set_at_every_instant_and_name_no_group() {
    let mut f = FixedSymbols::new(["B", "A", "A"]);
    assert_eq!(f.group(), None, "an explicit list records per-symbol, never grouped");
    let early = f.desired(0).unwrap();
    let late = f.desired(1_800_000_000_000).unwrap();
    assert_eq!(early, late, "the clock changes nothing for an explicit list");
    assert_eq!(early.into_iter().collect::<Vec<_>>(), vec!["A".to_string(), "B".to_string()]);
}

#[test]
fn a_boxed_resolver_is_send() {
    fn assert_send<T: Send>() {}
    assert_send::<Box<dyn SymbolResolver>>();
}
