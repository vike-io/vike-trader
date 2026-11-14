//! LIVE Polymarket market-data smokes (network, KEYLESS — public CLOB market channel):
//!
//! ```sh
//! cargo test -p vike-polymarket --features polymarket --test polymarket_smokes -- --ignored --nocapture market_ticks_live_smoke::
//! ```
//! The tunnel is `venue.polymarket.*` in the settings database (see `tests/polymarket_smokes/egress_support.rs`).
//!
//! Both tests are `#[ignore = "network"]` and deliberately carry NO credential gate — unlike every
//! other live smoke in this workspace (which self-skips absent `.env` creds, per
//! `vike_secrets::credentials`'s absent-credentials-is-the-live-gate rule), the Polymarket
//! market channel is public/unauthenticated data: there is no credential to gate on, and running
//! it costs the venue nothing (a read-only WS subscription). Run manually, not in CI.
//!
//! - `market_websocket_delivers_book_and_quote`: proves `DataClient::subscribe_book` end to end
//!   against the real WS — resolves a live, liquid token via `fetch_all_markets`'s first page, then
//!   waits for at least one book AND one derived quote to land in a `RecordingSink`.
//! - `ticks_reach_a_mounted_strategy`: the `okx_market_data_smoke` shape carried THROUGH the
//!   `vike_data::DataClient` seam — spawns a real core + `Mount`, wires an inline sink that
//!   forwards straight to `handle.tick_sender()` (a deliberately minimal 5-verb stand-in for the
//!   production forwarder, `vike_tradehub_core::CoreLaneSink`, which the daemon's feeds use; the GUI has no
//!   core and forwards nothing), and asserts the mounted strategy's
//!   `on_quote_tick`/`on_order_book` counters move.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use vike_marketdata::L2BookLevel;

use vike_bridge_core::data_client::bus::DataClientBus;
use vike_bridge_core::transport::UreqTransport;
use vike_data::{DataClient, DataClientSink};
use vike_marketdata::{Bar, L2Book, TickQuote, TickTrade};
use vike_polymarket::{DataClientBusSpecPolymarket, fetch_all_markets};

// The shared capturing sink (testing-arch Phase 4d): delivery counts come off its typed
// accessors; the crossed-market invariants the old inline sink asserted per callback are
// checked over the recorded ticks after the wait loop (a feed-thread panic can't fail a test
// anyway; the post-loop assertion can).
use vike_data::RecordingSink;

use crate::egress_support;

/// Resolve one live, liquid outcome token: the first `/markets` page's first market with at least
/// one token. Polymarket's `/markets` response has no volume field to sort by (that lives on the
/// Gamma API, a different host this crate doesn't touch), so "first page, first market" is the
/// pragmatic liquid-enough pick — active markets sort toward the front in practice.
fn pick_a_live_token() -> String {
    use vike_venues::venue_urls::{UrlChannel, UrlProtocol};
    let transport = UreqTransport::new("polymarket");
    let clob = egress_support::seed_row(UrlChannel::Trade, UrlProtocol::Rest, "");
    let markets = fetch_all_markets(&transport, &clob, 1).expect("fetch_all_markets page 1");
    markets
        .iter()
        .find_map(|m| m.tokens.first().map(|t| t.token_id.clone()))
        .expect("at least one market with a token on the first /markets page")
}

#[test]
#[ignore = "network"]
fn market_websocket_delivers_book_and_quote() {
    vike_log::test_init();
    egress_support::declare_from_the_settings_database();
    // §0.2 egress guard: a read-only smoke declares no region (decision 0095 made the region a
    // parameter), so this makes no network call — the order-placing smokes are the ones that
    // assert `vike_polymarket::DUBLIN_EGRESS_COUNTRY`.
    match vike_polymarket::check_expected_egress(None, vike_polymarket::DEFAULT_EGRESS_PROBE)
        .and_then(vike_polymarket::EgressCheck::into_result)
    {
        Ok(Some(e)) => eprintln!("egress OK: {e}"),
        Ok(None) => eprintln!("egress: unchecked (a read-only smoke declares no region)"),
        Err(e) => panic!("egress guard: {e}"),
    }
    let token = pick_a_live_token();
    tracing::info!(target: "vike_polymarket", token, "market_ws_delivers_book_and_quote: resolved token");

    let sink = Arc::new(RecordingSink::default());
    let source = DataClientBusSpecPolymarket::new(|| {}, &egress_support::seed())
        .expect("the seed holds the feed's rows");
    let mut feeds =
        DataClientBus::on(&Arc::new(source), Arc::clone(&sink) as Arc<dyn DataClientSink>);
    let id = feeds.subscribe_book(&token).expect("subscribe_book ok");

    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let (books, quotes) = (sink.books().len(), sink.quotes().len());
        if books > 0 && quotes > 0 {
            tracing::info!(target: "vike_polymarket", books, quotes, "live ticks observed");
            break;
        }
        assert!(
            Instant::now() < deadline,
            "no book+quote after 30s (books={books}, quotes={quotes}) — token quiet or WS down?"
        );
        std::thread::sleep(Duration::from_millis(200));
    }

    for (_, _, q) in sink.quotes() {
        assert!(q.ask >= q.bid, "crossed quote {}/{}", q.bid, q.ask);
    }
    for (_, _, b) in sink.books() {
        if let (Some(L2BookLevel { price: bid, .. }), Some(L2BookLevel { price: ask, .. })) =
            (b.best_bid(), b.best_ask())
        {
            assert!(bid < ask, "crossed L2 top {bid}/{ask}");
        }
    }

    feeds.unsubscribe(id);
    feeds.shutdown();
    tracing::info!(target: "vike_polymarket", "market_ws_delivers_book_and_quote: clean shutdown");
}

