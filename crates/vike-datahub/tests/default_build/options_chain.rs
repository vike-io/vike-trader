//! The COMPOSED-PATH gate for the OPTIONS-CHAIN verb: a server on an ephemeral loopback port,
//! driven through `DatahubClient::options_chain`, with a scripted source (every build) or a real
//! `OptionsBook` fed scripted passes and feed rows (`options-serve`) standing where
//! `vike_datahub::options::deribit::real_options_lane` goes. No venue, no network beyond loopback,
//! and — like `venue_catalog.rs` — no DataFusion: the verb writes no store.
//!
//! 1. [`a_laneless_server_answers_not_armed`] — an unarmed build still answers, successfully;
//! 2. [`a_scripted_lane_answers_across_the_wire`] — the front expiry by default, an asked one, an
//!    unheld one omitted, the full strip kept, and the refusals as successes;
//! 3. [`the_folds_reach_the_client_through_the_server`] (`options-serve`) — a REST pass seeds the
//!    book, a markprice batch and a ticker row fold onto it, and the client sees each.

use std::net::{SocketAddr, TcpListener};
use std::sync::Arc;
use std::thread;

use vike_data::{HistStore, MemHistStore};
use vike_datahub::options::{OptionsLane, OptionsSource, select_expiries};
use vike_datahub::server::serve_with_options;
use vike_datahub_client::DatahubClient;
use vike_datahub_client::options::{OptionsChainOutcome, OptionsRefusal};
use vike_marketdata::{
    Expiry, OptionChain, OptionKind, OptionQuote, StrikeRow, UnderlyingChains, UnderlyingKind,
};

const DATES: [&str; 2] = ["2026-06-26", "2026-07-31"];
const CALL: &str = "BTC-26JUN26-100000-C";

/// Serve `lane` (or none) key-less over an empty in-memory store and return the address.
fn spawn(lane: Option<OptionsLane>) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let store: Arc<dyn HistStore + Send + Sync> = Arc::new(MemHistStore::new());
    let lane = lane.map(Arc::new);
    thread::spawn(move || {
        let _ =
            serve_with_options(listener, store, None, None, None, None, None, None, false, lane);
    });
    addr
}

/// A BTC bundle over [`DATES`], one strike (spot 100k) with a coin-settled call and put.
fn bundle() -> UnderlyingChains {
    let chain = |d: &str| OptionChain {
        underlying: "BTC".into(),
        underlying_kind: UnderlyingKind::Crypto,
        underlying_price: Some(100_000.0),
        expiry: Expiry { date: d.into(), dte: 30, label: d.into() },
        asof_ms: 0,
        source: "deribit".into(),
        rows: vec![StrikeRow {
            strike: 100_000.0,
            call: Some(OptionQuote {
                bid: Some(500.0),
                mark: Some(550.0),
                iv: Some(0.5),
                instrument_name: Some(if d == DATES[0] { CALL.into() } else { format!("{d}-C") }),
                ..OptionQuote::new(100_000.0, OptionKind::Call)
            }),
            put: None,
        }],
    };
    UnderlyingChains {
        default_expiry: DATES[0].into(),
        expiries: DATES
            .iter()
            .map(|d| Expiry { date: (*d).into(), dte: 30, label: (*d).into() })
            .collect(),
        chains: DATES.iter().map(|d| ((*d).to_string(), chain(d))).collect(),
    }
}

/// A source that always holds [`bundle`] for BTC and nothing for ETH/SOL.
struct Scripted;

impl OptionsSource for Scripted {
    fn underlyings(&self) -> Vec<String> {
        vec!["BTC".into(), "ETH".into(), "SOL".into()]
    }
    fn snapshot(&self, u: &str, e: &[String]) -> Result<UnderlyingChains, String> {
        match u {
            "BTC" => Ok(select_expiries(&bundle(), e)),
            _ => Err("the first chain pass has not landed yet".into()),
        }
    }
}

fn lane_of(source: impl OptionsSource + 'static) -> OptionsLane {
    OptionsLane::new(vec![("deribit".into(), Box::new(source) as Box<dyn OptionsSource>)])
}

fn chains(client: &mut DatahubClient, u: &str, e: &[&str]) -> UnderlyingChains {
    let e: Vec<String> = e.iter().map(|s| (*s).to_string()).collect();
    match client.options_chain("deribit", u, &e).expect("answered").outcome {
        OptionsChainOutcome::Chains(c) => c,
        other => panic!("expected chains, got {other:?}"),
    }
}

#[test]
fn a_laneless_server_answers_not_armed() {
    let mut client = DatahubClient::connect(spawn(None)).expect("handshake");
    assert!(client.supports_options_chain(), "a build fact: {:?}", client.features());
    let reply = client.options_chain("deribit", "BTC", &[]).expect("a success, not an error");
    assert_eq!(reply.outcome, OptionsChainOutcome::NotArmed);
    assert!(reply.served_underlyings.is_empty(), "an unarmed server serves nothing");
}

