//! THE USER'S SYMBOL LISTS AND THE VENUES' INSTRUMENT LISTS ON THE WIRE, PINNED BY COMMITTED GOLDENS OUTSIDE `crates/`.
//!
//! `fixtures/datahub_wire/v1_lists_requests.jsonl` and `fixtures/datahub_wire/v1_lists_responses.jsonl` hold one JSON
//! frame BODY per line: every golden decodes into this build's type and this build still PRODUCES every key of it
//! (`crates/vike-hub-protocol/src/test_support.rs`' subset check), so a renamed tag or field fails here while both ends
//! of a round trip would move together. A new frame is a line APPENDED to its file; an existing line is never edited.

use std::path::PathBuf;

use vike_datahub_client::proto::{Request, Response};
use vike_hub_protocol::test_support::Goldens;

const REQUESTS: &str = "fixtures/datahub_wire/v1_lists_requests.jsonl";
const RESPONSES: &str = "fixtures/datahub_wire/v1_lists_responses.jsonl";
const PROTO_SOURCE: &str = "crates/vike-datahub-client/src/proto.rs";

/// The verbs and replies these goldens cover; each must have a line.
const REQUEST_TAGS: [&str; 7] = [
    "SymbolSets",
    "SetSymbolSet",
    "DeleteSymbolSet",
    "InstrumentLists",
    "RefreshInstrumentLists",
    "OperatorInstruments",
    "SplitWarnings",
];
const RESPONSE_TAGS: [&str; 5] = [
    "SymbolSets",
    "InstrumentLists",
    "InstrumentListsRefreshed",
    "OperatorInstruments",
    "SplitWarnings",
];

fn goldens() -> Goldens {
    Goldens::at(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
}

#[test]
fn the_golden_list_frames_decode_and_re_encode() {
    goldens().assert_round_trip::<Request>(REQUESTS, 9, PROTO_SOURCE);
    goldens().assert_round_trip::<Response>(RESPONSES, 6, PROTO_SOURCE);
}

#[test]
fn every_list_verb_and_reply_has_a_golden() {
    let goldens = goldens();
    let requests = goldens.lines(REQUESTS).join("\n");
    let responses = goldens.lines(RESPONSES).join("\n");
    for tag in REQUEST_TAGS {
        assert!(
            requests.contains(&format!("{{\"{tag}\":")) || requests.contains(&format!("\"{tag}\"")),
            "`Request::{tag}` has no golden in {REQUESTS}"
        );
    }
    for tag in RESPONSE_TAGS {
        assert!(
            responses.contains(&format!("{{\"{tag}\":")),
            "`Response::{tag}` has no golden in {RESPONSES}"
        );
    }
}
