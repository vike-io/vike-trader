//! **A tick replay and a bar run answer the same with the cache on and off**: one profile's tick run (three days) and
//! one bar run (forty days of minutes, a day missing) over a real store (`vike_store::HistoryStoreDataFusion`), in this
//! process over the store itself, over the daemon's cache missing (its cold days read in pieces at once) and hit, and
//! through a sandboxed child (whose reads are prefetched and answered SHARED by the relay) over a cold and a warm
//! cache, each digests the same answer; and so does each of them with the cache in front of a REAL datahub over the
//! store, whose pages it keeps as they came.

use std::collections::BTreeMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Arc;
use std::thread;

use vike_backtesthub_client::proto::{Request, Response};
use vike_data::HistoryStore;
use vike_marketdata::{Bar, TickQuote, TickTrade};
use vike_model::{MS_PER_DAY, OrderBroker, OrderPlacement, Strategy};
use vike_simulator::OrderBrokerSimulator;
use vike_store::HistoryStoreDataFusion;

use super::{loaded_strategy_files, relay, serve_child};
use crate::backtesthub_server::StudyDirs;
use crate::backtesthub_server::dispatch::answer_here;
use crate::bar_cache::CachedHistoryStore;
use crate::named_run::NamedRunLane;
use crate::studio_run::StrategyFileBuild;

/// The build both ends of the pipe state here (a binary hands in `vike_buildinfo::summary`).
const BUILD: &str = "tick_run_tests";

/// A UTC midnight, where the run's window opens.
const S: i64 = 20_000 * MS_PER_DAY;

const TEXT: &str = "//! Tick follower: long under the last mid, short over it.\n//!\n//! ```toml\n\
                    //! [strategy]\n//! runs_on = \"backtest\"\n//! ```\n";

/// Every 40th trade turns long while the trade prints under the last quote's mid, else short; on bars, every 40th bar
/// turns long while it closed under its open.
struct TickFollower {
    seen: usize,
    mid: Option<f64>,
    long: Option<bool>,
}

impl TickFollower {
    fn turn(&mut self, broker: &mut OrderBrokerSimulator, long: bool) {
        self.seen += 1;
        if self.seen.is_multiple_of(40) && self.long != Some(long) {
            let qty = if self.long.is_some() { 2.0 } else { 1.0 };
            let side = if long { 1 } else { -1 };
            broker.place("BTCUSDT", "", side, qty, OrderPlacement::Market, false);
            self.long = Some(long);
        }
    }
}

impl Strategy<OrderBrokerSimulator> for TickFollower {
    fn on_bar(&mut self, broker: &mut OrderBrokerSimulator, bar: &Bar) {
        self.turn(broker, bar.close < bar.open);
    }
    fn on_quote_tick(&mut self, _broker: &mut OrderBrokerSimulator, quote: &TickQuote) {
        self.mid = Some(quote.mid());
    }
    fn on_trade_tick(&mut self, broker: &mut OrderBrokerSimulator, trade: &TickTrade) {
        let Some(mid) = self.mid else {
            self.seen += 1;
            return;
        };
        self.turn(broker, trade.price < mid);
    }
}

fn the_file() -> StrategyFileBuild {
    Box::new(|_params_toml: &str, _facts: &str| {
        Ok(Box::new(TickFollower { seen: 0, mid: None, long: None })
            as Box<dyn Strategy<OrderBrokerSimulator>>)
    })
}

/// Three days of binance BTCUSDT ticks from [`S`].
fn profile() -> String {
    format!(
        "[data]\nvenue = \"binance\"\nsymbols = [\"BTCUSDT\"]\nkind = \"tick\"\n\
         from = \"{S}\"\nto = \"{}\"\n\n[engine]\ncash = 100000.0\nattach_funding = false\n\n\
         [strategy]\nfile = \"other/tick_follower.rs\"\n",
        S + 3 * MS_PER_DAY - 1
    )
}

/// The bar run's days: forty days of binance BTCUSDT minutes from [`S`], cold days read in several pieces.
const BAR_DAYS: i64 = 40;

/// [`BAR_DAYS`] of binance BTCUSDT `1m` bars from [`S`].
fn bar_profile() -> String {
    format!(
        "[data]\nvenue = \"binance\"\nsymbols = [\"BTCUSDT\"]\nkind = \"bar\"\ninterval = \"1m\"\n\
         from = \"{S}\"\nto = \"{}\"\n\n[engine]\ncash = 100000.0\nattach_funding = false\n\n\
         [strategy]\nfile = \"other/tick_follower.rs\"\n",
        S + BAR_DAYS * MS_PER_DAY - 1
    )
}

