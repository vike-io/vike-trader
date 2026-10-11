//! **The Options tool's poll pass, driven end to end over a real loopback datahub double.**
//!
//! `crates/vike-app-core/src/tools/options_tests.rs` gates the pure halves (the plan, the merge,
//! the sentences); this file gates what one pass of the poll thread actually does once something
//! answers. The double is a `std::net::TcpListener` speaking the real frame codec, so a change to
//! the handshake or to `Response::OptionsChain`'s shape reddens here rather than being agreed with
//! by a fake. The properties:
//!
//! 1. **One pass fills the tool**: the deribit listing becomes the option index, the shown
//!    underlying and the background ones are asked for, and the status reads LIVE.
//! 2. **The connection is HELD**: a second pass asks again over the SAME socket.
//! 3. **An old server is sent NO options frame**, and the status names the verb it lacks.
//! 4. **A refusal is the server's sentence and keeps the last good chains**, and a SWITCHED
//!    datahub shows nothing of the old one's.
//! 5. **With no window open, nothing is dialled at all.**

use std::collections::BTreeMap;
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use vike_app_core::data::catalog_wire::CatalogDial;
use vike_app_core::tools::{OptionsLink, OptionsPoller, ToolData};
use vike_datahub_client::catalog::{CatalogListing, CatalogOutcome};
use vike_datahub_client::options::{OptionsChainOutcome, OptionsChainReply};
use vike_datahub_client::proto::{Request, Response, read_frame, write_frame};
use vike_datahub_client::{FEATURE_OPTIONS_CHAIN, FEATURE_VENUE_CATALOG};
use vike_marketdata::{Expiry, OptionChain, OptionKind, UnderlyingChains, UnderlyingKind};
use vike_model::{Contract, Instrument, OptionSpec};

mod common;

const FRONT: &str = "2026-06-26";

fn option(name: &str, underlying: &str) -> Instrument {
    Instrument::new(
        "deribit",
        name,
        underlying,
        underlying,
        Contract::Option(OptionSpec {
            right: OptionKind::Call,
            strike: 100.0,
            expiry_ms: vike_options::expiry_ms(FRONT),
            style: vike_model::ExerciseStyle::European,
            margining: vike_model::Margining::Inverse,
            settle: underlying.to_string(),
            value_ccy: Some(underlying.to_string()),
            underlying: Some(underlying.to_string()),
        }),
    )
}

/// The front-expiry bundle the double answers for `underlying`.
fn front_bundle(underlying: &str) -> UnderlyingChains {
    let expiry = Expiry { date: FRONT.into(), dte: 1, label: "26 Jun".into() };
    let chain = OptionChain {
        underlying: underlying.into(),
        underlying_kind: UnderlyingKind::Crypto,
        underlying_price: Some(100.0),
        expiry: expiry.clone(),
        asof_ms: 0,
        source: "deribit".into(),
        rows: Vec::new(),
    };
    UnderlyingChains {
        default_expiry: FRONT.into(),
        expiries: vec![expiry],
        chains: BTreeMap::from([(FRONT.to_string(), chain)]),
    }
}

/// What every `OptionsChain` request is answered with (switchable mid-test).
#[derive(Clone, Copy, PartialEq)]
enum Answer {
    Chains,
    NotArmed,
}

#[derive(Default)]
struct Counts {
    connections: AtomicUsize,
    catalog: AtomicUsize,
    chains: AtomicUsize,
}

/// A KEY-LESS datahub double: one thread per connection (the index fetch and the held chain
/// connection overlap), counting connections and verb frames.
struct FakeHub {
    addr: String,
    counts: Arc<Counts>,
    answer: Arc<Mutex<Answer>>,
}

impl FakeHub {
    fn spawn(features: Vec<String>, answer: Answer) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().expect("local_addr").to_string();
        let counts = Arc::new(Counts::default());
        let shared = Arc::clone(&counts);
        let answer = Arc::new(Mutex::new(answer));
        let script = Arc::clone(&answer);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { break };
                shared.connections.fetch_add(1, Ordering::SeqCst);
                let (features, counts, answer) =
                    (features.clone(), Arc::clone(&shared), Arc::clone(&script));
                std::thread::spawn(move || serve(stream, features, &answer, &counts));
            }
        });
        Self { addr, counts, answer }
    }

    fn dial(&self) -> CatalogDial {
        CatalogDial {
            addr: self.addr.clone(),
            key_name: "VIKE_DATAHUB_OBSERVE_KEY".to_string(),
            keys: None,
        }
    }

    fn answer_with(&self, answer: Answer) {
        *self.answer.lock().unwrap() = answer;
    }

    fn count(&self, pick: impl Fn(&Counts) -> &AtomicUsize) -> usize {
        pick(&self.counts).load(Ordering::SeqCst)
    }
}

