#![cfg(feature = "data")]
//! THE WIRE'S BYTES, PINNED BY COMMITTED FIXTURES OUTSIDE `crates/`.
//!
//! The serde tags ARE the wire: a variant renamed in `src/proto.rs` is a protocol break that compiles
//! clean and passes every round-trip test, because both ends of a round trip move together. The
//! fixtures are what does not move with the edit: the `v1_*.jsonl` files of `fixtures/gatewayhub_wire/`, one JSON
//! frame BODY per line (the framing is `write_frame`'s, exercised below), kept under `fixtures/` so a
//! rename pass over `crates/` cannot reach them. (`vike-datahub-client`'s fixtures are length-prefixed
//! captures of a deployed peer; this protocol has no deployed peer yet, so its goldens are authored
//! once at inception, line-delimited so they can be read and reviewed as text. The first deployed
//! daemon's captures join them as `v1_*` additions, never replacements. The `stream` class's frames are the
//! tube's: their goldens are `fixtures/stream_wire/`, read by
//! `crates/vike-market-stream/tests/stream_tag_fixtures.rs`.)
//!
//! Nothing in this file spells an expected VARIANT tag (`SITES` says only WHERE each enum sits inside a
//! frame, as the goldens already have it). Three checks, all against the fixtures:
//!
//! * **decode** — every golden frame decodes into this build's type;
//! * **re-encode** — this build still PRODUCES every key and value the golden carries (a SUBSET check,
//!   so an additive field is allowed and a renamed one never is);
//! * **coverage** — every variant of every wire enum declared in `src/proto.rs` (and the presence
//!   word in `src/credential.rs`) appears in a fixture AT ITS ENUM'S OWN POSITION in a frame (`SITES`),
//!   so a new variant cannot ship uncaptured, and one that reuses another enum's variant name is not
//!   covered by that enum's frames.
//!
//! plus the decode-vs-drop pair: an unknown variant from a newer peer is a decode ERROR (a bad
//! request, not a bad connection), and an unknown FIELD on a reply is ignored (no
//! `deny_unknown_fields` on a reply).

use std::collections::BTreeSet;
use std::io::{Cursor, ErrorKind};
use std::path::PathBuf;

use serde_json::Value;
use vike_gatewayhub_client::proto::{
    OptionsFeedDown, Request, Response, read_frame_raw, write_frame,
};

const REQUESTS: &str = "fixtures/gatewayhub_wire/v1_requests.jsonl";
const RESPONSES: &str = "fixtures/gatewayhub_wire/v1_responses.jsonl";
const FEED_DOWN: &str = "fixtures/gatewayhub_wire/v1_feed_down.jsonl";

/// The sources whose wire enums the coverage check reads, as repo-relative paths in `const`s so the
/// path-key gate sees them.
const PROTO_SRC: &str = "crates/vike-gatewayhub-client/src/proto.rs";
const CREDENTIAL_SRC: &str = "crates/vike-gatewayhub-client/src/credential.rs";

/// Every wire enum, by the file that declares it. A new wire enum joins this list or its variants are
/// pinned by nothing — the `every_wire_enum_in_the_source_is_listed` check below refuses that.
const WIRE_ENUMS: &[(&str, &[&str])] = &[
    (
        PROTO_SRC,
        &[
            "Request",
            "FetchWhat",
            "OptionIndexKind",
            "OptionsCall",
            "OptionsReply",
            "OptionsFeedSpec",
            "WireTab",
            "WireCatalogMode",
            "Response",
            "FetchState",
            "FetchFailure",
            "FetchRows",
            "OptionsFeedDown",
        ],
    ),
    (CREDENTIAL_SRC, &["CredentialPresence"]),
];

fn read_repo(rel: &str) -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").join(rel);
    std::fs::read(&path).unwrap_or_else(|e| {
        panic!(
            "{rel} could not be read ({e}).\nThese goldens pin the wire; restore the file from git — \
             never regenerate it to make a test pass."
        )
    })
}

fn read_text(rel: &str) -> String {
    String::from_utf8(read_repo(rel)).unwrap_or_else(|e| panic!("{rel} is not UTF-8: {e}"))
}

/// The golden frame bodies of `rel`, one per non-empty line.
fn lines(rel: &str) -> Vec<String> {
    read_text(rel).lines().map(str::trim).filter(|l| !l.is_empty()).map(str::to_string).collect()
}

