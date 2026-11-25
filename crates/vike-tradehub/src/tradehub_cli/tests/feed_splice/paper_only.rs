//! **The paper-only node's live prices** (`paper_feeds.rs`'s `wire_paper_feeds`, the owner's yes of 2026-11-16): the
//! keyless venues its mounts trade are wired through the live arm's own feed arms over an unarmed table, a manual
//! market buy on the paper engine fills at the ask of the quote the feed delivers, a credentialed venue is never
//! dialled, and a price path that is away wires nothing and says why.

use super::*;
use crate::feeds::DatahubProbe;
use crate::tradehub_cli::paper_feeds::wire_paper_feeds;
use vike_execution::{Command, OrderIntent};
use vike_log::poll::wait_until;
use vike_model::{OrderRequest, OrderType};

/// A constructor that hands every arm an acknowledging feed and KEEPS the sink it was given (the test pushes the
/// venue's quotes through it), recording each venue it was asked for and the network the dial named (`None` is the
/// main network); `refused` makes its datahub probe answer no.
#[derive(Default)]
struct Keeping {
    sinks: Mutex<Vec<Dialled>>,
    refused: Option<String>,
}

/// One dial [`Keeping`] was asked for.
struct Dialled {
    venue: String,
    network: Option<String>,
    sink: Arc<dyn DataClientSink>,
}

impl Keeping {
    fn asked(&self) -> Vec<String> {
        self.sinks.lock().expect("sinks").iter().map(|d| d.venue.clone()).collect()
    }

    fn networks(&self) -> Vec<(String, Option<String>)> {
        let sinks = self.sinks.lock().expect("sinks");
        sinks.iter().map(|d| (d.venue.clone(), d.network.clone())).collect()
    }
}

impl StrategyLaneFactories for Keeping {
    crate::testutil::scripted_strategy!();

    fn client(
        &self,
        venue: &str,
        network: Option<&str>,
        sink: Arc<dyn DataClientSink>,
    ) -> Result<Box<dyn DataClient + Send>, String> {
        let network = network.map(str::to_string);
        self.sinks.lock().expect("sinks").push(Dialled { venue: venue.to_string(), network, sink });
        Ok(Box::new(super::super::StubFeed::default()))
    }

    fn datahub_probe(&self, _venues: &[String]) -> Option<DatahubProbe> {
        let refused = self.refused.clone();
        Some(DatahubProbe::new(move || refused.clone().map(Into::into)))
    }
}

/// A HALT sentinel this test owns and never creates (`deribit.rs`'s `own_sentinel`, the same reason).
fn own_sentinel() -> PathBuf {
    std::env::temp_dir()
        .join(format!("vike-tradehub-paper-only-prices-{}", std::process::id()))
        .join("HALT")
}

/// A 1m mount of `venue` on `symbol`.
fn mount(venue: &str, symbol: &str, tick_size: f64) -> MountPlacement {
    let mut config = MountPlacement::new(venue, symbol, 100_000.0, tick_size);
    config.interval = "1m".to_string();
    config.interval_ms = 60_000;
    config
}

/// A mount that trades nothing itself: the order is the operator's.
struct Idle;

impl vike_model::Strategy<vike_core::LiveBroker> for Idle {}

/// **A keyless venue is priced, a credentialed one is not dialled, and a manual market buy fills at the ask**: a
/// paper-only node mounting deribit (keyless) and oanda (credentialed) wires deribit's feed alone, says it prices
/// deribit, and the paper deribit engine fills an operator's market buy at the ask of the quote that feed delivers.
#[test]
fn a_paper_only_node_prices_its_keyless_venue_and_a_market_buy_fills_at_the_ask() {
    let symbol =
        super::fixture_account_symbol("deribit").expect("a deribit account states a symbol");
    let deribit = mount("deribit", symbol, 0.5);
    let oanda = mount("oanda", "EUR_USD", 0.00001);
    let sentinel = own_sentinel();
    assert!(!sentinel.exists(), "the pinned sentinel must not exist: {}", sentinel.display());
    let core = vike_tradehub_core::build_paper_strategy_core_with(
        Box::new(Idle),
        &deribit,
        StrategyCorePaperConfig {
            halt: StrategyCorePaperHalt::Pinned(sentinel),
            ..Default::default()
        },
    );
    let ctors = Keeping::default();

    let wired = wire_paper_feeds(&[deribit.clone(), oanda], &core.handle, &ctors);

    assert_eq!(
        wired.priced,
        ["deribit"],
        "the keyless venue is priced, the credentialed one is not"
    );
    assert_eq!(wired.unavailable, None);
    assert_eq!(wired.feeds.len(), 1);
    assert_eq!(ctors.asked(), ["deribit"], "oanda's feed is never dialled on a paper-only node");

    core.handle.send_command(Command::Order(OrderIntent::Submit(Box::new(OrderRequest {
        order_client_id: "paper-buy-1".into(),
        venue: "deribit".into(),
        symbol: symbol.into(),
        side: 1,
        qty: 0.001,
        order_type: OrderType::Market,
        ..Default::default()
    }))));
    assert!(
        wait_until(Duration::from_secs(10), || core
            .handle
            .snapshot()
            .order("paper-buy-1")
            .is_some()),
        "the market buy never reached the paper book"
    );
    let sink = Arc::clone(&ctors.sinks.lock().expect("sinks")[0].sink);
    let filled = wait_until(Duration::from_secs(10), || {
        sink.quote(
            "deribit",
            symbol,
            vike_marketdata::TickQuote {
                timestamp: vike_model::now_ms(),
                local_timestamp: vike_model::now_ms(),
                bid: 49_990.0,
                ask: 50_010.0,
                bid_size: 1_000.0,
                ask_size: 1_000.0,
                symbol: symbol.into(),
            },
        );
        !core.fills.lock().expect("fills").is_empty()
    });
    assert!(filled, "no quote from the wired feed filled the market buy");
    let fill = core.fills.lock().expect("fills")[0].clone();
    assert_eq!((fill.side, fill.price), (1, 50_010.0), "a market buy pays the ask, never the mid");

    let mut feeds = wired.feeds;
    for feed in &mut feeds {
        feed.shutdown();
    }
    core.handle.shutdown_and_join();
}

