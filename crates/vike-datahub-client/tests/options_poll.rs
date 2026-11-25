//! **The Options tool's poll pass, driven end to end over a real loopback datahub double.**
//!
//! `crates/vike-datahub-client/src/tool_feeds/poll/options_tests.rs` gates the pure halves (the plan, the merge,
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
//! 6. **A SILENT datahub ends the pass instead of parking the thread**: the status names the
//!    timeout, the last good chains stay, and the next pass re-dials.

use std::collections::BTreeMap;
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use vike_datahub_client::catalog::{CatalogListing, CatalogOutcome};
use vike_datahub_client::dial::DatahubDial;
use vike_datahub_client::options::{OptionsChainOutcome, OptionsChainReply};
use vike_datahub_client::proto::{Request, Response};
use vike_datahub_client::test_support::accept_keyless;
use vike_datahub_client::tool_feeds::poll::options::{OptionsLink, OptionsPollState};
use vike_datahub_client::tool_feeds::poll::{DatahubPollSnapshot, DatahubPoller};
use vike_datahub_client::{FEATURE_OPTIONS_CHAIN, FEATURE_VENUE_CATALOG};
use vike_hub_protocol::frame::{read_frame, write_frame};
use vike_marketdata::{
    OptionChain, OptionChainExpiry, OptionChainSet, OptionKind, OptionUnderlyingKind,
};
use vike_model::{Contract, ContractOption, Instrument};

const FRONT: &str = "2026-06-26";

fn option(name: &str, underlying: &str) -> Instrument {
    Instrument::new(
        "deribit",
        name,
        underlying,
        underlying,
        Contract::Option(ContractOption {
            right: OptionKind::Call,
            strike: 100.0,
            expiry_ms: vike_options::expiry_ms(FRONT),
            style: vike_model::ContractOptionExerciseStyle::European,
            margining: vike_model::Margining::Inverse,
            settle: underlying.to_string(),
            value_currency: Some(underlying.to_string()),
            underlying: Some(underlying.to_string()),
        }),
    )
}

