//! No-gateway lifecycle for the feed contract: a FakeFeeds mirrors the socket backend's
//! (`DataClientBusSpecBackendIbkrSocket`'s) spawn/stop/join machinery over a scripted data source, proving subscribe→data→unsubscribe(join) works and the
//! sink receives the mapped ticks — the ibapi pumps themselves are covered by the live smoke.
#![cfg(all(feature = "test-support", feature = "ibkr-socket"))]

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use vike_data::{DataClient, DataClientError, DataClientSink, DataClientSubscriptionId};
use vike_marketdata::{Bar, L2Book, TickQuote, TickTrade};

#[derive(Default)]
struct Recording {
    quotes: Mutex<Vec<TickQuote>>,
    trades: Mutex<Vec<TickTrade>>,
}
impl DataClientSink for Recording {
    fn seed_bars(&self, _v: &str, _s: &str, _i: &str, _b: Vec<Bar>) {}
    fn close_bar(&self, _v: &str, _s: &str, _i: &str, _b: Bar) {}
    fn forming_bar(&self, _v: &str, _s: &str, _i: &str, _b: Bar) {}
    fn mark_tick(&self, _v: &str, _s: &str, _p: f64, _t: i64) {}
    fn quote(&self, _v: &str, _s: &str, q: TickQuote) {
        self.quotes.lock().unwrap().push(q);
    }
    fn trade(&self, _v: &str, _s: &str, t: TickTrade) {
        self.trades.lock().unwrap().push(t);
    }
    fn book(&self, _v: &str, _s: &str, _b: Arc<L2Book>) {}
}

struct FakeFeeds {
    sink: Arc<dyn DataClientSink>,
    next: u64,
    subs: HashMap<DataClientSubscriptionId, (Arc<AtomicBool>, JoinHandle<()>)>,
}
impl FakeFeeds {
    fn new(sink: Arc<dyn DataClientSink>) -> Self {
        FakeFeeds { sink, next: 0, subs: HashMap::new() }
    }
}
impl DataClient for FakeFeeds {
    fn subscribe_bars(
        &mut self,
        _s: &str,
        _i: &str,
    ) -> Result<DataClientSubscriptionId, DataClientError> {
        unimplemented!()
    }
    fn subscribe_quotes(
        &mut self,
        symbol: &str,
    ) -> Result<DataClientSubscriptionId, DataClientError> {
        let stop = Arc::new(AtomicBool::new(false));
        let (sink, st, symbol_name) =
            (Arc::clone(&self.sink), Arc::clone(&stop), symbol.to_string());
        let h = std::thread::spawn(move || {
            // emit one quote, then poll stop like the real pump
            sink.quote(
                "ibkr",
                &symbol_name,
                TickQuote {
                    timestamp: 1,
                    local_timestamp: 1,
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
        let id = DataClientSubscriptionId(self.next);
        self.next += 1;
        self.subs.insert(id, (stop, h));
        Ok(id)
    }
    fn subscribe_trades(&mut self, _s: &str) -> Result<DataClientSubscriptionId, DataClientError> {
        unimplemented!()
    }
    fn subscribe_book(&mut self, _s: &str) -> Result<DataClientSubscriptionId, DataClientError> {
        unimplemented!()
    }
    fn unsubscribe(&mut self, id: DataClientSubscriptionId) {
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
    let mut feeds = FakeFeeds::new(rec.clone() as Arc<dyn DataClientSink>);
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

/// **A cpapi box opens no ibapi socket** (decision 0140): the public ibapi data door refuses a
/// `cpapi` config with `Unsupported` BEFORE dialling. The config points at port 1, where nothing
/// listens, so a door that skipped the refusal would answer `Connect` instead and fail this test.
/// The price stream's socket backend is crate-private; its half is
/// `crates/bridges/vike-ibkr/src/feed/socket/mod.rs`'s `a_cpapi_config_opens_no_data_socket`.
#[test]
fn a_cpapi_config_opens_no_data_socket() {
    use vike_ibkr::{HistoricalFetcher, IbkrConfig, IbkrError};
    use vike_venues::IbkrBackend;
    let config = IbkrConfig {
        tier: vike_model::AccountTier::Demo,
        backend: IbkrBackend::Cpapi,
        host: "127.0.0.1".into(),
        port: 1,
        client_id: 1,
        account: "TEST".into(),
        cpapi_url: "https://127.0.0.1:1".into(),
        market_data_type: vike_ibkr::config::MarketDataType::default(),
        data_client_id: 2,
        reconcile_client_id: 3,
        extended_hours: false,
    };
    match HistoricalFetcher::connect(&config) {
        Err(IbkrError::Unsupported(msg)) => assert!(msg.contains("cpapi"), "{msg}"),
        Err(e) => panic!("the historical fetcher refused for the wrong reason: {e}"),
        Ok(_) => panic!("the historical fetcher connected on a cpapi config"),
    }
}