/// **A price path that is away wires nothing and says why** (the live arm's feed gate): the datahub probe's refusal
/// is the `unavailable` cause the ready banner prints after `PRICE FEEDS UNAVAILABLE`, no venue is priced and no feed
/// is dialled. A node mounting only credentialed venues asks nothing at all.
#[test]
fn a_paper_only_node_whose_price_path_is_away_prices_nothing_and_says_why() {
    let symbol =
        super::fixture_account_symbol("deribit").expect("a deribit account states a symbol");
    let deribit = mount("deribit", symbol, 0.5);
    let sentinel = own_sentinel();
    let core = vike_tradehub_core::build_paper_strategy_core_with(
        Box::new(Idle),
        &deribit,
        StrategyCorePaperConfig {
            halt: StrategyCorePaperHalt::Pinned(sentinel),
            ..Default::default()
        },
    );
    let why = "the tradehub holds no strategy-lane key (VIKE_DATAHUB_TRADE_KEY)".to_string();
    let away = Keeping { refused: Some(why.clone()), ..Keeping::default() };

    let wired = wire_paper_feeds(std::slice::from_ref(&deribit), &core.handle, &away);
    assert!(wired.priced.is_empty() && wired.feeds.is_empty());
    assert_eq!(wired.unavailable, Some(crate::feeds::FeedsDown::from(why)));
    assert!(away.asked().is_empty(), "nothing is dialled past a refused probe");

    let credentialed = Keeping { refused: Some("never asked".into()), ..Keeping::default() };
    let wired =
        wire_paper_feeds(&[mount("oanda", "EUR_USD", 0.00001)], &core.handle, &credentialed);
    assert!(wired.priced.is_empty() && wired.feeds.is_empty());
    assert_eq!(wired.unavailable, None, "no keyless venue, so no feed was wanted");
    core.handle.shutdown_and_join();
}

/// **A paper hyperliquid book prices on the REAL market**: with nothing armed the live arm's plan picks hyperliquid's
/// TEST network, and the paper-only arm dials the main network's public feed instead (the dial names no network,
/// `feeds/hyperliquid.rs`'s `strategy_network`), as binance's paper reads binance.com. It is a data dial only: the
/// constructor is asked for a feed and nothing else, and the paper core arms no account.
#[test]
fn a_paper_only_node_reads_hyperliquids_main_network_with_nothing_armed() {
    let symbol = super::fixture_account_symbol("hyperliquid")
        .expect("a hyperliquid account states a symbol");
    let hyperliquid = mount("hyperliquid", symbol, 1.0);
    let unarmed = venue_feed_plan(
        &hyperliquid,
        &vike_tradehub_core::ArmingTable::default(),
        &vike_tradehub_core::AdapterPolicy::default(),
    )
    .expect("keyless hyperliquid plans over an unarmed table");
    assert!(
        matches!(unarmed, crate::StrategyLanePlan::Hyperliquid(vike_model::VenueNetwork::Testnet)),
        "the live arm's unarmed answer, which the paper arm must not take: {unarmed:?}"
    );
    assert!(matches!(
        crate::tradehub_cli::paper_feeds::on_the_real_market(unarmed),
        crate::StrategyLanePlan::Hyperliquid(vike_model::VenueNetwork::Mainnet)
    ));

    let core = vike_tradehub_core::build_paper_strategy_core_with(
        Box::new(Idle),
        &hyperliquid,
        StrategyCorePaperConfig {
            halt: StrategyCorePaperHalt::Pinned(own_sentinel()),
            ..Default::default()
        },
    );
    let ctors = Keeping::default();
    let mut wired = wire_paper_feeds(std::slice::from_ref(&hyperliquid), &core.handle, &ctors);
    assert_eq!(wired.priced, ["hyperliquid"]);
    assert_eq!(
        ctors.networks(),
        [("hyperliquid".to_string(), None)],
        "the main network: the dial names no network"
    );
    for feed in &mut wired.feeds {
        feed.shutdown();
    }
    core.handle.shutdown_and_join();
}