fn serve(stream: TcpStream, features: Vec<String>, answer: &Mutex<Answer>, counts: &Counts) {
    let mut r = stream.try_clone().expect("clone the accepted stream");
    let mut w = stream;
    if !common::accept_keyless(&mut w, features) {
        return;
    }
    while let Ok(req) = read_frame::<_, Request>(&mut r) {
        let resp = match req {
            Request::VenueCatalog { venue } => {
                counts.catalog.fetch_add(1, Ordering::SeqCst);
                let instruments =
                    vec![option("BTC-26JUN26-100-C", "BTC"), option("ETH-26JUN26-100-C", "ETH")];
                Response::VenueCatalog(CatalogListing {
                    venue,
                    outcome: CatalogOutcome::Listed {
                        instruments,
                        truncated: false,
                        cached: false,
                    },
                })
            }
            Request::OptionsChain { venue, underlying, .. } => {
                counts.chains.fetch_add(1, Ordering::SeqCst);
                let answer = *answer.lock().unwrap();
                // The served set rides on every answer; a server serving the venue nothing names none.
                let served_underlyings = match answer {
                    Answer::Chains => vec!["BTC".to_string(), "ETH".to_string()],
                    Answer::NotArmed => Vec::new(),
                };
                let outcome = match answer {
                    Answer::Chains => OptionsChainOutcome::Chains(front_bundle(&underlying)),
                    Answer::NotArmed => OptionsChainOutcome::NotArmed,
                };
                Response::OptionsChain(OptionsChainReply {
                    venue,
                    underlying,
                    served_underlyings,
                    outcome,
                })
            }
            other => Response::Error(format!("unexpected verb: {other:?}")),
        };
        if write_frame(&mut w, &resp).is_err() {
            return;
        }
    }
}

/// The tool's shared data with a live link, and one window showing the default underlying.
fn open_window() -> Mutex<ToolData> {
    let (tx, _rx) = std::sync::mpsc::channel();
    let td = ToolData { opt_link: OptionsLink::new(tx), ..ToolData::default() };
    td.opt_link.want(None, None);
    Mutex::new(td)
}

#[test]
fn one_pass_fills_the_tool_and_the_second_reuses_the_connection() {
    let hub = FakeHub::spawn(
        vec![FEATURE_VENUE_CATALOG.to_string(), FEATURE_OPTIONS_CHAIN.to_string()],
        Answer::Chains,
    );
    let shared = open_window();
    let mut poller = OptionsPoller::default();
    let t0 = Instant::now();

    assert!(poller.run_once(&shared, Some(&hub.dial()), t0), "a live pick is asked for");
    {
        let td = shared.lock().unwrap();
        assert_eq!(td.opt_index.len(), 2, "the deribit listing became the index");
        assert_eq!(td.opt_underlyings, ["BTC", "ETH"], "the shown one and the background one");
        assert!(td.opt_status.starts_with("LIVE"), "{}", td.opt_status);
        assert!(td.opt_status.contains(&hub.addr), "{}", td.opt_status);
    }
    assert_eq!(hub.count(|c| &c.catalog), 1);
    assert_eq!(hub.count(|c| &c.chains), 2, "BTC (shown) and ETH (background)");
    assert_eq!(hub.count(|c| &c.connections), 2, "one for the listing, one held for the chains");

    shared.lock().unwrap().opt_link.want(None, None);
    assert!(poller.run_once(&shared, Some(&hub.dial()), t0 + Duration::from_secs(1)));
    assert_eq!(hub.count(|c| &c.chains), 3, "one second later: the shown underlying only");
    assert_eq!(hub.count(|c| &c.catalog), 1, "a populated index is not refetched");
    assert_eq!(hub.count(|c| &c.connections), 2, "the chain connection is HELD across passes");
}

