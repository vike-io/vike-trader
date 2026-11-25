//! **One poll, two halves, the same asks** (D57): `OptionsPoller` folded into [`DatahubPoller`], and
//! each half still asks the verbs it asked before, at the cadence it asked them at, on a connection
//! of its own. A recording datahub double answers every verb and writes down what was asked.

use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::NaiveDate;
use vike_hub_protocol::handshake::{HandshakeFrame, HandshakeReply};
use vike_marketdata::OptionKind;
use vike_options::test_support::test_option;

use super::connection::{FEEDS_REPLY_TIMEOUT, OPTIONS_REPLY_TIMEOUT};
use super::options::{OPTIONS_POLL_MS, OptionsPollState};
use super::*;
use crate::catalog::{CatalogListing, CatalogOutcome};
use crate::options::{OptionsChainOutcome, OptionsChainReply};
use crate::proto::{Request, Response, request_kind};
use crate::tool_feeds::{CalendarEconomicReply, CalendarEquityReply, NewsReply};
use crate::{FEATURE_OPTIONS_CHAIN, FEATURE_TOOL_FEEDS, FEATURE_VENUE_CATALOG, PROTO_VERSION};
use vike_hub_protocol::frame::{read_frame, write_frame};

const NOW_MS: i64 = 1_760_000_000_000;

/// A key-less datahub double serving every verb both halves ask: it counts connections and
/// records each request as its verb (and the week, venue or underlying it names).
struct RecordingHub {
    addr: String,
    connections: Arc<AtomicUsize>,
    asked: Arc<Mutex<Vec<String>>>,
}

impl RecordingHub {
    fn spawn() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().expect("local_addr").to_string();
        let connections = Arc::new(AtomicUsize::new(0));
        let asked = Arc::new(Mutex::new(Vec::new()));
        let (count, record) = (Arc::clone(&connections), Arc::clone(&asked));
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { break };
                count.fetch_add(1, Ordering::SeqCst);
                let record = Arc::clone(&record);
                std::thread::spawn(move || serve(stream, &record));
            }
        });
        Self { addr, connections, asked }
    }

    fn dial(&self) -> DatahubDial {
        DatahubDial { address: self.addr.clone(), keys: None, epoch: 0 }
    }

    /// What was asked since the last call.
    fn take(&self) -> Vec<String> {
        std::mem::take(&mut *self.asked.lock().unwrap())
    }
}

/// An empty snapshot at generation 1: the feed is due again after [`POLL_EVERY`].
fn empty<T>() -> DatahubPollOutcome<T> {
    DatahubPollOutcome::Snapshot {
        rows: Vec::new(),
        fetched_at_ms: NOW_MS,
        generation: 1,
        sources_ok: 1,
        sources_total: 1,
        cadence_secs: 0,
    }
}

fn serve(stream: TcpStream, asked: &Mutex<Vec<String>>) {
    let mut r = stream.try_clone().expect("clone the accepted stream");
    let mut w = stream;
    if !matches!(read_frame::<_, HandshakeFrame>(&mut r), Ok(HandshakeFrame::Hello { .. })) {
        return;
    }
    let features = [FEATURE_TOOL_FEEDS, FEATURE_OPTIONS_CHAIN, FEATURE_VENUE_CATALOG]
        .map(str::to_string)
        .to_vec();
    let welcome = HandshakeReply::Welcome { proto_version: PROTO_VERSION, features, nonce: None };
    if write_frame(&mut w, &welcome).is_err() {
        return;
    }
    while let Ok(req) = read_frame::<_, Request>(&mut r) {
        let verb = request_kind(&req);
        let (seen, response) = match req {
            Request::NewsFeed { .. } => {
                (verb.to_string(), Response::NewsFeed(NewsReply { outcome: empty() }))
            }
            Request::EconomicCalendar { .. } => (
                verb.to_string(),
                Response::EconomicCalendar(CalendarEconomicReply { outcome: empty() }),
            ),
            Request::EquityCalendar { week_start, .. } => (
                format!("{verb} {week_start}"),
                Response::EquityCalendar(CalendarEquityReply {
                    earnings: empty(),
                    dividends: empty(),
                    ipos: empty(),
                }),
            ),
            Request::VenueCatalog { venue } => {
                let instruments = vec![test_option(
                    "BTC-26JUN26-100000-C",
                    "BTC",
                    "2026-06-26",
                    100_000.0,
                    OptionKind::Call,
                    "BTC",
                )];
                (
                    format!("{verb} {venue}"),
                    Response::VenueCatalog(CatalogListing {
                        venue,
                        outcome: CatalogOutcome::Listed {
                            instruments,
                            truncated: false,
                            cached: false,
                        },
                    }),
                )
            }
            Request::OptionsChain { venue, underlying, .. } => (
                format!("{verb} {venue} {underlying}"),
                Response::OptionsChain(OptionsChainReply {
                    venue,
                    underlying,
                    served_underlyings: vec!["BTC".to_string()],
                    outcome: OptionsChainOutcome::NotArmed,
                }),
            ),
            other => (verb.to_string(), Response::Error(format!("unexpected verb: {other:?}"))),
        };
        asked.lock().unwrap().push(seen);
        if write_frame(&mut w, &response).is_err() {
            return;
        }
    }
}

