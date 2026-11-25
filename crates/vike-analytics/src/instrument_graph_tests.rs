use super::*;
use vike_marketdata::OptionKind;
use vike_model::test_support::corpus::lcg;
use vike_model::test_support::instruments::perp;
use vike_model::{
    ContractFuture, ContractFutureDelivery, ContractFutureFamily, ContractOption,
    ContractOptionExerciseStyle, ContractPredictionOutcome, InstrumentVenueIds, Margining,
};

const MARCH: i64 = 1_774_598_400_000;
const APRIL: i64 = MARCH + 7 * 86_400_000;

fn spot(venue: &str, symbol: &str, base: &str, quote: &str) -> Instrument {
    Instrument::new(venue, symbol, base, quote, Contract::CryptoSpot)
}

fn perpetual(venue: &str, symbol: &str, base: &str, quote: &str) -> Instrument {
    Instrument::new(venue, symbol, base, quote, perp(Margining::Linear, quote))
}

fn future(venue: &str, symbol: &str, base: &str, quote: &str, expiry_ms: i64) -> Instrument {
    let contract = Contract::Future(ContractFuture {
        margining: Margining::Inverse,
        settle: base.into(),
        value_currency: None,
        expiry_ms,
        tenor: None,
        delivery: ContractFutureDelivery::Cash,
        family: ContractFutureFamily::Crypto,
    });
    Instrument::new(venue, symbol, base, quote, contract)
}

fn option(symbol: &str, right: OptionKind, expiry_ms: i64) -> Instrument {
    let contract = Contract::Option(ContractOption {
        right,
        strike: 90_000.0,
        expiry_ms,
        style: ContractOptionExerciseStyle::European,
        margining: Margining::Inverse,
        settle: "BTC".into(),
        value_currency: None,
        underlying: Some("BTC".into()),
    });
    Instrument::new("deribit", symbol, "BTC", "USD", contract)
}

fn outcome(token: &str, condition: &str, label: &str, set: Option<&str>) -> Instrument {
    let contract = Contract::PredictionOutcome(ContractPredictionOutcome {
        condition_id: condition.into(),
        outcome: label.into(),
        token_id: token.into(),
        resolution_ms: None,
        neg_risk: Some(set.is_some()),
    });
    Instrument::new("polymarket", token, label, "USDC", contract).with_wire(InstrumentVenueIds {
        asset_id: None,
        coin: None,
        group: set.map(String::from),
    })
}

/// Five venues' rows, by position: binance's spot and perpetual, bybit's perpetual stated in lower
/// case with blanks, deribit's perpetual, March future and two options (March, whose future is
/// listed, and April, whose is not), okx's ETH spot and March future (no perpetual) and a BTC
/// perpetual carrying an `instFamily` in `wire.group`, polymarket's three tokens of one neg-risk set
/// and one plain market, and a dukascopy row that states no base.
fn catalog() -> Vec<Instrument> {
    let swap = InstrumentVenueIds { asset_id: None, coin: None, group: Some("BTC-USD".into()) };
    vec![
        spot("binance", "BTCUSDT", "BTC", "USDT"),
        perpetual("binance", "BTCUSDT.P", "BTC", "USDT"),
        perpetual("bybit", "BTCUSDT.P", " btc", "usdt "),
        perpetual("deribit", "BTC-PERPETUAL", "BTC", "USD"),
        future("deribit", "BTC-27MAR26", "BTC", "USD", MARCH),
        option("BTC-27MAR26-90000-C", OptionKind::Call, MARCH),
        option("BTC-3APR26-90000-P", OptionKind::Put, APRIL),
        spot("okx", "ETH-USDT", "ETH", "USDT"),
        future("okx", "ETH-USDT-260327", "ETH", "USDT", MARCH),
        outcome("101", "0xa", "Yes", Some("0xset")),
        outcome("102", "0xa", "No", Some("0xset")),
        outcome("103", "0xb", "Yes", Some("0xset")),
        outcome("104", "0xc", "Yes", None),
        spot("dukascopy", "AAPL.US/USD", "", "USD"),
        perpetual("okx", "BTC-USD-SWAP", "BTC", "USD").with_wire(swap),
    ]
}

#[test]
fn the_catalog_graph_by_hand() {
    let graph = instrument_graph(&catalog());
    let venues = |list: &[&str]| list.iter().map(|v| (*v).to_string()).collect::<Vec<_>>();
    assert_eq!(
        graph.assets,
        vec![
            AssetGroup {
                asset: "BTC".into(),
                members: vec![0, 1, 2, 3, 4, 5, 6, 14],
                venues: venues(&["binance", "bybit", "deribit", "okx"]),
            },
            AssetGroup { asset: "ETH".into(), members: vec![7, 8], venues: venues(&["okx"]) },
        ],
        "no outcome label and no unstated base is an asset"
    );
    assert_eq!(
        graph.neg_risk_sets,
        vec![NegRiskSet { group: "0xset".into(), members: vec![9, 10, 11] }],
        "okx's instFamily is no neg-risk set"
    );
    let links: Vec<(usize, usize)> =
        graph.links.iter().map(|link| (link.derivative, link.underlying)).collect();
    // binance perp → spot; deribit future → perp, March option → March future, April option →
    // perp; okx future → spot (no perp). bybit's and okx's BTC perps have no spot in their book.
    assert_eq!(links, vec![(1, 0), (4, 3), (5, 4), (6, 3), (8, 7)]);
}