/// The bar run's minutes, but day 17's (a day no stamp lists, inside the cold run, so one piece reads over it): the
/// closes walk a fixed pattern.
fn planted_bars() -> (tempfile::TempDir, Arc<HistoryStoreDataFusion>) {
    let dir = tempfile::tempdir().expect("a temp dir");
    let store = HistoryStoreDataFusion::open(dir.path()).expect("the store");
    let bars: Vec<Bar> = (0..BAR_DAYS * 1_440)
        .filter(|i| i / 1_440 != 17)
        .map(|i| {
            let close = 60_000.0 + ((i * 37) % 101) as f64 - ((i * 11) % 53) as f64;
            let open = close + ((i * 13) % 7) as f64 - 3.0;
            Bar::ohlcv(S + i * 60_000, open, close + 4.0, close - 4.0, close, 1.0 + (i % 7) as f64)
        })
        .collect();
    store.append_bars("binance", "BTCUSDT", "1m", &bars, None).expect("bars");
    (dir, Arc::new(store))
}

/// The run's ticks: quotes and trades every few seconds over four days (one past the window), each day written as two
/// parts whose timestamps meet, and a `group=` part on day 1 at the same timestamps.
fn planted() -> (tempfile::TempDir, Arc<HistoryStoreDataFusion>) {
    let dir = tempfile::tempdir().expect("a temp dir");
    let store = HistoryStoreDataFusion::open(dir.path()).expect("the store");
    let quote = |timestamp: i64, i: i64| {
        let bid = 60_000.0 + ((i * 37) % 101) as f64 - ((i * 11) % 53) as f64;
        TickQuote {
            timestamp,
            local_timestamp: timestamp + 3,
            bid,
            ask: bid + 1.0,
            bid_size: 2.0,
            ask_size: 3.0,
            symbol: "BTCUSDT".into(),
        }
    };
    let trade = |timestamp: i64, i: i64| TickTrade {
        timestamp,
        local_timestamp: timestamp + 5,
        price: 60_000.0 + ((i * 53) % 97) as f64 - ((i * 7) % 41) as f64,
        size: 0.01,
        is_buyer_maker: i % 2 == 0,
        symbol: "BTCUSDT".into(),
    };
    for day in 0..4 {
        for half in 0..2_i64 {
            let ts = |i: i64| S + day * MS_PER_DAY + (i + half * 2_000) / 2 * 9_000;
            let quotes: Vec<_> = (0..2_000).map(|i| quote(ts(i), i + day + half)).collect();
            let trades: Vec<_> = (0..2_000).map(|i| trade(ts(i) + 1, i * 3 + day)).collect();
            let key = format!("{day}:{half}");
            store.append_quotes("binance", "BTCUSDT", &quotes, Some(&key)).expect("quotes");
            store.append_trades("binance", "BTCUSDT", &trades, Some(&key)).expect("trades");
        }
    }
    let grouped: Vec<_> = (0..300).map(|i| trade(S + MS_PER_DAY + i * 9_000 + 1, i)).collect();
    store.append_trades_grouped("binance", "majors", &grouped, Some("group")).expect("grouped");
    (dir, Arc::new(store))
}

/// The daemon's cache over `store`, today weeks past both windows.
fn cached(store: &Arc<dyn HistoryStore + Send + Sync>) -> Arc<CachedHistoryStore> {
    let clock = Box::new(|| S + 60 * MS_PER_DAY);
    Arc::new(CachedHistoryStore::new(store.clone(), 64, 64, clock))
}

/// `store` behind a REAL datahub on loopback (`vike_datahub::serve`), read as the daemon reads it
/// (`HistoryStoreDatahub`): a cache over it keeps the datahub's OWN pages (`HistoryStore::read_columns`).
fn behind_a_datahub(store: &Arc<HistoryStoreDataFusion>) -> Arc<dyn HistoryStore + Send + Sync> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback port");
    let addr = listener.local_addr().expect("the bound address").to_string();
    let served: Arc<dyn HistoryStore + Send + Sync> = store.clone();
    thread::spawn(move || {
        let _ = vike_datahub::serve(listener, served);
    });
    Arc::new(vike_datahub_client::HistoryStoreDatahub::new(addr))
}

