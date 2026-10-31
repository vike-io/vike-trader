//! Feature negotiation for the CREDENTIAL verbs of the account plane (decision 0144's wire half):
//! `exchange_once` REFUSES CLIENT-SIDE, with nothing on the wire after the handshake, against a node
//! that does not advertise every capability the request owes, and round-trips the request against
//! one that does. The refusal assertion is "the node saw NOTHING" (`support/fake_node.rs` records
//! it), not merely "the call errored".
//!
//! Two strings, because the node that predates `credential-admin` still advertises
//! `account-verbs` and cannot decode the retire variant: the second string is the only guard.

#[path = "support/fake_node.rs"]
mod fake_node;

use std::net::SocketAddr;
use std::sync::mpsc;

use vike_hub_proto::auth::Scope;
use vike_tradehub_client::exchange_once;
use vike_tradehub_client::proto::{
    FEATURE_ACCOUNT_VERBS, FEATURE_CREDENTIAL_ADMIN, Request, Response,
};
use vike_tradehub_client::wire::{
    AccountRequest, AccountVerb, WireAccountWritten, WireCredentialsHeld,
};

/// The one key the scripted node accepts (it verifies whatever scope the client claims).
const KEY: &[u8] = b"credential-negotiation-test-key";

const NEEDS: [&str; 2] = [FEATURE_ACCOUNT_VERBS, FEATURE_CREDENTIAL_ADMIN];

/// A scripted node advertising `features`: a retire is answered `AccountWritten`, a presence read
/// `CredentialsHeld` naming its first asked name; every frame is recorded as its verb word.
fn scripted_node(features: Vec<String>) -> (SocketAddr, mpsc::Receiver<Vec<String>>) {
    fake_node::scripted_node(KEY, features, |req| match req {
        Request::Account(account) => {
            let word = account.verb.word().to_string();
            let answer = match &account.verb {
                AccountVerb::HeldCredentials { names } => {
                    Response::CredentialsHeld(Box::new(WireCredentialsHeld {
                        held: names.iter().take(1).cloned().collect(),
                    }))
                }
                _ => Response::AccountWritten(Box::new(WireAccountWritten {
                    verb: word.clone(),
                    row: None,
                    changed: true,
                    note: None,
                })),
            };
            (format!("{word} confirm={}", account.confirm.as_deref().unwrap_or("-")), answer)
        }
        other => panic!("unscripted post-auth request: {other:?}"),
    })
}

fn retire(key: &str) -> Request {
    Request::Account(Box::new(AccountRequest {
        verb: AccountVerb::RetireCredential { key: key.into() },
        confirm: Some(key.into()),
    }))
}

/// The pre-`credential-admin` node: it advertises `account-verbs` and nothing newer.
#[test]
fn a_node_without_the_credential_capability_is_sent_nothing() {
    let (addr, report) = scripted_node(fake_node::features(&["observe", FEATURE_ACCOUNT_VERBS]));
    let err = exchange_once(addr, KEY, Scope::Write, &NEEDS, &retire("SOME_KEY"))
        .expect_err("an unadvertised capability must be refused client-side");
    assert_eq!(err.kind(), std::io::ErrorKind::Unsupported, "{err}");
    assert!(err.to_string().contains(FEATURE_CREDENTIAL_ADMIN), "names the capability: {err}");
    let seen = report.recv().expect("the node reports");
    assert!(seen.is_empty(), "NOTHING may be sent after auth on a refusal, saw: {seen:?}");
}

/// …and a node with neither string (the unarmed default) is refused the same way, naming the
/// FIRST missing one.
#[test]
fn an_unarmed_node_is_refused_naming_the_account_capability() {
    let (addr, report) = scripted_node(fake_node::features(&["observe"]));
    let err = exchange_once(addr, KEY, Scope::Write, &NEEDS, &retire("SOME_KEY"))
        .expect_err("refused client-side");
    assert!(err.to_string().contains(FEATURE_ACCOUNT_VERBS), "{err}");
    assert!(report.recv().expect("the node reports").is_empty());
}

/// The complement, without which the two refusals above are satisfied by refusing everything.
#[test]
fn an_advertising_node_gets_the_frame_and_its_reply_comes_back_verbatim() {
    let (addr, report) =
        scripted_node(fake_node::features(&[FEATURE_ACCOUNT_VERBS, FEATURE_CREDENTIAL_ADMIN]));
    let reply = exchange_once(addr, KEY, Scope::Write, &NEEDS, &retire("SOME_KEY"))
        .expect("advertised, so sent");
    assert!(
        matches!(&reply, Response::AccountWritten(w) if w.verb == "retire-credential" && w.changed),
        "{reply:?}"
    );
    assert_eq!(
        report.recv().expect("the node reports"),
        vec!["retire-credential confirm=SOME_KEY".to_string()]
    );
}

/// A node's REFUSAL is an answer, not an `io::Error`: the caller words it for the operator.
#[test]
fn a_node_refusal_is_returned_as_the_reply() {
    let (addr, _report) = fake_refusing_node();
    let reply = exchange_once(addr, KEY, Scope::Write, &[], &retire("SOME_KEY")).expect("answered");
    assert!(matches!(reply, Response::Error(ref e) if e == "nothing was changed"), "{reply:?}");
}

fn fake_refusing_node() -> (SocketAddr, mpsc::Receiver<Vec<String>>) {
    fake_node::scripted_node(KEY, Vec::new(), |_| {
        ("refused".to_string(), Response::Error("nothing was changed".into()))
    })
}