#[test]
fn every_row_of_the_nearest_step_is_linked() {
    let rows = vec![
        future("deribit", "BTC-27MAR26", "BTC", "USD", MARCH),
        spot("deribit", "BTC_USD_A", "BTC", "USD"),
        spot("deribit", "BTC_USD_B", "BTC", "USD"),
    ];
    let links: Vec<(usize, usize)> =
        instrument_graph(&rows).links.iter().map(|l| (l.derivative, l.underlying)).collect();
    assert_eq!(links, vec![(0, 1), (0, 2)]);
    assert_eq!(instrument_graph(&[]), InstrumentGraph::default());
}

fn near(got: f64, want: f64) -> bool {
    (got - want).abs() <= 1e-14 * want.abs().max(1.0)
}

/// Series `0`, `1` = series 0 one step later and `2` = series 0 two steps later, over 200 seeded
/// draws, at the positions `order` names: `0` leads both and `1` leads `2`, each at its own lag with
/// a correlation of one.
fn chain(order: [usize; 3]) -> Vec<Vec<f64>> {
    let mut seed = 7;
    let a: Vec<f64> = (0..202).map(|_| lcg(&mut seed) - 0.5).collect();
    // Series `s` at observation `t` is `a[t + 2 - s]`.
    order.iter().map(|&series| a[2 - series..202 - series].to_vec()).collect()
}

#[test]
fn edges_point_from_the_leader_and_leadership_ranks_the_chain() {
    let edges = lead_lag_edges(&chain([0, 1, 2]), 2, 1.5).unwrap();
    let shape: Vec<(usize, usize, i64)> =
        edges.iter().map(|e| (e.leader, e.follower, e.lag)).collect();
    assert_eq!(shape, vec![(0, 1, 1), (0, 2, 2), (1, 2, 1)]);
    assert!(edges.iter().all(|e| near(e.correlation, 1.0) && e.ratio > 1.5), "{edges:?}");
    assert_eq!(net_leadership(&edges, 3), vec![1.0, 0.0, -1.0]);
    // The same three in reverse column order: every edge turns around.
    let edges = lead_lag_edges(&chain([2, 1, 0]), 2, 1.5).unwrap();
    let shape: Vec<(usize, usize, i64)> =
        edges.iter().map(|e| (e.leader, e.follower, e.lag)).collect();
    assert_eq!(shape, vec![(1, 0, 1), (2, 0, 2), (2, 1, 1)]);
    assert_eq!(net_leadership(&edges, 3), vec![-1.0, 0.0, 1.0]);
    // No pair reaches an impossible ratio; a single node leads nothing.
    assert_eq!(lead_lag_edges(&chain([0, 1, 2]), 2, f64::INFINITY).unwrap(), vec![]);
    assert_eq!(net_leadership(&[], 1), vec![0.0]);
}

/// The star `K₁,₃`: the largest eigenvalue is `√3` with the vector `(√3, 1, 1, 1) / √6`, so the
/// centre scores `1/√2` and each leaf `1/√6`; the path of three is the star `K₁,₂`, `(1, √2, 1) / 2`.
#[test]
fn eigenvector_centrality_by_hand() {
    let star = [
        vec![0.0, 1.0, 1.0, 1.0],
        vec![1.0, 0.0, 0.0, 0.0],
        vec![1.0, 0.0, 0.0, 0.0],
        vec![1.0, 0.0, 0.0, 0.0],
    ];
    let scores = eigenvector_centrality(&star).unwrap();
    let (centre, leaf) = (0.5f64.sqrt(), (1.0f64 / 6.0).sqrt());
    assert!(near(scores[0], centre) && scores[1..].iter().all(|s| near(*s, leaf)), "{scores:?}");
    let path = [vec![0.0, 1.0, 0.0], vec![1.0, 0.0, 1.0], vec![0.0, 1.0, 0.0]];
    let scores = eigenvector_centrality(&path).unwrap();
    assert!(near(scores[0], 0.5) && near(scores[1], centre) && near(scores[2], 0.5), "{scores:?}");
}

#[test]
fn no_centrality_without_one_leading_vector() {
    // Two equal edges with nothing between them: the eigenvalue 1 twice.
    let two_edges = [
        vec![0.0, 1.0, 0.0, 0.0],
        vec![1.0, 0.0, 0.0, 0.0],
        vec![0.0, 0.0, 0.0, 1.0],
        vec![0.0, 0.0, 1.0, 0.0],
    ];
    assert!(eigenvector_centrality(&two_edges).is_none());
    // A stronger edge wins, and the other component scores zero.
    let unequal = [
        vec![0.0, 2.0, 0.0, 0.0],
        vec![2.0, 0.0, 0.0, 0.0],
        vec![0.0, 0.0, 0.0, 1.0],
        vec![0.0, 0.0, 1.0, 0.0],
    ];
    let scores = eigenvector_centrality(&unequal).unwrap();
    assert!(near(scores[0], 0.5f64.sqrt()) && near(scores[1], 0.5f64.sqrt()), "{scores:?}");
    assert!(scores[2] == 0.0 && scores[3] == 0.0, "{scores:?}");
    assert!(eigenvector_centrality(&[vec![0.0, -1.0], vec![-1.0, 0.0]]).is_none(), "negative");
    assert!(eigenvector_centrality(&[vec![0.0, f64::NAN], vec![f64::NAN, 0.0]]).is_none());
    assert!(eigenvector_centrality(&[vec![0.0]]).is_none(), "no positive weight");
    assert!(eigenvector_centrality(&[vec![0.0, 1.0]]).is_none(), "not square");
    assert!(eigenvector_centrality(&[vec![0.0, 1.0], vec![1.0]]).is_none(), "ragged");
    assert!(eigenvector_centrality(&[]).is_none(), "empty");
}