/// The front-expiry bundle the double answers for `underlying`.
fn front_bundle(underlying: &str) -> OptionChainSet {
    let expiry = OptionChainExpiry { date: FRONT.into(), dte: 1, label: "26 Jun".into() };
    let chain = OptionChain {
        underlying: underlying.into(),
        underlying_kind: OptionUnderlyingKind::Crypto,
        underlying_price: Some(100.0),
        expiry: expiry.clone(),
        asof_ms: 0,
        source: "deribit".into(),
        rows: Vec::new(),
    };
    OptionChainSet {
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
    /// The half-open shape: the request arrives, nothing comes back, the socket stays OPEN (no FIN)
    /// until the test switches the answer away, when that connection closes.
    Silent,
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

    /// A key-less dial of the double (it opens every connection key-less).
    fn dial(&self) -> DatahubDial {
        DatahubDial { address: self.addr.clone(), keys: None, epoch: 0 }
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
    if !accept_keyless(&mut w, features) {
        return;
    }
    while let Ok(req) = read_frame::<_, Request>(&mut r) {
        let response = match req {
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
                if *answer.lock().unwrap() == Answer::Silent {
                    while *answer.lock().unwrap() == Answer::Silent {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    return;
                }
                let answer = *answer.lock().unwrap();
                // The served set rides on every answer; a server serving the venue nothing names none.
                let served_underlyings = match answer {
                    Answer::Chains | Answer::Silent => vec!["BTC".to_string(), "ETH".to_string()],
                    Answer::NotArmed => Vec::new(),
                };
                let outcome = match answer {
                    Answer::Chains | Answer::Silent => {
                        OptionsChainOutcome::Chains(front_bundle(&underlying))
                    }
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
        if write_frame(&mut w, &response).is_err() {
            return;
        }
    }
}

/// The tool's shared data with a live link, and one window showing the default underlying.
fn open_window() -> Mutex<DatahubPollSnapshot> {
    let (tx, _rx) = std::sync::mpsc::channel();
    let td = DatahubPollSnapshot {
        options_link: OptionsLink::new(tx),
        ..DatahubPollSnapshot::default()
    };
    td.options_link.want(None, None);
    Mutex::new(td)
}

#[test]
fn one_pass_fills_the_tool_and_the_second_reuses_the_connection() {
    let hub = FakeHub::spawn(
        vec![FEATURE_VENUE_CATALOG.to_string(), FEATURE_OPTIONS_CHAIN.to_string()],
        Answer::Chains,
    );
    let shared = open_window();
    let mut poller = DatahubPoller::<OptionsPollState>::default();
    let t0 = Instant::now();

    assert!(poller.run_once(&shared, Some(&hub.dial()), t0), "a live pick is asked for");
    {
        let td = shared.lock().unwrap();
        assert_eq!(td.options_index.len(), 2, "the deribit listing became the index");
        assert_eq!(td.options_underlyings, ["BTC", "ETH"], "the shown one and the background one");
        assert!(td.options_status.starts_with("LIVE"), "{}", td.options_status);
        assert!(td.options_status.contains(&hub.addr), "{}", td.options_status);
    }
    assert_eq!(hub.count(|c| &c.catalog), 1);
    assert_eq!(hub.count(|c| &c.chains), 2, "BTC (shown) and ETH (background)");
    assert_eq!(hub.count(|c| &c.connections), 2, "one for the listing, one held for the chains");

    shared.lock().unwrap().options_link.want(None, None);
    assert!(poller.run_once(&shared, Some(&hub.dial()), t0 + Duration::from_secs(1)));
    assert_eq!(hub.count(|c| &c.chains), 3, "one second later: the shown underlying only");
    assert_eq!(hub.count(|c| &c.catalog), 1, "a populated index is not refetched");
    assert_eq!(hub.count(|c| &c.connections), 2, "the chain connection is HELD across passes");
}

#[test]
fn an_old_server_is_sent_no_options_frame_and_says_so() {
    let hub = FakeHub::spawn(vec![FEATURE_VENUE_CATALOG.to_string()], Answer::Chains);
    let shared = open_window();
    let mut poller = DatahubPoller::<OptionsPollState>::default();
    assert!(poller.run_once(&shared, Some(&hub.dial()), Instant::now()));
    let status = shared.lock().unwrap().options_status.clone();
    assert!(status.contains(FEATURE_OPTIONS_CHAIN), "the missing verb is named: {status}");
    assert!(status.contains("nothing was sent"), "{status}");
    assert_eq!(hub.count(|c| &c.chains), 0, "not one OptionsChain frame may be written");
    assert!(shared.lock().unwrap().options_by_underlying.is_empty(), "and nothing was invented");
}

#[test]
fn a_refusal_is_the_servers_sentence_and_keeps_the_last_good_chains() {
    let hub = FakeHub::spawn(
        vec![FEATURE_VENUE_CATALOG.to_string(), FEATURE_OPTIONS_CHAIN.to_string()],
        Answer::Chains,
    );
    let shared = open_window();
    let mut poller = DatahubPoller::<OptionsPollState>::default();
    let t0 = Instant::now();
    assert!(poller.run_once(&shared, Some(&hub.dial()), t0));
    let seeded = shared.lock().unwrap().options_by_underlying.clone();
    assert!(seeded.contains_key("BTC"));

    // The SAME server's operator turns its options lane off.
    hub.answer_with(Answer::NotArmed);
    shared.lock().unwrap().options_link.want(None, None);
    assert!(poller.run_once(&shared, Some(&hub.dial()), t0 + Duration::from_secs(1)));
    let td = shared.lock().unwrap();
    let expected = OptionsChainReply {
        venue: "deribit".into(),
        underlying: "BTC".into(),
        served_underlyings: Vec::new(),
        outcome: OptionsChainOutcome::NotArmed,
    }
    .describe();
    assert_eq!(td.options_status, expected, "the SERVER's own words");
    assert_eq!(td.options_by_underlying, seeded, "a refusal never replaces the last good chains");
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
    let mut poller = DatahubPoller::<OptionsPollState>::default();
    let t0 = Instant::now();
    assert!(poller.run_once(&shared, Some(&old.dial()), t0));
    assert!(!shared.lock().unwrap().options_by_underlying.is_empty());

    let new = FakeHub::spawn(
        vec![FEATURE_VENUE_CATALOG.to_string(), FEATURE_OPTIONS_CHAIN.to_string()],
        Answer::NotArmed,
    );
    shared.lock().unwrap().options_link.want(None, None);
    assert!(poller.run_once(&shared, Some(&new.dial()), t0 + Duration::from_secs(1)));
    let td = shared.lock().unwrap();
    assert!(td.options_by_underlying.is_empty(), "the old server's chains are not last good here");
    assert!(td.options_underlyings.is_empty(), "nor its selector");
    assert!(!td.options_status.contains(&old.addr), "nor its status: {}", td.options_status);
    assert_eq!(new.count(|c| &c.catalog), 1, "the index is asked of the NEW server");
    assert_eq!(new.count(|c| &c.connections), 2, "its listing, and the chain connection");
}

/// **The half-open link.** The held connection's peer stops answering mid-pass without closing:
/// each pass ends within the (test-sized) reply bound, the chains the last answer carried stay on
/// screen, and every pass after a failure dials a NEW connection. A timed-out reply is a failure of
/// the LINK, so the first two passes say only that the poll is reconnecting and the third names the
/// timeout.
#[test]
fn a_silent_datahub_ends_the_pass_keeps_last_good_and_the_next_pass_redials() {
    let hub = FakeHub::spawn(
        vec![FEATURE_VENUE_CATALOG.to_string(), FEATURE_OPTIONS_CHAIN.to_string()],
        Answer::Chains,
    );
    let shared = open_window();
    let mut poller =
        DatahubPoller::<OptionsPollState>::with_reply_timeout(Duration::from_millis(200));
    let t0 = Instant::now();
    assert!(poller.run_once(&shared, Some(&hub.dial()), t0));
    let seeded = shared.lock().unwrap().options_by_underlying.clone();
    assert!(seeded.contains_key("BTC"));
    assert_eq!(hub.count(|c| &c.connections), 2, "the listing, and the held chain connection");

    hub.answer_with(Answer::Silent);
    // Half-second steps: a pick is stamped with the REAL clock and lives `INTEREST_TTL`, so every
    // simulated pass, the live one below included, must stay inside it.
    for pass in 1..=3_u64 {
        shared.lock().unwrap().options_link.want(None, None);
        let started = Instant::now();
        let at = t0 + Duration::from_millis(500 * pass);
        assert!(poller.run_once(&shared, Some(&hub.dial()), at));
        let waited = started.elapsed();
        assert!(
            waited < Duration::from_secs(10),
            "the pass must not park on a silent peer: {waited:?}"
        );
        let td = shared.lock().unwrap();
        assert!(!td.options_status.starts_with("LIVE"), "{}", td.options_status);
        assert_eq!(
            td.options_by_underlying, seeded,
            "a dead link never replaces the last good chains"
        );
        if pass < 3 {
            assert!(
                td.options_status.starts_with("reconnecting"),
                "pass {pass}: {}",
                td.options_status
            );
        } else {
            assert!(
                td.options_status.contains("no reply byte for 200ms"),
                "names it: {}",
                td.options_status
            );
            assert!(
                td.options_status.contains(&hub.addr),
                "and the datahub: {}",
                td.options_status
            );
        }
    }

    hub.answer_with(Answer::Chains);
    shared.lock().unwrap().options_link.want(None, None);
    assert!(poller.run_once(&shared, Some(&hub.dial()), t0 + Duration::from_secs(2)));
    assert_eq!(
        hub.count(|c| &c.connections),
        5,
        "the listing, then each dead chain connection dropped and the next pass re-dialled"
    );
    let status = shared.lock().unwrap().options_status.clone();
    assert!(status.starts_with("LIVE"), "answered again over the new connection: {status}");
}

#[test]
fn with_no_window_open_nothing_is_dialled() {
    let hub = FakeHub::spawn(
        vec![FEATURE_VENUE_CATALOG.to_string(), FEATURE_OPTIONS_CHAIN.to_string()],
        Answer::Chains,
    );
    let (tx, _rx) = std::sync::mpsc::channel();
    let shared = Mutex::new(DatahubPollSnapshot {
        options_link: OptionsLink::new(tx),
        ..DatahubPollSnapshot::default()
    });
    let mut poller = DatahubPoller::<OptionsPollState>::default();
    assert!(!poller.run_once(&shared, Some(&hub.dial()), Instant::now()));
    assert_eq!(hub.count(|c| &c.connections), 0);
}

#[test]
fn with_no_backend_the_status_says_so_and_nothing_is_asked() {
    let shared = open_window();
    let mut poller = DatahubPoller::<OptionsPollState>::default();
    assert!(poller.run_once(&shared, None, Instant::now()));
    let status = shared.lock().unwrap().options_status.clone();
    assert!(status.contains("config.datahub_addr"), "{status}");
    assert!(!status.starts_with("LIVE"), "{status}");
}