/// `expected` ⊆ `actual`, recursively: every key of `expected` exists in `actual` with a contained
/// value, arrays match elementwise, scalars are equal. The asymmetry is the point (module doc).
fn contains(actual: &Value, expected: &Value, at: &str) -> Result<(), String> {
    match (actual, expected) {
        (Value::Object(a), Value::Object(e)) => {
            for (k, ev) in e {
                let Some(av) = a.get(k) else {
                    let present: Vec<&String> = a.keys().collect();
                    return Err(format!(
                        "{at}/{k} is in the golden frame and NOT in what this build encodes \
                         (present keys: {present:?})"
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

/// Decode every golden line of `rel` as `T`, re-encode it THROUGH THE FRAME CODEC, and require the
/// golden to survive.
fn assert_round_trip<T>(rel: &str, expect_frames: usize)
where
    T: serde::de::DeserializeOwned + serde::Serialize,
{
    let goldens = lines(rel);
    assert_eq!(
        goldens.len(),
        expect_frames,
        "{rel} should hold {expect_frames} frames, not {} — a frame was added or dropped without \
         updating this assertion, and a shrunken fixture guards less than it claims",
        goldens.len()
    );
    for golden in &goldens {
        let expected: Value = serde_json::from_str(golden)
            .unwrap_or_else(|e| panic!("{rel} holds a line that is not JSON ({e}): {golden}"));
        let decoded: T = serde_json::from_str(golden).unwrap_or_else(|e| {
            panic!(
                "A GOLDEN FRAME NO LONGER DECODES — a wire tag or field in {PROTO_SRC} changed.\n\
                 fixture: {rel}\n  frame: {golden}\n  serde: {e}\nRestore the source; do not touch \
                 the fixture."
            )
        });
        let mut wire = Vec::new();
        write_frame(&mut wire, &decoded).expect("the wire types encode");
        let body = read_frame_raw(&mut Cursor::new(wire)).expect("a frame written reads back");
        let reencoded: Value = serde_json::from_slice(&body).expect("the body is JSON");
        contains(&reencoded, &expected, "").unwrap_or_else(|why| {
            panic!(
                "THIS BUILD NO LONGER PRODUCES A GOLDEN FRAME'S BYTES.\nfixture: {rel}\n  frame: {golden}\n  \
                 {why}\nA wire tag or field in {PROTO_SRC} was renamed. Restore it."
            )
        });
    }
}

#[test]
fn the_golden_requests_decode_and_re_encode() {
    assert_round_trip::<Request>(REQUESTS, 32);
}

#[test]
fn the_golden_responses_decode_and_re_encode() {
    assert_round_trip::<Response>(RESPONSES, 29);
}

#[test]
fn the_golden_feed_frames_decode_and_re_encode() {
    assert_round_trip::<OptionsFeedDown>(FEED_DOWN, 4);
}

/// The variants of `pub enum NAME { … }` in `src`: the identifiers at four-space indent that start with
/// a capital (doc comments, attributes and struct-variant fields are all something else).
fn variants_of(src: &str, name: &str) -> Vec<String> {
    let header = format!("pub enum {name} {{");
    let start = src.find(&header).unwrap_or_else(|| panic!("`{header}` is not in the source"));
    let mut out = Vec::new();
    for line in src[start..].lines().skip(1) {
        if line == "}" {
            break;
        }
        let Some(rest) = line.strip_prefix("    ") else { continue };
        if rest.starts_with(' ') {
            continue; // a struct variant's field, or a continuation line
        }
        if rest.starts_with(|c: char| c.is_ascii_uppercase()) {
            let ident: String = rest.chars().take_while(|c| c.is_ascii_alphanumeric()).collect();
            out.push(ident);
        }
    }
    assert!(
        !out.is_empty(),
        "no variants found for {name} — the scanner stopped matching the source"
    );
    out
}

/// Where the value of one wire enum sits inside a golden frame: the fixture file, and the object keys
/// from the frame's root down to it (empty = the frame itself IS the enum's value). Frames are
/// externally tagged, so the first key of a path is the root enum's variant tag.
struct Site {
    file: &'static str,
    path: &'static [&'static str],
}

/// **Every wire enum's golden sites.** A variant is covered only by a frame that carries it AT ITS
/// ENUM'S OWN POSITION: `StreamDown::Quote` is the top-level tag of a stream-down frame, and the
/// `"Quote"` a `Gap` frame carries under `verb` is `GapVerb::Quote`, a different variant of a
/// different enum that must not stand in for it. (A text search for the quoted name, over all the
/// fixtures at once, did exactly that: `StreamDown::{Seed, BarClose, Quote, Trade, Book, BookDelta}`
/// were "covered" by `GapVerb`, and `StreamDown::{Heartbeat, Bye}` by `OptionsFeedDown`.)
///
/// A new wire enum joins `WIRE_ENUMS` AND this table, or `every_wire_enum_has_a_golden_site` refuses it.
const SITES: &[(&str, &[Site])] = &[
    ("Request", &[Site { file: REQUESTS, path: &[] }]),
    (
        "FetchWhat",
        &[
            Site { file: REQUESTS, path: &["Fetch", "what"] },
            Site { file: REQUESTS, path: &["FetchCredentialed", "what"] },
        ],
    ),
    (
        "OptionIndexKind",
        &[Site { file: REQUESTS, path: &["Fetch", "what", "OptionIndex", "kind"] }],
    ),
    ("OptionsCall", &[Site { file: REQUESTS, path: &["OptionsRest", "call"] }]),
    ("OptionsReply", &[Site { file: RESPONSES, path: &["OptionsReplied"] }]),
    ("OptionsFeedSpec", &[Site { file: REQUESTS, path: &["OptionsFeedOpen", "feed"] }]),
    ("WireTab", &[Site { file: REQUESTS, path: &["Fetch", "what", "CatalogSearch", "tab"] }]),
    ("WireCatalogMode", &[Site { file: RESPONSES, path: &["CatalogDescribed", "mode"] }]),
    ("Response", &[Site { file: RESPONSES, path: &[] }]),
    ("FetchState", &[Site { file: RESPONSES, path: &["FetchProgress", "state"] }]),
    ("FetchFailure", &[Site { file: RESPONSES, path: &["FetchFailed", "failure"] }]),
    ("FetchRows", &[Site { file: RESPONSES, path: &["FetchPage", "rows"] }]),
    ("OptionsFeedDown", &[Site { file: FEED_DOWN, path: &[] }]),
    ("CredentialPresence", &[Site { file: RESPONSES, path: &["PresenceIs", "presence"] }]),
];

/// The variant tags found at `site` over the golden frames of its file: a bare string is a unit
/// variant's tag, an object with one key is a data variant's. A frame without the path is skipped
/// (it is some other variant of the root enum).
fn tags_at(site: &Site) -> BTreeSet<String> {
    let mut tags = BTreeSet::new();
    for golden in lines(site.file) {
        let frame: Value = serde_json::from_str(&golden).unwrap_or_else(|e| {
            panic!("{} holds a line that is not JSON ({e}): {golden}", site.file)
        });
        let mut at = &frame;
        let mut present = true;
        for key in site.path {
            match at.get(*key) {
                Some(next) => at = next,
                None => {
                    present = false;
                    break;
                }
            }
        }
        if !present {
            continue;
        }
        match at {
            Value::String(tag) => {
                tags.insert(tag.clone());
            }
            Value::Object(map) if map.len() == 1 => {
                tags.extend(map.keys().cloned());
            }
            _ => {}
        }
    }
    tags
}

/// The variant tags a golden frame carries for `name`, over every site the table gives it.
fn golden_tags_of(name: &str) -> BTreeSet<String> {
    let sites = SITES
        .iter()
        .find(|(enum_name, _)| *enum_name == name)
        .map(|(_, sites)| *sites)
        .unwrap_or_else(|| {
            panic!("`{name}` is a wire enum with no golden site in SITES; add its row")
        });
    sites.iter().flat_map(tags_at).collect()
}

/// Every variant of every wire enum appears in a golden frame **at that enum's own position**. The
/// check is keyed on `(enum, variant)`: a same-named variant of another enum, or the same word in
/// another position, covers nothing.
#[test]
fn every_wire_variant_is_covered_by_the_fixtures() {
    let mut missing = Vec::new();
    for (file, enums) in WIRE_ENUMS {
        let src = read_text(file);
        for name in *enums {
            let golden = golden_tags_of(name);
            for variant in variants_of(&src, name) {
                if !golden.contains(&variant) {
                    missing.push(format!("{name}::{variant}"));
                }
            }
        }
    }
    assert!(
        missing.is_empty(),
        "wire variant(s) that no golden frame carries at their enum's position: {missing:?}\nA NEW \
         variant ships with a captured frame in fixtures/gatewayhub_wire/ — a wire tag nobody \
         captured is a wire tag nobody holds still. (A same-named variant of another enum does not \
         count: SITES says where each enum sits in a frame.)"
    );
}

/// `SITES` and `WIRE_ENUMS` name the same enums, so a coverage row cannot be forgotten or left behind.
#[test]
fn every_wire_enum_has_a_golden_site() {
    let listed: Vec<&str> =
        WIRE_ENUMS.iter().flat_map(|(_, names)| names.iter().copied()).collect();
    let sited: Vec<&str> = SITES.iter().map(|(name, _)| *name).collect();
    let unsited: Vec<&&str> = listed.iter().filter(|n| !sited.contains(n)).collect();
    let orphan: Vec<&&str> = sited.iter().filter(|n| !listed.contains(n)).collect();
    assert!(unsited.is_empty(), "wire enum(s) in WIRE_ENUMS with no row in SITES: {unsited:?}");
    assert!(orphan.is_empty(), "SITES row(s) for an enum WIRE_ENUMS does not list: {orphan:?}");
    // And a site really finds something: a path that matches no frame covers nothing, silently.
    for (name, sites) in SITES {
        for site in *sites {
            assert!(
                !tags_at(site).is_empty(),
                "the site {:?} of `{name}` in {} matches no golden frame",
                site.path,
                site.file
            );
        }
    }
}

/// The list above is complete: every `pub enum` in `proto.rs` that derives `Serialize` is named in it
/// (or is one of the few that serialize as a DTO inside another and are covered through it).
#[test]
fn every_wire_enum_in_the_source_is_listed() {
    let src = read_text(PROTO_SRC);
    let listed = WIRE_ENUMS[0].1;
    let mut unlisted = Vec::new();
    let all: Vec<&str> = src.lines().collect();
    for (i, line) in all.iter().enumerate() {
        let Some(rest) = line.strip_prefix("pub enum ") else { continue };
        let name: String = rest.chars().take_while(|c| c.is_ascii_alphanumeric()).collect();
        let derives_serialize = all[..i]
            .iter()
            .rev()
            .take_while(|l| l.starts_with("#[") || l.starts_with("///") || l.is_empty())
            .any(|l| l.starts_with("#[derive(") && l.contains("Serialize"));
        if derives_serialize && !listed.contains(&name.as_str()) {
            unlisted.push(name);
        }
    }
    assert!(
        unlisted.is_empty(),
        "wire enum(s) not in WIRE_ENUMS, so their variants are pinned by nothing: {unlisted:?}"
    );
}

/// ATTRIBUTE lines only: a `#[serde(rename = "…")]` argument is a wire spelling, so every one the source
/// declares must appear in a golden. (There are none today; the day one is added, it arrives with a
/// captured frame.)
#[test]
fn every_serde_rename_pin_is_covered_by_the_fixtures() {
    let src = read_text(PROTO_SRC);
    let mut frozen = String::new();
    for rel in [REQUESTS, RESPONSES, FEED_DOWN] {
        frozen.push_str(&read_text(rel));
    }
    const OPEN: &str = "rename = \"";
    let mut missing = Vec::new();
    for line in src.lines() {
        let trimmed = line.trim_start();
        if !trimmed.starts_with("#[serde(") {
            continue;
        }
        let mut rest = trimmed;
        while let Some(i) = rest.find(OPEN) {
            rest = &rest[i + OPEN.len()..];
            let Some(end) = rest.find('"') else { break };
            let pin = &rest[..end];
            if !frozen.contains(&format!("\"{pin}\"")) {
                missing.push(pin.to_string());
            }
            rest = &rest[end..];
        }
    }
    assert!(missing.is_empty(), "rename pin(s) no golden frame carries: {missing:?}");
}

#[test]
fn an_unknown_variant_is_a_decode_error_and_an_unknown_reply_field_is_ignored() {
    // A verb from a newer client: the server reads raw, fails to decode, answers Error and KEEPS the
    // connection (decode-vs-drop) — so decoding must fail cleanly, not panic.
    assert!(serde_json::from_str::<Request>(r#"{"FromTheFuture":{"x":1}}"#).is_err());
    // A reply may grow a field: an old client must still read it (no `deny_unknown_fields`).
    let grown = r#"{"FetchDone":{"rid":1,"pages":0,"rows":0,"checksum":"abc"}}"#;
    assert!(serde_json::from_str::<Response>(grown).is_ok());
}

/// The frame codec is `vike_hub_proto::frame`'s: a golden written and read back is the same bytes.
#[test]
fn a_frame_is_a_big_endian_length_then_the_body() {
    let mut wire = Vec::new();
    write_frame(&mut wire, &Request::Ping).unwrap();
    assert_eq!(&wire[..4], &(u32::try_from(wire.len() - 4).unwrap()).to_be_bytes());
    assert_eq!(&wire[4..], br#""Ping""#);
    let mut cursor = Cursor::new(wire);
    assert_eq!(read_frame_raw(&mut cursor).unwrap(), br#""Ping""#);
    let eof = read_frame_raw(&mut cursor).unwrap_err();
    assert_eq!(eof.kind(), ErrorKind::UnexpectedEof, "a clean end of stream is UnexpectedEof");
}
