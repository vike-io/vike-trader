//! Feature negotiation for the VENUE READ verb (`vike-cli trade venue`): the client REFUSES
//! CLIENT-SIDE, with nothing on the wire after the handshake, against a node whose `Welcome.features`
//! does not advertise [`FEATURE_VENUE_READ`], and round-trips both reads against one that does.
//!
//! Why a feature string and not a `NODE_PROTO_VERSION` bump: the version is folded into the signed auth
//! MAC, so a bump breaks the handshake against every running node. The refusal assertion is "the node saw
//! NOTHING" (`support/fake_node.rs` records it), not merely "the call errored".

#[path = "support/fake_node.rs"]
mod fake_node;

use std::net::SocketAddr;
use std::sync::mpsc;

use vike_tradehub_client::proto::{
    FEATURE_DIRECTORY, FEATURE_VENUE_READ, FEATURE_VENUE_READ_POSITIONS, Request, Response,
};
use vike_tradehub_client::venue_read;
use vike_tradehub_client::wire::{
    WireVenueAllowance, WireVenueBalance, WireVenueOpenOrder, WireVenuePosition, WireVenueRead,
    WireVenueReadKind,
};

/// The one key the scripted node accepts (scope separation is proven in
/// `crates/vike-tradehub/tests/daemon/venue_read.rs`).
const KEY: &[u8] = b"venue-read-negotiation-test-key";

fn canned_balance() -> WireVenueRead {
    WireVenueRead::Balance(WireVenueBalance {
        asset: "pUSD".into(),
        balance: 20.0,
        allowances: vec![WireVenueAllowance {
            spender: "0xE111180000d2663C0091e4f400237545B87B996B".into(),
            name: "CTF exchange".into(),
            amount: 0.0,
            unlimited: true,
        }],
    })
}

fn canned_orders() -> WireVenueRead {
    WireVenueRead::OpenOrders {
        orders: vec![WireVenueOpenOrder {
            venue_order_id: "0xabc".into(),
            client_order_id: None,
            symbol: "6710259892".into(),
            side: 1,
            price: Some(0.2),
            qty: 5.0,
            filled_qty: 0.0,
            status: "ACCEPTED".into(),
            ts_ms: 1_700_000_000_000,
        }],
        truncated: false,
    }
}

fn canned_positions() -> WireVenueRead {
    WireVenueRead::Positions {
        positions: vec![WireVenuePosition {
            symbol: "1484".into(),
            title: "Solana Up or Down".into(),
            outcome: "Down".into(),
            qty: 11.11,
            avg_price: Some(0.2099),
            price: Some(0.0),
            value: Some(0.0),
            redeemable: true,
        }],
        truncated: false,
    }
}

/// A scripted node advertising `features` that answers each kind with its canned payload, and sends the
/// request it was asked on `asked`.
fn scripted_node(
    features: Vec<String>,
) -> (SocketAddr, mpsc::Receiver<Vec<String>>, mpsc::Receiver<Request>) {
    let (asked_tx, asked_rx) = mpsc::channel();
    let (addr, report) = fake_node::scripted_node(KEY, features, move |req| {
        let reply = match &req {
            Request::VenueRead { kind: WireVenueReadKind::Balance, .. } => {
                Response::VenueRead(Box::new(canned_balance()))
            }
            Request::VenueRead { kind: WireVenueReadKind::OpenOrders, .. } => {
                Response::VenueRead(Box::new(canned_orders()))
            }
            Request::VenueRead { kind: WireVenueReadKind::Positions, .. } => {
                Response::VenueRead(Box::new(canned_positions()))
            }
            other => panic!("unscripted post-auth request: {other:?}"),
        };
        asked_tx.send(req).expect("the test is listening");
        ("VenueRead".to_string(), reply)
    });
    (addr, report, asked_rx)
}

/// An OLD node: everything EXCEPT `venue-read`. It still carries the directory read, so the refusal below
/// checks THIS capability and not a neighbour's.
fn old_node_features() -> Vec<String> {
    fake_node::features(&["observe", "subscribe", "preview", "strategy-verbs", FEATURE_DIRECTORY])
}

/// ⚠ THE REFUSAL: `venue_read` against a node that does not advertise the feature errors
/// [`std::io::ErrorKind::Unsupported`] naming the capability, and the node received NOTHING after the
/// handshake: what a caller reads as "this node predates the venue read".
#[test]
fn a_venue_read_is_refused_client_side_against_an_old_node() {
    let (addr, report, asked) = scripted_node(old_node_features());
    let err = venue_read(addr, KEY, "polymarket", "DEFAULT", WireVenueReadKind::Balance)
        .expect_err("must refuse client-side");
    assert_eq!(err.kind(), std::io::ErrorKind::Unsupported);
    assert!(
        err.to_string().contains(FEATURE_VENUE_READ),
        "the refusal names the missing capability: {err}"
    );
    let seen = report.recv().expect("scripted node reports");
    assert!(seen.is_empty(), "NOTHING may be sent after auth on a refusal, saw: {seen:?}");
    assert!(asked.try_recv().is_err(), "the node was asked nothing");
}

