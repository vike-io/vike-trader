//! THE STREAM WIRE'S BYTES, PINNED BY COMMITTED FIXTURES OUTSIDE `crates/`.
//!
//! The serde tags ARE the wire: a variant renamed in `src/wire.rs` is a protocol break that compiles
//! clean and passes every round-trip test, because both ends of a round trip move together. The
//! fixtures are what does not move with the edit: the `v1_stream_*.jsonl` files of
//! `fixtures/stream_wire/`, one JSON frame BODY per line (the framing is `write_frame`'s), kept under
//! `fixtures/` so a rename pass over `crates/` cannot reach them. (The request/response/feed goldens
//! of the gatewayhub's own protocol stay in `fixtures/gatewayhub_wire/`, read by
//! `crates/vike-gatewayhub-client/tests/wire_tag_fixtures.rs`.)
//!
//! Nothing in this file spells an expected VARIANT tag (`SITES` says only WHERE each enum sits inside a
//! frame, as the goldens already have it). Three checks, all against the fixtures:
//!
//! * **decode** — every golden frame decodes into this build's type;
//! * **re-encode** — this build still PRODUCES every key and value the golden carries (a SUBSET check,
//!   so an additive field is allowed and a renamed one never is);
//! * **coverage** — every variant of every wire enum declared in `src/wire.rs` appears in a fixture AT
//!   ITS ENUM'S OWN POSITION in a frame (`SITES`), so a new variant cannot ship uncaptured, and one that
//!   reuses another enum's variant name is not covered by that enum's frames.
//!
//! plus the decode-vs-drop pair: an unknown variant from a newer peer is a decode ERROR, and an unknown
//! FIELD on a frame is ignored (no `deny_unknown_fields`).

use std::path::PathBuf;

use vike_hub_proto::test_support::{Goldens, Site};
use vike_market_stream::wire::{StreamDown, StreamUp};

const STREAM_UP: &str = "fixtures/stream_wire/v1_stream_up.jsonl";
const STREAM_DOWN: &str = "fixtures/stream_wire/v1_stream_down.jsonl";

/// The source whose wire enums the coverage check reads, as a repo-relative path in a `const` so the
/// path-key gate sees it.
const WIRE_SRC: &str = "crates/vike-market-stream/src/wire.rs";

/// Every wire enum, by the file that declares it. A new wire enum joins this list or its variants are
/// pinned by nothing — the `every_wire_enum_in_the_source_is_listed` check below refuses that.
const WIRE_ENUMS: &[(&str, &[&str])] = &[(
    WIRE_SRC,
    &[
        "StreamUp",
        "SubOp",
        "SubErrKind",
        "StreamDown",
        "ByeReason",
        "GapVerb",
        "WireStream",
        "WireLane",
        "WireDelivery",
    ],
)];

/// The goldens, read under the repository root this binary was built in (the shared checks live in
/// `crates/vike-hub-proto/src/test_support.rs`, compiled once for every protocol tag suite).
fn goldens() -> Goldens {
    Goldens::at(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
}

#[test]
fn the_golden_stream_frames_decode_and_re_encode() {
    goldens().assert_round_trip::<StreamUp>(STREAM_UP, 14, WIRE_SRC);
    goldens().assert_round_trip::<StreamDown>(STREAM_DOWN, 37, WIRE_SRC);
}

/// **Every wire enum's golden sites.** A variant is covered only by a frame that carries it AT ITS
/// ENUM'S OWN POSITION: `StreamDown::Quote` is the top-level tag of a stream-down frame, and the
/// `"Quote"` a `Gap` frame carries under `verb` is `GapVerb::Quote`, a different variant of a
/// different enum that must not stand in for it.
///
/// A new wire enum joins `WIRE_ENUMS` AND this table, or `every_wire_enum_has_a_golden_site` refuses it.
const SITES: &[(&str, &[Site])] = &[
    ("StreamUp", &[Site { file: STREAM_UP, path: &[] }]),
    ("SubOp", &[Site { file: STREAM_UP, path: &["Sub", "op"] }]),
    ("SubErrKind", &[Site { file: STREAM_DOWN, path: &["SubErr", "err"] }]),
    ("StreamDown", &[Site { file: STREAM_DOWN, path: &[] }]),
    ("ByeReason", &[Site { file: STREAM_DOWN, path: &["Bye"] }]),
    ("GapVerb", &[Site { file: STREAM_DOWN, path: &["Gap", "verb"] }]),
    (
        "WireStream",
        &[
            Site { file: STREAM_DOWN, path: &["Status", "stream"] },
            Site { file: STREAM_DOWN, path: &["Delay", "stream"] },
            Site { file: STREAM_DOWN, path: &["Gap", "stream"] },
        ],
    ),
    (
        "WireLane",
        &[
            Site { file: STREAM_DOWN, path: &["Status", "stream", "Lane"] },
            Site { file: STREAM_DOWN, path: &["Delay", "stream", "Lane"] },
            Site { file: STREAM_DOWN, path: &["Gap", "stream", "Lane"] },
            Site { file: STREAM_DOWN, path: &["Delivery", "lane"] },
        ],
    ),
    (
        "WireDelivery",
        &[
            Site { file: STREAM_UP, path: &["Sub", "class"] },
            Site { file: STREAM_UP, path: &["Delivery", "class"] },
            Site { file: STREAM_DOWN, path: &["Delivery", "class"] },
        ],
    ),
];

/// Every variant of every wire enum appears in a golden frame **at that enum's own position**. The
/// check is keyed on `(enum, variant)`: a same-named variant of another enum, or the same word in
/// another position, covers nothing.
#[test]
fn every_wire_variant_is_covered_by_the_fixtures() {
    goldens().assert_every_variant_is_covered(WIRE_ENUMS, SITES, "fixtures/stream_wire/");
}

/// `SITES` and `WIRE_ENUMS` name the same enums, so a coverage row cannot be forgotten or left behind.
#[test]
fn every_wire_enum_has_a_golden_site() {
    goldens().assert_every_enum_has_a_golden_site(WIRE_ENUMS, SITES);
}

/// The list above is complete: every `pub enum` in `wire.rs` that derives `Serialize` is named in it.
#[test]
fn every_wire_enum_in_the_source_is_listed() {
    let src = goldens().read_text(WIRE_SRC);
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
    let src = goldens().read_text(WIRE_SRC);
    let mut frozen = String::new();
    for rel in [STREAM_UP, STREAM_DOWN] {
        frozen.push_str(&goldens().read_text(rel));
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
fn an_unknown_variant_is_a_decode_error_and_an_unknown_field_is_ignored() {
    // A frame from a newer peer: decoding must fail cleanly, not panic.
    assert!(serde_json::from_str::<StreamUp>(r#""Teleport""#).is_err());
    // A frame may grow a field: an older peer must still read it (no `deny_unknown_fields`).
    let grown = r#"{"Seed":{"k":1,"seq":1,"interval":"1m","bars":[],"future":true}}"#;
    assert!(serde_json::from_str::<StreamDown>(grown).is_ok());
}