/// An inline `DataClientSink` forwarding straight to a `vike_execution::TickSender` — the minimal
/// equivalent of `vike_tradehub_core::CoreLaneSink` (the daemon's feed→core forwarder) needed to prove
/// ticks reach a mounted strategy through the real core, not just the venue-side sink.
struct CoreTickSink(vike_execution::TickSender);

impl DataClientSink for CoreTickSink {
    fn seed_bars(&self, _v: &str, _s: &str, _i: &str, _b: Vec<Bar>) {}
    fn close_bar(&self, _v: &str, _s: &str, _i: &str, _b: Bar) {}
    fn forming_bar(&self, _v: &str, _s: &str, _i: &str, _b: Bar) {}
    fn mark_tick(&self, _v: &str, _s: &str, _p: f64, _t: i64) {}
    fn quote(&self, venue: &str, symbol: &str, quote: TickQuote) {
        let _ = self.0.quote(vike_execution::QuoteUpdate {
            venue: venue.into(),
            symbol: symbol.into(),
            quote,
        });
    }
    fn trade(&self, venue: &str, symbol: &str, trade: TickTrade) {
        let _ = self.0.trade(vike_execution::TradeUpdate {
            venue: venue.into(),
            symbol: symbol.into(),
            trade,
        });
    }
    fn book(&self, venue: &str, symbol: &str, book: Arc<L2Book>) {
        let _ = self.0.book(vike_execution::BookUpdate {
            venue: venue.into(),
            symbol: symbol.into(),
            book,
        });
    }
}

#[derive(Default)]
struct TickCounter {
    quotes: Arc<AtomicUsize>,
    books: Arc<AtomicUsize>,
}

impl vike_model::Strategy<vike_core::LiveBroker> for TickCounter {
    fn on_quote_tick(&mut self, _b: &mut vike_core::LiveBroker, q: &TickQuote) {
        assert!(q.ask >= q.bid, "crossed quote {}/{}", q.bid, q.ask);
        self.quotes.fetch_add(1, Ordering::Relaxed);
    }
    fn on_order_book(&mut self, _b: &mut vike_core::LiveBroker, book: &L2Book) {
        if let (Some(L2BookLevel { price: bid, .. }), Some(L2BookLevel { price: ask, .. })) =
            (book.best_bid(), book.best_ask())
        {
            assert!(bid < ask, "crossed L2 top {bid}/{ask}");
        }
        self.books.fetch_add(1, Ordering::Relaxed);
    }
}

#[test]
#[ignore = "network"]
fn ticks_reach_a_mounted_strategy() {
    vike_log::test_init();
    egress_support::declare_from_the_settings_database();
    match vike_polymarket::check_expected_egress(None, vike_polymarket::DEFAULT_EGRESS_PROBE)
        .and_then(vike_polymarket::EgressCheck::into_result)
    {
        Ok(Some(e)) => eprintln!("egress OK: {e}"),
        Ok(None) => eprintln!("egress: unchecked (a read-only smoke declares no region)"),
        Err(e) => panic!("egress guard: {e}"),
    }
    let token = pick_a_live_token();
    tracing::info!(target: "vike_polymarket", token, "ticks_reach_a_mounted_strategy: resolved token");

    let (quotes, books) = (Arc::<AtomicUsize>::default(), Arc::<AtomicUsize>::default());
    let engine = vike_execution::Engine::new(
        vike_execution::Account::new(1.0, "polymarket", None, vike_execution::BalanceMode::Delta),
        vike_execution::RiskGate::new(vike_model::RiskLimits::new()),
        vike_execution::testing::RecordingClient::default(),
        "polymarket",
        &token,
    );
    let config = vike_core::CoreConfig {
        strategy: Some(vike_core::Mount {
            order_client_id_code: None,
            account: None,
            symbols: Vec::new(),
            controller_id: None,
            underlying_symbol: None,
            venue: "polymarket".into(),
            symbol: token.clone(),
            interval: "1m".into(),
            strategy: Box::new(TickCounter {
                quotes: Arc::clone(&quotes),
                books: Arc::clone(&books),
            }),
        }),
        ..vike_core::CoreConfig::default()
    };
    let handle = vike_core::spawn_core(engine, config);

    let sink: Arc<dyn DataClientSink> = Arc::new(CoreTickSink(handle.tick_sender()));
    let source = DataClientBusSpecPolymarket::new(|| {}, &egress_support::seed())
        .expect("the seed holds the feed's rows");
    let mut feeds = DataClientBus::on(&Arc::new(source), sink);
    let id = feeds.subscribe_book(&token).expect("subscribe_book ok");

    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let (qn, bn) = (quotes.load(Ordering::Relaxed), books.load(Ordering::Relaxed));
        if qn > 0 && bn > 0 {
            tracing::info!(target: "vike_polymarket", qn, bn, "live ticks reached the mounted strategy");
            break;
        }
        assert!(
            Instant::now() < deadline,
            "strategy counters idle after 30s (quotes={qn}, books={bn})"
        );
        std::thread::sleep(Duration::from_millis(200));
    }

    feeds.unsubscribe(id);
    feeds.shutdown();
    handle.shutdown_and_join();
    tracing::info!(target: "vike_polymarket", "ticks_reach_a_mounted_strategy: clean shutdown");
}