#[test]
fn an_old_server_is_sent_no_options_frame_and_says_so() {
    let hub = FakeHub::spawn(vec![FEATURE_VENUE_CATALOG.to_string()], Answer::Chains);
    let shared = open_window();
    let mut poller = OptionsPoller::default();
    assert!(poller.run_once(&shared, Some(&hub.dial()), Instant::now()));
    let status = shared.lock().unwrap().opt_status.clone();
    assert!(status.contains(FEATURE_OPTIONS_CHAIN), "the missing verb is named: {status}");
    assert!(status.contains("nothing was sent"), "{status}");
    assert_eq!(hub.count(|c| &c.chains), 0, "not one OptionsChain frame may be written");
    assert!(shared.lock().unwrap().opt_by_underlying.is_empty(), "and nothing was invented");
}

#[test]
fn a_refusal_is_the_servers_sentence_and_keeps_the_last_good_chains() {
    let hub = FakeHub::spawn(
        vec![FEATURE_VENUE_CATALOG.to_string(), FEATURE_OPTIONS_CHAIN.to_string()],
        Answer::Chains,
    );
    let shared = open_window();
    let mut poller = OptionsPoller::default();
    let t0 = Instant::now();
    assert!(poller.run_once(&shared, Some(&hub.dial()), t0));
    let seeded = shared.lock().unwrap().opt_by_underlying.clone();
    assert!(seeded.contains_key("BTC"));

    // The SAME server's operator turns its options lane off.
    hub.answer_with(Answer::NotArmed);
    shared.lock().unwrap().opt_link.want(None, None);
    assert!(poller.run_once(&shared, Some(&hub.dial()), t0 + Duration::from_secs(1)));
    let td = shared.lock().unwrap();
    let expected = OptionsChainReply {
        venue: "deribit".into(),
        underlying: "BTC".into(),
        served_underlyings: Vec::new(),
        outcome: OptionsChainOutcome::NotArmed,
    }
    .describe();
    assert_eq!(td.opt_status, expected, "the SERVER's own words");
    assert_eq!(td.opt_by_underlying, seeded, "a refusal never replaces the last good chains");
}

/// **A switched backend shows nothing of the old one.** The moment the dial names another
/// datahub, the old server's chains, selector and index are gone — before the new one answers,
/// and whatever it answers.
#[test]
fn a_switched_datahub_never_shows_the_old_ones_chains() {
    let old = FakeHub::spawn(
        vec![FEATURE_VENUE_CATALOG.to_string(), FEATURE_OPTIONS_CHAIN.to_string()],
        Answer::Chains,
    );
    let shared = open_window();
    let mut poller = OptionsPoller::default();
    let t0 = Instant::now();
    assert!(poller.run_once(&shared, Some(&old.dial()), t0));
    assert!(!shared.lock().unwrap().opt_by_underlying.is_empty());

    let new = FakeHub::spawn(
        vec![FEATURE_VENUE_CATALOG.to_string(), FEATURE_OPTIONS_CHAIN.to_string()],
        Answer::NotArmed,
    );
    shared.lock().unwrap().opt_link.want(None, None);
    assert!(poller.run_once(&shared, Some(&new.dial()), t0 + Duration::from_secs(1)));
    let td = shared.lock().unwrap();
    assert!(td.opt_by_underlying.is_empty(), "the old server's chains are not last good here");
    assert!(td.opt_underlyings.is_empty(), "nor its selector");
    assert!(!td.opt_status.contains(&old.addr), "nor its status: {}", td.opt_status);
    assert_eq!(new.count(|c| &c.catalog), 1, "the index is asked of the NEW server");
    assert_eq!(new.count(|c| &c.connections), 2, "its listing, and the chain connection");
}

#[test]
fn with_no_window_open_nothing_is_dialled() {
    let hub = FakeHub::spawn(
        vec![FEATURE_VENUE_CATALOG.to_string(), FEATURE_OPTIONS_CHAIN.to_string()],
        Answer::Chains,
    );
    let (tx, _rx) = std::sync::mpsc::channel();
    let shared = Mutex::new(ToolData { opt_link: OptionsLink::new(tx), ..ToolData::default() });
    let mut poller = OptionsPoller::default();
    assert!(!poller.run_once(&shared, Some(&hub.dial()), Instant::now()));
    assert_eq!(hub.count(|c| &c.connections), 0);
}

#[test]
fn with_no_backend_the_status_says_so_and_nothing_is_asked() {
    let shared = open_window();
    let mut poller = OptionsPoller::default();
    assert!(poller.run_once(&shared, None, Instant::now()));
    let status = shared.lock().unwrap().opt_status.clone();
    assert!(status.contains("config.datahub_addr"), "{status}");
    assert!(!status.starts_with("LIVE"), "{status}");
}
