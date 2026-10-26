//! The CAPABILITY NEGOTIATION of the three tool-feed verbs — [`Request::NewsFeed`],
//! [`Request::EconomicCalendar`] and [`Request::EquityCalendar`], negotiated by ONE string,
//! [`FEATURE_TOOL_FEEDS`]. The model is `crates/vike-datahub-client/tests/options_chain_negotiation.rs`,
//! and the legs:
//!
//! - **a server without the capability** — all three are refused CLIENT-SIDE by name, nothing sent,
//!   no fallback;
//! - **a malformed week** — refused CLIENT-SIDE by the validator the server's door runs, nothing sent;
//! - **an answering server** — each request reaches the wire with the generation the caller held,
//!   and every outcome of the reply comes back as the value it was;
//! - **a desynced peer** — an answer of the wrong variant is an `Err` naming both.
//!
//! The legs that need the REAL server (an unarmed build answering `NotArmed`, the loopback handler)
//! live with the handler in `vike-datahub`. Loopback + in-memory only.

#[path = "support/fake_peer.rs"]
mod fake_peer;

use std::assert_matches;

use vike_datahub_client::DatahubClient;
use vike_datahub_client::proto::{FEATURE_TOOL_FEEDS, Request, Response};
use vike_datahub_client::tool_feeds::{
    EconomicCalendarReply, EquityCalendarReply, NewsReply, ToolFeedOutcome, ToolFeedRefusal,
};

use fake_peer::spawn_fake_peer;

/// Leg 1: a server that does not advertise the capability is refused BEFORE a frame is written,
/// naming the capability — an old server is refused by name, with no fallback.
#[test]
fn a_server_without_the_capability_is_refused_client_side_without_sending() {
    let (addr, rx, handle) =
        spawn_fake_peer(vec!["load_bars".to_string(), "options_chain".to_string()], |_| None);
    let mut client = DatahubClient::connect(addr).expect("handshake succeeds — same version");
    assert!(!client.supports_tool_feeds());
    let errors = [
        client.news_feed(None).expect_err("refused client-side"),
        client.economic_calendar(Some(3)).expect_err("refused client-side"),
        client.equity_calendar("2026-10-12", None).expect_err("refused client-side"),
    ];
    for err in errors {
        assert!(err.contains(FEATURE_TOOL_FEEDS), "names the capability: {err}");
        assert!(err.contains("nothing was sent"), "{err}");
    }
    drop(client);
    let received = rx.recv().expect("fake reports");
    assert!(received.is_empty(), "the client SENT {received:?} after a local refusal");
    handle.join().expect("fake joins");
}

/// Leg 2: a malformed week is refused locally by the validator the server's door runs, so a server
/// that DOES advertise the verbs still receives nothing.
#[test]
fn a_malformed_week_is_refused_client_side_without_sending() {
    let (addr, rx, handle) = spawn_fake_peer(vec![FEATURE_TOOL_FEEDS.to_string()], |_| None);
    let mut client = DatahubClient::connect(addr).expect("handshake");
    assert!(client.supports_tool_feeds());
    for bad in ["", "2026-9-012", "2026/10/12", "2026-02-30"] {
        let err = client.equity_calendar(bad, None).expect_err("malformed");
        assert!(err.contains("week_start"), "{err}");
    }
    drop(client);
    let received = rx.recv().expect("fake reports");
    assert!(received.is_empty(), "the client SENT {received:?} after a local refusal");
    handle.join().expect("fake joins");
}

/// Leg 3: an advertising server is asked, with the held generation on the wire, and its outcomes
/// arrive as sent — the cheap-poll `Unchanged`, a keyed feed's `NotConfigured` beside a served
/// keyless one, and a refused week.
#[test]
fn each_verb_reaches_the_wire_with_its_generation_and_the_outcome_comes_back_whole() {
    let (addr, rx, handle) = spawn_fake_peer(vec![FEATURE_TOOL_FEEDS.to_string()], |r| match r {
        Request::NewsFeed { .. } => Some(Response::NewsFeed(NewsReply {
            outcome: ToolFeedOutcome::Unchanged { generation: 7 },
        })),
        Request::EconomicCalendar { .. } => {
            Some(Response::EconomicCalendar(EconomicCalendarReply {
                outcome: ToolFeedOutcome::NotArmed,
            }))
        }
        Request::EquityCalendar { .. } => Some(Response::EquityCalendar(EquityCalendarReply {
            earnings: ToolFeedOutcome::NotConfigured { key_name: "FINNHUB_API_KEY".to_string() },
            dividends: ToolFeedOutcome::Warming,
            ipos: ToolFeedOutcome::Refused(ToolFeedRefusal::WeekOutOfRange {
                allowed_from: "2026-10-05".to_string(),
                allowed_to: "2026-10-19".to_string(),
            }),
        })),
        _ => None,
    });
    let mut client = DatahubClient::connect(addr).expect("handshake");
    let news = client.news_feed(Some(7)).expect("answered");
    assert_eq!(news.outcome, ToolFeedOutcome::Unchanged { generation: 7 });
    let econ = client.economic_calendar(None).expect("answered");
    assert_eq!(econ.outcome, ToolFeedOutcome::NotArmed);
    let equity = client.equity_calendar("2026-10-12", Some(2)).expect("answered");
    assert_matches!(&equity.earnings, ToolFeedOutcome::NotConfigured { key_name } if key_name == "FINNHUB_API_KEY");
    assert_eq!(equity.dividends, ToolFeedOutcome::Warming);
    assert_matches!(equity.ipos, ToolFeedOutcome::Refused(_));
    drop(client);
    let received = rx.recv().expect("fake reports");
    assert_eq!(received.len(), 3, "exactly the three requests were sent: {received:?}");
    assert_matches!(&received[0], Request::NewsFeed { if_generation: Some(7) });
    assert_matches!(&received[1], Request::EconomicCalendar { if_generation: None });
    assert_matches!(
        &received[2],
        Request::EquityCalendar { week_start, if_generation: Some(2) } if week_start == "2026-10-12"
    );
    handle.join().expect("fake joins");
}

/// Leg 4: a peer that answers a verb with the WRONG variant is a desync, reported by name — never
/// read as an empty feed.
#[test]
fn a_reply_of_the_wrong_variant_is_a_named_desync() {
    let (addr, _rx, handle) =
        spawn_fake_peer(vec![FEATURE_TOOL_FEEDS.to_string()], |_| Some(Response::Pong));
    let mut client = DatahubClient::connect(addr).expect("handshake");
    let err = client.news_feed(None).expect_err("desync");
    assert!(err.contains("protocol desync: expected NewsFeed, got Pong"), "{err}");
    drop(client);
    handle.join().expect("fake joins");
}

/// Leg 5: a server that answers a tool-feed request with `Response::Error` hands the message back,
/// and the client stays usable.
#[test]
fn a_server_error_is_the_error_and_the_client_stays_usable() {
    let (addr, _rx, handle) = spawn_fake_peer(vec![FEATURE_TOOL_FEEDS.to_string()], |r| {
        matches!(r, Request::EquityCalendar { .. })
            .then(|| Response::Error("week_start rejected".to_string()))
    });
    let mut client = DatahubClient::connect(addr).expect("handshake");
    let err = client.equity_calendar("2026-10-12", None).expect_err("server error");
    assert_eq!(err, "week_start rejected");
    assert!(client.is_usable());
    drop(client);
    handle.join().expect("fake joins");
}