/// …and against a node that DOES advertise it, the same call carries the venue, the account and the kind
/// exactly, and each kind round-trips its canned payload.
#[test]
fn both_reads_round_trip_when_advertised() {
    let mut features = old_node_features();
    features.push(FEATURE_VENUE_READ.into());
    for (kind, want) in [
        (WireVenueReadKind::Balance, canned_balance()),
        (WireVenueReadKind::OpenOrders, canned_orders()),
    ] {
        let (addr, report, asked) = scripted_node(features.clone());
        let got = venue_read(addr, KEY, "polymarket", "ALT", kind).expect("advertised ⇒ served");
        assert_eq!(got, want);
        assert_eq!(got.kind(), kind);
        assert_eq!(report.recv().expect("report"), vec!["VenueRead".to_string()]);
        assert_eq!(
            asked.recv().expect("the node was asked"),
            Request::VenueRead { venue: "polymarket".into(), account: "ALT".into(), kind }
        );
    }
}

/// ⚠ THE POSITIONS REFUSAL: a node from the build that has `venue-read` but not the positions kind cannot
/// decode the kind's name, so against it `venue_read(Positions)` errors [`std::io::ErrorKind::Unsupported`]
/// naming `venue-read-positions` and the node receives NOTHING; the two older kinds still go to the same
/// node. Against a node that advertises the new word, positions round-trips.
#[test]
fn positions_owes_its_own_word_and_is_refused_client_side_without_it() {
    let mut older = old_node_features();
    older.push(FEATURE_VENUE_READ.into());
    let (addr, report, asked) = scripted_node(older.clone());
    let err = venue_read(addr, KEY, "polymarket", "DEFAULT", WireVenueReadKind::Positions)
        .expect_err("must refuse client-side");
    assert_eq!(err.kind(), std::io::ErrorKind::Unsupported);
    assert!(err.to_string().contains(FEATURE_VENUE_READ_POSITIONS), "names the word: {err}");
    let seen = report.recv().expect("scripted node reports");
    assert!(seen.is_empty(), "NOTHING may be sent after auth on a refusal, saw: {seen:?}");
    assert!(asked.try_recv().is_err(), "the node was asked nothing");

    let (addr, _report, _asked) = scripted_node(older.clone());
    let balance = venue_read(addr, KEY, "polymarket", "DEFAULT", WireVenueReadKind::Balance);
    assert_eq!(balance.expect("the older kinds are unaffected"), canned_balance());

    older.push(FEATURE_VENUE_READ_POSITIONS.into());
    let (addr, report, asked) = scripted_node(older);
    let got = venue_read(addr, KEY, "polymarket", "ALT", WireVenueReadKind::Positions)
        .expect("advertised ⇒ served");
    assert_eq!(got, canned_positions());
    assert_eq!(got.kind(), WireVenueReadKind::Positions);
    assert_eq!(report.recv().expect("report"), vec!["VenueRead".to_string()]);
    assert_eq!(
        asked.recv().expect("the node was asked"),
        Request::VenueRead {
            venue: "polymarket".into(),
            account: "ALT".into(),
            kind: WireVenueReadKind::Positions
        }
    );
}

/// A node that advertises the feature and refuses the read in words (`not offered by <venue>`, the account
/// is not running there, asked too soon) surfaces as [`std::io::ErrorKind::InvalidData`] carrying the node's
/// sentence: the node ANSWERED, and the text is the operator's.
#[test]
fn a_node_refusal_is_invalid_data_with_the_nodes_words() {
    let sentence = "not offered by binance: its bridge has no account-wide balance read";
    let (addr, _) = fake_node::scripted_node(KEY, vec![FEATURE_VENUE_READ.into()], move |req| {
        let Request::VenueRead { .. } = req else {
            panic!("expected VenueRead");
        };
        ("VenueRead".to_string(), Response::Error(sentence.into()))
    });
    let err = venue_read(addr, KEY, "binance", "DEFAULT", WireVenueReadKind::Balance)
        .expect_err("a node refusal must surface");
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    assert!(err.to_string().contains(sentence), "carries the node's words: {err}");
}

/// The node's mirror decodes the gatewayhub's OWN golden answers (`fixtures/gatewayhub_wire`'s
/// `VenueReadReply` rows), field for field: the two vocabularies cannot drift apart unseen. Read at run
/// time and skipped LOUDLY when the fixture is not there (a packaged crate has no repo around it).
#[test]
fn the_mirror_decodes_the_gatewayhubs_golden_answers() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/gatewayhub_wire/v1_trade_down.jsonl");
    let Ok(text) = std::fs::read_to_string(&path) else {
        eprintln!("SKIPPED: {} is not readable here", path.display());
        return;
    };
    let mut decoded = 0;
    for line in text.lines().filter(|l| l.contains("\"VenueReadReply\"")) {
        let frame: serde_json::Value = serde_json::from_str(line).expect("a golden line is JSON");
        let answer = &frame["VenueReadReply"]["answer"];
        let Some(ok) = answer.get("Ok") else { continue };
        let read: WireVenueRead =
            serde_json::from_value(ok.clone()).expect("the mirror decodes the golden answer");
        // …and writes it back byte for byte, so the field ORDER is the gatewayhub's too.
        assert_eq!(serde_json::to_value(&read).unwrap(), *ok);
        decoded += 1;
    }
    assert!(
        decoded >= 3,
        "the fixture holds a balance, an open-orders and a positions answer, found {decoded}"
    );
}