/// The cadences the two polls ran at before the collapse, unchanged by it.
#[test]
fn the_two_halves_keep_their_cadences() {
    assert_eq!(POLL_EVERY, Duration::from_secs(60), "one News and Calendar ask a minute");
    assert_eq!(FEEDS_TICK, Duration::from_secs(5), "the feeds thread looks every five seconds");
    assert_eq!(WARMING_RETRY, Duration::from_secs(5), "a warming feed is asked again in five");
    assert_eq!(OPTIONS_POLL_MS, 1_000, "one Options pass a second");
    assert_eq!(FEEDS_REPLY_TIMEOUT, Duration::from_secs(20));
    assert_eq!(OPTIONS_REPLY_TIMEOUT, Duration::from_secs(10));
}

/// **Each half asks its own verbs, at its own cadence, on its own connection.** The News and
/// Calendar half asks `NewsFeed`, `EconomicCalendar` and `EquityCalendar` (the viewer's Monday)
/// and nothing again inside its minute; the Options half asks the deribit `VenueCatalog` listing
/// for its index, then `OptionsChain` for the underlying it names, and the chain again on its next
/// pass a second later. Neither asks the other's verbs.
#[test]
fn both_halves_ask_the_verbs_they_asked_before_at_the_same_cadence() {
    let hub = RecordingHub::spawn();
    let dial = hub.dial();
    let shared = Mutex::new(DatahubPollSnapshot::default());
    {
        let td = shared.lock().unwrap();
        td.feeds_link.want(DatahubPollInterest::News);
        td.feeds_link.want(DatahubPollInterest::Calendar);
        td.options_link.want(None, None);
    }
    let t0 = Instant::now();
    let clock = |after: Duration| DatahubPollClock {
        now: t0 + after,
        now_ms: NOW_MS,
        today: NaiveDate::from_ymd_opt(2026, 10, 14).unwrap(),
    };
    let feeds_verbs = ["NewsFeed", "EconomicCalendar", "EquityCalendar 2026-10-12"];

    let mut feeds = DatahubPoller::<DatahubPollState>::default();
    assert!(feeds.run_once(&shared, Some(&dial), &clock(Duration::ZERO)));
    assert_eq!(hub.take(), feeds_verbs, "the News and Calendar verbs");
    assert!(
        !feeds.run_once(&shared, Some(&dial), &clock(POLL_EVERY - FEEDS_TICK)),
        "nothing is due inside the minute"
    );
    assert!(hub.take().is_empty());
    assert!(feeds.run_once(&shared, Some(&dial), &clock(POLL_EVERY)));
    assert_eq!(hub.take(), feeds_verbs, "asked again a minute later");

    let mut options = DatahubPoller::<OptionsPollState>::default();
    assert!(options.run_once(&shared, Some(&dial), t0));
    assert_eq!(
        hub.take(),
        ["VenueCatalog deribit", "OptionsChain deribit BTC"],
        "the index's listing, then the chain"
    );
    assert!(options.run_once(&shared, Some(&dial), t0 + Duration::from_millis(OPTIONS_POLL_MS)));
    assert_eq!(hub.take(), ["OptionsChain deribit BTC"], "a second later: the chain again");

    assert_eq!(
        hub.connections.load(Ordering::SeqCst),
        3,
        "the feeds half's held connection, the listing's own dial and the options half's held one"
    );
}
