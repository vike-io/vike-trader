//! No-gateway lifecycle for the feed contract: a FakeFeeds mirrors IbkrFeeds' spawn/stop/join
//! machinery over a scripted data source, proving subscribe→data→unsubscribe(join) works and the
//! sink receives the mapped ticks — the ibapi pumps themselves are covered by the live smoke.
#![cfg(all(feature = "test-support", feature = "ibkr-socket"))]

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use vike_data::{DataClient, LiveDataError, LiveDataSink, SubscriptionId};
use vike_model::{Bar, L2Book, QuoteTick, TradeTick};

#[derive(Default)]
struct Recording {
    quotes: Mutex<Vec<QuoteTick>>,
    trades: Mutex<Vec<TradeTick>>,
}
impl LiveDataSink for Recording {
    fn seed_bars(&self, _v: &str, _s: &str, _i: &str, _b: Vec<Bar>) {}
    fn close_bar(&self, _v: &str, _s: &str, _i: &str, _b: Bar) {}
    fn forming_bar(&self, _v: &str, _s: &str, _i: &str, _b: Bar) {}
    fn mark_tick(&self, _v: &str, _s: &str, _p: f64, _t: i64) {}
    fn quote(&self, _v: &str, _s: &str, q: QuoteTick) {
        self.quotes.lock().unwrap().push(q);
    }
    fn trade(&self, _v: &str, _s: &str, t: TradeTick) {
        self.trades.lock().unwrap().push(t);
    }
    fn book(&self, _v: &str, _s: &str, _b: Arc<L2Book>) {}
}

struct FakeFeeds {
    sink: Arc<dyn LiveDataSink>,
    next: u64,
    subs: HashMap<SubscriptionId, (Arc<AtomicBool>, JoinHandle<()>)>,
}
impl FakeFeeds {
    fn new(sink: Arc<dyn LiveDataSink>) -> Self {
        FakeFeeds { sink, next: 0, subs: HashMap::new() }
    }
}
impl DataClient for FakeFeeds {
    fn subscribe_bars(&mut self, _s: &str, _i: &str) -> Result<SubscriptionId, LiveDataError> {
        unimplemented!()
    }
    fn subscribe_quotes(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        let stop = Arc::new(AtomicBool::new(false));
        let (sink, st, sym) = (Arc::clone(&self.sink), Arc::clone(&stop), symbol.to_string());
        let h = std::thread::spawn(move || {
            // emit one quote, then poll stop like the real pump
            sink.quote(
                "ibkr",
                &sym,
                QuoteTick {
                    ts: 1,
                    local_ts: 1,
                    bid: 190.0,
                    ask: 190.5,
                    bid_size: 1.0,
                    ask_size: 1.0,
                    symbol: String::new(),
                },
            );
            while !st.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(20));
            }
        });
        let id = SubscriptionId(self.next);
        self.next += 1;
        self.subs.insert(id, (stop, h));
        Ok(id)
    }
    fn subscribe_trades(&mut self, _s: &str) -> Result<SubscriptionId, LiveDataError> {
        unimplemented!()
    }
    fn subscribe_book(&mut self, _s: &str) -> Result<SubscriptionId, LiveDataError> {
        unimplemented!()
    }
    fn unsubscribe(&mut self, id: SubscriptionId) {
        if let Some((stop, h)) = self.subs.remove(&id) {
            stop.store(true, Ordering::Relaxed);
            let _ = h.join();
        }
    }
    fn shutdown(&mut self) {
        for (s, _) in self.subs.values() {
            s.store(true, Ordering::Relaxed);
        }
        for (_, (_, h)) in self.subs.drain() {
            let _ = h.join();
        }
    }
}

#[test]
fn subscribe_delivers_then_unsubscribe_joins() {
    let rec = Arc::new(Recording::default());
    let mut feeds = FakeFeeds::new(rec.clone() as Arc<dyn LiveDataSink>);
    let id = feeds.subscribe_quotes("AAPL.SMART.USD").unwrap();
    // wait for the scripted quote
    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    while rec.quotes.lock().unwrap().is_empty() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(rec.quotes.lock().unwrap().len(), 1);
    assert_eq!(rec.quotes.lock().unwrap()[0].bid, 190.0);
    feeds.unsubscribe(id); // must join cleanly (no hang)
    feeds.shutdown();
}
