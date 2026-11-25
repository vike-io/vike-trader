//! The strategy lane's SYMBOL-FACTS read (`Request::StrategySymbolFacts`, decision 0285's amendment of 2026-11-16) is
//! negotiated by `FEATURE_SYMBOL_FACTS` (decision 0112): a daemon that does not advertise it is sent NOTHING (an older
//! one could not decode the verb), and one that does is asked the exact request; its facts are the answer, and its
//! refusal is its own words.

use vike_datahub_client::DatahubClient;
use vike_datahub_client::proto::{
    FEATURE_INPUT_BARS, FEATURE_STRATEGY_LANE, FEATURE_SYMBOL_FACTS, Request, Response,
};
use vike_datahub_client::test_support::spawn_fake_peer;
use vike_hub_protocol::call::HubCallError;
use vike_model::SymbolProperties;
use vike_utility::errors::FailureOutcome;

const TOKEN: &str = "71321045679252212594626385532706912750332728571942532289631379312455583992563";

#[test]
fn a_daemon_without_the_string_is_sent_nothing() {
    let features = vec![FEATURE_STRATEGY_LANE.to_string(), FEATURE_INPUT_BARS.to_string()];
    let (addr, received, peer) = spawn_fake_peer(features, |_| None);
    let mut client = DatahubClient::connect(addr).expect("connect");
    let err =
        client.strategy_symbol_facts("polymarket", TOKEN).expect_err("refused before sending");
    assert!(
        matches!(&err, HubCallError::NotOffered { feature, .. } if feature == FEATURE_SYMBOL_FACTS),
        "{err:?}"
    );
    let err = err.to_string();
    assert!(err.contains(FEATURE_SYMBOL_FACTS) && err.contains("Nothing was sent"), "{err}");
    drop(client);
    assert!(received.recv().expect("the peer reports").is_empty(), "a frame reached the peer");
    peer.join().expect("the peer ends cleanly");
}

#[test]
fn a_daemon_with_the_string_is_asked_the_exact_request_and_answers_facts_or_words() {
    let facts = SymbolProperties { tick_size: 0.01, min_qty: 5.0, ..Default::default() };
    let answer = facts;
    let (addr, received, peer) =
        spawn_fake_peer(vec![FEATURE_SYMBOL_FACTS.to_string()], move |r| {
            let Request::StrategySymbolFacts { symbol, .. } = r else { return None };
            Some(if symbol == TOKEN {
                Response::Properties(Some(Box::new(answer)))
            } else {
                Response::Error("no facts of `2` on polymarket".into())
            })
        });
    let mut client = DatahubClient::connect(addr).expect("connect");
    assert_eq!(client.strategy_symbol_facts("polymarket", TOKEN).expect("its facts"), facts);
    let refused = client.strategy_symbol_facts("polymarket", "2").expect_err("its words");
    assert_eq!(refused.to_string(), "no facts of `2` on polymarket");
    assert!(matches!(refused, HubCallError::Refused { id: None, .. }), "the datahub's answer");
    drop(client);
    let sent = received.recv().expect("the peer reports");
    let asked = Request::StrategySymbolFacts { venue: "polymarket".into(), symbol: TOKEN.into() };
    assert_eq!(
        serde_json::to_string(&sent[0]).unwrap(),
        serde_json::to_string(&asked).unwrap(),
        "the exact request"
    );
    peer.join().expect("the peer ends cleanly");
}

/// A datahub that is not there says nothing about the symbol: the one-shot read's `Err` is a link word, never the
/// `Refused` a tradehub would remember as a verdict on a paper account's symbol.
#[test]
fn a_datahub_that_is_not_there_is_no_refusal() {
    let addr = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap();
    let keys = vike_hub_protocol::auth::HubKeys::new(Vec::new(), Vec::new())
        .with_trade(b"symbol-facts-negotiation-trade-key".to_vec());
    let (outcome, text) =
        vike_datahub_client::strat::symbol_facts(&addr.to_string(), &keys, "polymarket", TOKEN)
            .expect_err("nothing listens there");
    assert_eq!(outcome, FailureOutcome::NotSent, "{text}");
}
