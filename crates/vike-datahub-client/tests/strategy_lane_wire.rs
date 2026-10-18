//! THE STRATEGY LANE'S OPEN, PINNED BY COMMITTED GOLDENS OUTSIDE `crates/`.
//!
//! `StratOpen` and `StratOpened` are the two positional frames of the lane (`docs/decisions/0167`'s
//! tube carries everything after them), and their serde tags are the wire: a rename compiles clean and
//! passes every round trip because both ends move together. The goldens are what does not move with
//! the edit: `fixtures/datahub_wire/v1_strat_requests.jsonl` and
//! `fixtures/datahub_wire/v1_strat_responses.jsonl`, one JSON frame BODY per line (the framing is
//! `write_frame`'s), under `fixtures/` so a rename pass over `crates/` cannot reach them.
//!
//! Three checks, all against the fixtures (the shape of `crates/vike-market-stream/tests/stream_tag_fixtures.rs`):
//! every golden decodes into this build's type, this build still PRODUCES every key and value of it
//! (a SUBSET check: an additive field is allowed, a renamed one never is), and every strategy-lane
//! variant of `Request` / `Response` the source declares has a golden - so a lane verb added later
//! cannot ship uncaptured. (The pre-rename compute frames stay `wire_tag_fixtures.rs`'s.)

use std::io::Cursor;
use std::path::PathBuf;

use serde_json::Value;
use vike_datahub_client::proto::{Request, Response, read_frame_raw, write_frame};

const REQUESTS: &str = "fixtures/datahub_wire/v1_strat_requests.jsonl";
const RESPONSES: &str = "fixtures/datahub_wire/v1_strat_responses.jsonl";
const PROTO_SRC: &str = "crates/vike-datahub-client/src/proto.rs";

fn read_text(rel: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "{rel} could not be read ({e}).\nThese goldens pin the lane's wire; restore the file \
             from git - never regenerate it to make a test pass."
        )
    })
}

fn lines(rel: &str) -> Vec<String> {
    read_text(rel).lines().map(str::trim).filter(|l| !l.is_empty()).map(str::to_string).collect()
}

/// `expected` is a subset of `actual`, recursively (objects by key, arrays elementwise, scalars equal).
fn contains(actual: &Value, expected: &Value, at: &str) -> Result<(), String> {
    match (actual, expected) {
        (Value::Object(a), Value::Object(e)) => {
            for (k, ev) in e {
                let Some(av) = a.get(k) else {
                    return Err(format!(
                        "{at}/{k} is in the golden and NOT in what this build encodes"
                    ));
                };
                contains(av, ev, &format!("{at}/{k}"))?;
            }
            Ok(())
        }
        (Value::Array(a), Value::Array(e)) if a.len() == e.len() => {
            for (i, (av, ev)) in a.iter().zip(e).enumerate() {
                contains(av, ev, &format!("{at}/{i}"))?;
            }
            Ok(())
        }
        _ if actual == expected => Ok(()),
        _ => Err(format!("{at}: golden has {expected}, this build encodes {actual}")),
    }
}

fn assert_round_trip<T>(rel: &str, expect_frames: usize)
where
    T: serde::de::DeserializeOwned + serde::Serialize,
{
    let goldens = lines(rel);
    assert_eq!(goldens.len(), expect_frames, "{rel} holds {} frames", goldens.len());
    for golden in &goldens {
        let expected: Value = serde_json::from_str(golden)
            .unwrap_or_else(|e| panic!("{rel} holds a line that is not JSON ({e}): {golden}"));
        let decoded: T = serde_json::from_str(golden).unwrap_or_else(|e| {
            panic!(
                "A GOLDEN LANE FRAME NO LONGER DECODES - a tag or field in {PROTO_SRC} changed.\n\
                 fixture: {rel}\n  frame: {golden}\n  serde: {e}\nRestore the source, not the fixture."
            )
        });
        let mut wire = Vec::new();
        write_frame(&mut wire, &decoded).expect("the lane's frames encode");
        let body = read_frame_raw(&mut Cursor::new(wire)).expect("a frame written reads back");
        let reencoded: Value = serde_json::from_slice(&body).expect("the body is JSON");
        contains(&reencoded, &expected, "").unwrap_or_else(|why| {
            panic!(
                "THIS BUILD NO LONGER PRODUCES A GOLDEN LANE FRAME'S BYTES.\nfixture: {rel}\n  \
                 frame: {golden}\n  {why}"
            )
        });
    }
}

#[test]
fn the_golden_lane_frames_decode_and_re_encode() {
    assert_round_trip::<Request>(REQUESTS, 3);
    assert_round_trip::<Response>(RESPONSES, 1);
}

/// The variant names of `pub enum NAME { ... }` in `src` that mention the strategy lane (the open and
/// its reply): four-space-indented capitalised identifiers whose doc block names `StratOpen`.
fn lane_variants(src: &str, name: &str) -> Vec<String> {
    let header = format!("pub enum {name} {{");
    let start = src.find(&header).unwrap_or_else(|| panic!("`{header}` is not in the source"));
    let mut out = Vec::new();
    for line in src[start..].lines().skip(1) {
        if line == "}" {
            break;
        }
        let Some(rest) = line.strip_prefix("    ") else { continue };
        if rest.starts_with(' ') || !rest.starts_with(|c: char| c.is_ascii_uppercase()) {
            continue;
        }
        let ident: String = rest.chars().take_while(|c| c.is_ascii_alphanumeric()).collect();
        // `Strat` then an UPPERCASE letter: the lane's verbs (`StratOpen`, `StratOpened`), not the
        // `Strategies` reply of `ListStrategies`.
        if ident
            .strip_prefix("Strat")
            .is_some_and(|rest| rest.starts_with(|c: char| c.is_ascii_uppercase()))
        {
            out.push(ident);
        }
    }
    out
}

/// A lane verb added to `Request` / `Response` (any variant named `Strat...`) must arrive with a
/// golden: the tag it ships under is on a wire nobody can see otherwise.
#[test]
fn every_lane_variant_the_source_declares_has_a_golden() {
    let src = read_text(PROTO_SRC);
    let requests = lines(REQUESTS).join("\n");
    let responses = lines(RESPONSES).join("\n");
    let mut declared = lane_variants(&src, "Request");
    declared.extend(lane_variants(&src, "Response"));
    assert!(declared.len() >= 2, "the scanner found no lane variants: {declared:?}");
    for variant in lane_variants(&src, "Request") {
        assert!(
            requests.contains(&format!("{{\"{variant}\":")),
            "`Request::{variant}` has no golden in {REQUESTS}"
        );
    }
    for variant in lane_variants(&src, "Response") {
        assert!(
            responses.contains(&format!("{{\"{variant}\":")),
            "`Response::{variant}` has no golden in {RESPONSES}"
        );
    }
}

/// The decode-vs-drop pair: a lane frame with an unknown field decodes (no `deny_unknown_fields`), and
/// an unknown lane verb is an ERROR the server answers rather than a dropped connection.
#[test]
fn an_unknown_field_is_ignored_and_an_unknown_lane_verb_is_a_decode_error() {
    let widened = r#"{"StratOpen":{"venue":"binance","a_field_from_a_newer_peer":1}}"#;
    assert!(serde_json::from_str::<Request>(widened).is_ok());
    assert!(serde_json::from_str::<Request>(r#"{"StratUpdate":{"add":[],"remove":[]}}"#).is_err());
}
