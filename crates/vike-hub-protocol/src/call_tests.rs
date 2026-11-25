use std::io;

use vike_utility::errors::FailureOutcome;

use super::{HubAccessRefusalId, HubCallError};
use crate::handshake::HandshakeReply;

/// One of each variant, with the outcome and the `io::ErrorKind` it must keep, and the sentence a person reads.
fn every_variant() -> Vec<(HubCallError, FailureOutcome, io::ErrorKind, &'static str)> {
    let text = || "the words".to_string();
    vec![
        (
            HubCallError::NotSent(io::Error::new(io::ErrorKind::ConnectionRefused, text())),
            FailureOutcome::NotSent,
            io::ErrorKind::ConnectionRefused,
            "the words",
        ),
        (
            HubCallError::OutcomeUnknown(io::Error::new(io::ErrorKind::TimedOut, text())),
            FailureOutcome::OutcomeUnknown,
            io::ErrorKind::TimedOut,
            "the words",
        ),
        (
            HubCallError::NotNow { id: Some("tradehub.starting".into()), text: text() },
            FailureOutcome::NotNow,
            io::ErrorKind::InvalidData,
            "error [tradehub.starting]: the words",
        ),
        (
            HubCallError::Refused { id: None, text: text() },
            FailureOutcome::Refused,
            io::ErrorKind::Other,
            "the words",
        ),
        (
            HubCallError::Refused { id: Some("datahub.strategy_lane.full".into()), text: text() },
            FailureOutcome::Refused,
            io::ErrorKind::Other,
            "error [datahub.strategy_lane.full]: the words",
        ),
        (
            HubCallError::AccessDenied(text()),
            FailureOutcome::Refused,
            io::ErrorKind::PermissionDenied,
            "error [hub.access.denied]: the words",
        ),
        (
            HubCallError::NotOffered { feature: "person-auth".into(), text: text() },
            FailureOutcome::Refused,
            io::ErrorKind::Unsupported,
            "the words",
        ),
        (
            HubCallError::Protocol(text().into()),
            FailureOutcome::Refused,
            io::ErrorKind::InvalidData,
            "the words",
        ),
    ]
}

/// **The door's refusal: its id is the client's, and the wire keeps its bytes both ways.** `AuthDenied` carries the
/// hub's reason and nothing more, decoded from and encoded to the bytes every older peer speaks; the one id a person
/// reads, once, is the client's `hub.access.denied`, put in front of the client's sentence by `Display` alone.
#[test]
fn a_door_refusal_shows_the_clients_one_id_and_its_frame_keeps_its_bytes() {
    let bytes = r#"{"AuthDenied":{"reason":"bad mac"}}"#;
    let frame: HandshakeReply =
        serde_json::from_str(bytes).expect("an older hub's AuthDenied decodes");
    assert_eq!(serde_json::to_string(&frame).unwrap(), bytes, "and encodes to the same bytes");
    let HandshakeReply::AuthDenied { reason } = frame else { panic!("decoded as {frame:?}") };
    assert_eq!(reason, "bad mac", "the reason is the hub's words alone: no id rides the wire");

    let denied = HubCallError::AccessDenied(format!("datahub auth denied (Read): {reason}"));
    let shown = denied.to_string();
    assert_eq!(shown, "error [hub.access.denied]: datahub auth denied (Read): bad mac");
    assert_eq!(shown.matches("error [").count(), 1, "ONE id: {shown}");
    assert_eq!(HubAccessRefusalId::Denied.word(), "hub.access.denied");
}

/// **Each variant holds its outcome by construction**, and its sentence is the words it was given, byte for byte, but
/// a refusal the hub declared with an id, which shows it in front: `error [<id>]: <words>`, and the door's refusal,
/// which shows the client's one access id.
#[test]
fn each_variant_is_its_outcome_and_says_its_words() {
    for (error, outcome, _, sentence) in every_variant() {
        assert_eq!(error.failure_outcome(), outcome, "{error:?}");
        assert_eq!(error.to_string(), sentence, "{error:?}");
    }
}

/// **A verb that keeps `io::Result` loses nothing**: the kind each variant had before the type, the same text, and
/// the variant itself, carried inside (a firewall's `PermissionDenied` on a dial is still `NotSent`, never the hub's
/// refusal).
#[test]
fn into_an_io_error_it_keeps_its_kind_its_words_and_its_outcome() {
    let carried_outcome = |error: &io::Error| {
        error
            .get_ref()
            .and_then(|inner| inner.downcast_ref::<HubCallError>())
            .map(HubCallError::failure_outcome)
    };
    for (error, outcome, kind, sentence) in every_variant() {
        let carried = io::Error::from(error);
        assert_eq!(carried.kind(), kind, "{carried:?}");
        assert_eq!(carried.to_string(), sentence);
        assert_eq!(carried_outcome(&carried), Some(outcome), "{carried:?}");
    }
    let firewall =
        io::Error::from(HubCallError::NotSent(io::Error::from(io::ErrorKind::PermissionDenied)));
    assert_eq!(carried_outcome(&firewall), Some(FailureOutcome::NotSent));
}