#[test]
fn a_scripted_lane_answers_across_the_wire() {
    let mut client = DatahubClient::connect(spawn(Some(lane_of(Scripted)))).expect("handshake");

    let reply = client.options_chain("deribit", "BTC", &[]).expect("answered");
    assert_eq!(reply.served_underlyings, ["BTC", "ETH", "SOL"], "the served set rides the answer");
    let not_ready = client.options_chain("deribit", "SOL", &[]).expect("answered");
    assert_eq!(not_ready.served_underlyings, ["BTC", "ETH", "SOL"], "...a refusal's too");

    let front = chains(&mut client, "BTC", &[]);
    assert_eq!(front.chains.keys().collect::<Vec<_>>(), [DATES[0]], "empty = the front expiry");
    assert_eq!(front.expiries.len(), DATES.len(), "the FULL strip rides every answer");

    let asked = chains(&mut client, "BTC", &[DATES[1], "2027-01-29"]);
    assert_eq!(asked.chains.keys().collect::<Vec<_>>(), [DATES[1]], "an unheld expiry is omitted");

    let refused = |client: &mut DatahubClient, venue: &str, u: &str| match client
        .options_chain(venue, u, &[])
        .expect("a success")
        .outcome
    {
        OptionsChainOutcome::Refused(r) => r,
        other => panic!("expected a refusal, got {other:?}"),
    };
    assert_eq!(
        refused(&mut client, "okx", "BTC"),
        OptionsRefusal::UnknownVenue { supported: vec!["deribit".into()] }
    );
    assert!(matches!(
        refused(&mut client, "deribit", "DOGE"),
        OptionsRefusal::UnknownUnderlying { .. }
    ));
    assert!(matches!(refused(&mut client, "deribit", "ETH"), OptionsRefusal::NotReady { .. }));
    // ...and a malformed request is the one ERROR, refused before a frame is sent.
    assert!(client.options_chain("deribit", "btc", &[]).is_err());
    // The connection is still positional after every one of them.
    assert_eq!(chains(&mut client, "BTC", &[]).chains.len(), 1);
}

/// The fold-through path end to end: what the feeds fold into the book is what the client reads.
#[cfg(feature = "options-serve")]
#[test]
fn the_folds_reach_the_client_through_the_server() {
    use std::collections::BTreeMap;

    use vike_datahub::options::book::OptionsBook;
    use vike_marketdata::OptionTicker;
    use vike_model::{Contract, ExerciseStyle, Instrument, Margining, OptionSpec};
    use vike_options::OptionIndex;

    struct Book(OptionsBook, Vec<String>);
    impl OptionsSource for Book {
        fn underlyings(&self) -> Vec<String> {
            self.1.clone()
        }
        fn snapshot(&self, u: &str, e: &[String]) -> Result<UnderlyingChains, String> {
            self.0.snapshot(u, e)
        }
    }

    const NOW: i64 = 1_780_000_000_000;
    let book = OptionsBook::new();
    let mut client =
        DatahubClient::connect(spawn(Some(lane_of(Book(book.clone(), vec!["BTC".into()])))))
            .expect("handshake");

    // Before the first pass: NotReady, never an empty chain.
    let reply = client.options_chain("deribit", "BTC", &[]).expect("answered");
    assert!(matches!(reply.outcome, OptionsChainOutcome::Refused(OptionsRefusal::NotReady { .. })));

    // The REST pass lands, then the index, then a markprice batch and a ticker row.
    let focus = book.merge_pass(BTreeMap::from([("BTC".to_string(), bundle())]), &["BTC"], 10);
    assert!(focus.is_some_and(|f| f.contains(&CALL.to_string())), "the focus set names the call");
    let call = Instrument::new(
        "deribit",
        CALL,
        "BTC",
        "BTC",
        Contract::Option(OptionSpec {
            right: OptionKind::Call,
            strike: 100_000.0,
            expiry_ms: vike_options::expiry_ms(DATES[0]),
            style: ExerciseStyle::European,
            margining: Margining::Inverse,
            settle: "BTC".into(),
            value_ccy: Some("BTC".into()),
            underlying: Some("BTC".into()),
        }),
    );
    book.set_index(OptionIndex::from_instruments([&call]));
    let mut mark = OptionTicker::new(NOW, CALL);
    mark.mark = Some(0.01);
    mark.mark_iv = Some(0.7);
    assert_eq!(book.fold_markprice(&[mark], NOW), 1);

    let q = chains(&mut client, "BTC", &[]).chains[DATES[0]].rows[0].call.clone().expect("call");
    assert_eq!(
        q.mark,
        Some(0.01 * 100_000.0),
        "the streamed mark, coin-scaled, reached the client"
    );
    assert_eq!(q.iv, Some(0.7));

    let mut tick = OptionTicker::new(NOW, CALL);
    tick.bid = Some(0.006);
    assert_eq!(book.fold_ticker(&tick, NOW), 1);
    let q = chains(&mut client, "BTC", &[]).chains[DATES[0]].rows[0].call.clone().expect("call");
    assert_eq!(q.bid, Some(0.006 * 100_000.0), "the streamed bid reached the client");
    assert_eq!(q.mark, Some(0.01 * 100_000.0), "a ticker without a mark kept the streamed one");
}