/// An answer's text, hashed: the trades, the report and every number in it.
fn digest(response: &Response) -> u64 {
    let mut digest = DefaultHasher::new();
    format!("{response:?}").hash(&mut digest);
    digest.finish()
}

/// The run of `profile` answered in THIS process over `store`.
fn here(store: Arc<dyn HistoryStore + Send + Sync>, profile: &str) -> Response {
    let request = Request::RunBacktest(profile.to_string());
    let strategies = loaded_strategy_files(
        &request,
        vec![(Ok(the_file()), None)],
        vec![(TEXT.into(), BTreeMap::new())],
    );
    let study = StudyDirs { strategies, ..StudyDirs::working_directory() };
    answer_here(request, &store, &study, NamedRunLane::DISARMED, None).0
}

/// The run of `profile` answered by a child over two OS pipes, its reads answered from `store` by this side's relay.
fn through_a_child(store: &(dyn HistoryStore + Send + Sync), profile: &str) -> Response {
    let (mut from_child, child_out) = std::io::pipe().expect("pipe up");
    let (child_in, mut to_child) = std::io::pipe().expect("pipe down");
    let child = thread::spawn(move || {
        let built = vec![(Ok(the_file()), None)];
        let _ = serve_child(Box::new(child_in), Box::new(child_out), BUILD, built);
    });
    let request = Request::RunBacktest(profile.to_string());
    let relayed = relay::relay(
        &mut to_child,
        &mut from_child,
        BUILD,
        &request,
        TEXT,
        &BTreeMap::new(),
        store,
        &|| {},
        None,
    );
    drop(to_child);
    child.join().expect("the child's half");
    relayed.unwrap_or_else(|stopped| panic!("the relay stopped: {}", stopped.why)).response
}

/// **The same digest, cache on or off**: the run over the store itself is the reference; over the cache it misses,
/// then hits (the ticks were kept), and through a child over a cold cache and over the warm one it answers the same.
#[test]
fn a_tick_run_digests_the_same_with_the_cache_on_and_off() {
    let (_dir, store) = planted();
    digests_the_same(&store, &profile(), CachedHistoryStore::kept_tick_bytes);
}

/// **The same digest for a BAR run, cache on or off**: forty cold days of minutes, read in pieces at once on the miss
/// and by the cold child, answer as the store itself does, and so do the hits.
#[test]
fn a_bar_run_digests_the_same_with_the_cache_on_and_off() {
    let (_dir, store) = planted_bars();
    digests_the_same(&store, &bar_profile(), CachedHistoryStore::kept_bytes);
}

/// The run of `profile` over `store` itself is the reference. Over the cache in front of the store (which hands it
/// rows, encoded once) and in front of a real datahub over it (which hands it the datahub's pages, kept as they came)
/// it misses (and `kept` says its days were kept), then hits, and through a child over a cold cache and over the warm
/// one it answers the same; so does the datahub with no cache, here and through a child (whose relay hands the pages
/// on as they came).
fn digests_the_same(
    store: &Arc<HistoryStoreDataFusion>,
    profile: &str,
    kept: fn(&CachedHistoryStore) -> usize,
) {
    let reference = here(store.clone(), profile);
    assert!(matches!(&reference, Response::Report(_)), "the reference run must run: {reference:?}");
    let datahub = behind_a_datahub(store);
    let mut answers = vec![
        ("datahub, no cache".to_string(), here(datahub.clone(), profile)),
        ("datahub, no cache, child".to_string(), through_a_child(datahub.as_ref(), profile)),
    ];
    let direct: Arc<dyn HistoryStore + Send + Sync> = store.clone();
    for (inner, over) in [(direct, "the store"), (datahub, "the datahub")] {
        let warm = cached(&inner);
        answers.push((format!("{over}: miss"), here(warm.clone(), profile)));
        assert!(kept(&warm) > 0, "{over}: the run's days are kept");
        answers.push((format!("{over}: hit"), here(warm.clone(), profile)));
        let cold_child = through_a_child(cached(&inner).as_ref(), profile);
        answers.push((format!("{over}: child, cold"), cold_child));
        answers.push((format!("{over}: child, warm"), through_a_child(warm.as_ref(), profile)));
    }
    for (what, answer) in answers {
        assert_eq!(digest(&answer), digest(&reference), "{what}: the same answer: {answer:?}");
    }
}
