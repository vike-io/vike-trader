//! THE `paramscan` RENAME'S WIRE GUARD: a pre-rename peer's REAL FRAMES, on disk, outside `crates/`.
//!
//! # The defect this file exists for, and why its predecessor could not catch it
//!
//! `crates/vike-datahub-client/src/proto.rs` carries five `#[serde(rename = "…")]` attributes that
//! freeze the compute wire's byte spelling while the Rust identifiers moved to `paramscan`. Only
//! the attributes' STRING ARGUMENTS are on the wire, and they must never change — a typo in one is
//! a silent, permanent protocol break that compiles clean.
//!
//! The guard for that used to be a `#[test]` **in `proto.rs` itself**, asserting the same strings
//! as source-code literals a few hundred lines below the pins. That is not a guard: during the
//! rename that introduced the pins, one pass corrupted a pin AND rewrote the test's expected
//! string in the same edit, and **a 1,097-test run and a complete 22-gate `verify-branch` both
//! went green over a broken protocol**. It was found by a human reading the diff. A check that
//! lives inside its own subject cannot survive the edit it exists to catch, because one `sed` moves
//! both halves together.
//!
//! # The shape of the fix: the expectation is not source code at all
//!
//! The expected bytes are two committed fixtures of REAL length-prefixed frames —
//! `fixtures/datahub_wire/v7_paramscan_rename_requests.frames` and its `…_responses` sibling —
//! written exactly as `vike_datahub_client::proto::write_frame` writes them (big-endian `u32`
//! length, then the UTF-8 JSON body). They sit under `fixtures/`, so a rename pass over
//! `crates/` — over this file included — cannot reach them, and a text tool that DID reach them
//! would desync the length prefixes and fail the framing read before a single assertion ran.
//!
//! Nothing in this file spells an expected wire string. Every expectation is READ from the
//! fixture: the decode direction proves the pins still accept a pre-rename peer's bytes, the
//! re-encode direction proves this build still PRODUCES them, and
//! [`every_pinned_wire_tag_is_covered_by_the_fixtures`] proves the fixtures cover every pin the
//! source declares, so a new pin cannot ship uncovered.
//!
//! # Why the re-encode check is a SUBSET and not byte equality
//!
//! Byte equality would redden on a legitimate ADDITIVE field — the exact change `PROTO_VERSION`'s
//! own log records twice (v6's `params`, v7's `search`), each of which an old peer decodes fine.
//! Reddening there would force a fixture REGENERATION, and a regeneration is precisely the door a
//! corrupted pin walks in through. So the rule is: everything the old peer's frame contained must
//! still be produced under the same names, and a new key beside it is allowed. A renamed key never
//! is.
//!
//! # Declared scope
//!
//! `proto.rs` only. `crates/vike-datahub-client/src/wire_studio.rs` and
//! `crates/vike-datahub-client/src/market.rs` carry the wire's payload types and today declare no
//! `rename` pin at all; the day one of them does, its file joins [`PROTO_SRC`] rather than being
//! assumed covered here.

use std::io::{Cursor, ErrorKind};
use std::path::PathBuf;

use serde_json::Value;
use vike_datahub_client::proto::{Request, Response, read_frame_raw};

/// The pre-rename REQUEST frames: `RunSweepProfile`, then the Studio verb whose own struct-variant
/// FIELD key is wire-visible too.
///
/// ⚠ Spelled as a REPO-RELATIVE path in a module-level `const` deliberately, and read at RUNTIME
/// rather than through `include_bytes!`: that shape is what puts it inside
/// `crates/vike-ops/tests/path_key_gate.rs`'s scan (which excludes include arguments by
/// construction), so a moved or deleted fixture reddens a second, independent gate as well as this
/// file.
const REQUEST_FRAMES: &str = "fixtures/datahub_wire/v7_paramscan_rename_requests.frames";

/// The pre-rename RESPONSE frames: `SweepResult` and `SweepReport`.
const RESPONSE_FRAMES: &str = "fixtures/datahub_wire/v7_paramscan_rename_responses.frames";

/// The file whose `#[serde(rename = "…")]` attributes these fixtures pin. Read as TEXT by
/// [`every_pinned_wire_tag_is_covered_by_the_fixtures`] — never parsed, never compiled against.
const PROTO_SRC: &str = "crates/vike-datahub-client/src/proto.rs";

/// Repo-root-relative read. `CARGO_MANIFEST_DIR` (never CWD), the idiom every parity suite in this
/// workspace uses — see `crates/vike-exec/tests/parity/r5_parity.rs`.
fn read_repo(rel: &str) -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").join(rel);
    std::fs::read(&path).unwrap_or_else(|e| {
        panic!(
            "{rel} could not be read ({e}).\nThese bytes are a FROZEN capture of what an \
             already-deployed peer sends; there is no regenerator and regeneration is not a \
             supported operation. Restore it from git."
        )
    })
}

/// Every frame BODY in the fixture at `rel`, read through the real framing reader — so a corrupted
/// length prefix fails HERE, loudly, instead of being silently re-interpreted.
fn frame_bodies(rel: &str) -> Vec<Vec<u8>> {
    let mut cur = Cursor::new(read_repo(rel));
    let mut out = Vec::new();
    loop {
        match read_frame_raw(&mut cur) {
            Ok(body) => out.push(body),
            Err(e) if e.kind() == ErrorKind::UnexpectedEof && !out.is_empty() => break,
            Err(e) => panic!("{rel} is not a well-framed stream after {} frames: {e}", out.len()),
        }
    }
    out
}

/// `expected` ⊆ `actual`, recursively: every object key in `expected` must exist in `actual` with a
/// contained value; arrays must match in length and elementwise; scalars must be equal.
///
/// The asymmetry is the whole point — see this file's module doc. `Err` carries the path of the
/// first divergence so a failure names the key rather than dumping two documents.
fn contains(actual: &Value, expected: &Value, at: &str) -> Result<(), String> {
    match (actual, expected) {
        (Value::Object(a), Value::Object(e)) => {
            for (k, ev) in e {
                let Some(av) = a.get(k) else {
                    let present: Vec<&String> = a.keys().collect();
                    return Err(format!(
                        "{at}/{k} is in the frozen frame and NOT in what this build encodes \
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
        _ => Err(format!("{at}: frozen frame has {expected}, this build encodes {actual}")),
    }
}

/// Decode each frozen frame as `T`, re-encode it, and require the frozen document to survive the
/// round trip.
fn assert_round_trip<T>(rel: &str, expect_frames: usize)
where
    T: serde::de::DeserializeOwned + serde::Serialize,
{
    let bodies = frame_bodies(rel);
    assert_eq!(
        bodies.len(),
        expect_frames,
        "{rel} should hold {expect_frames} frames, not {} — a frame was added or dropped without \
         updating this assertion, and a shrunken fixture guards less than it claims",
        bodies.len()
    );

    for body in &bodies {
        let expected: Value = serde_json::from_slice(body)
            .unwrap_or_else(|e| panic!("{rel} holds a frame whose body is not JSON: {e}"));
        let decoded: T = serde_json::from_slice(body).unwrap_or_else(|e| {
            let frame = String::from_utf8_lossy(body);
            panic!(
                "A PRE-RENAME PEER'S FRAME NO LONGER DECODES — this is a permanent wire break.\n\
                 fixture: {rel}\n  frame: {frame}\n  serde: {e}\nA `#[serde(rename)]` argument in \
                 {PROTO_SRC} was changed. Those strings are frozen: restore the argument, do not \
                 touch the fixture."
            )
        });
        let reencoded = serde_json::to_value(&decoded).expect("the wire types serialize");
        contains(&reencoded, &expected, "").unwrap_or_else(|why| {
            panic!(
                "THIS BUILD NO LONGER PRODUCES A PRE-RENAME PEER'S BYTES.\nfixture: {rel}\n  \
                 {why}\nA `#[serde(rename)]` argument in {PROTO_SRC} was changed, or a \
                 deserialize-only `alias` was added in place of one. Restore the rename."
            )
        });
    }
}

/// The REQUEST half: the two frozen request frames decode into this build's `Request`, and this
/// build re-encodes them unchanged.
#[test]
fn a_pre_rename_peers_request_frames_still_decode_and_re_encode() {
    assert_round_trip::<Request>(REQUEST_FRAMES, 2);
}

/// The RESPONSE half.
#[test]
fn a_pre_rename_peers_response_frames_still_decode_and_re_encode() {
    assert_round_trip::<Response>(RESPONSE_FRAMES, 2);
}

/// ⚠ THE COMPLETENESS HALF: every `#[serde(rename)]` argument the protocol source declares appears
/// verbatim in the frozen bytes.
///
/// Without it the two round-trip tests above are only as good as the fixtures' COVERAGE, and a pin
/// added later — or a pin quietly dropped from a frame — would be guarded by nothing while both
/// tests stayed green. It reads `proto.rs` as TEXT and spells no expected string of its own: the
/// expectation is "whatever the source pins, the frozen bytes contain", so a rename pass that
/// rewrites a pin reddens this too, with a message naming the argument that moved.
///
/// A NEW pin therefore arrives with a frozen frame carrying it, which is the rule this protocol
/// wants anyway: a wire tag nobody captured is a wire tag nobody is holding still.
#[test]
fn every_pinned_wire_tag_is_covered_by_the_fixtures() {
    let src = String::from_utf8(read_repo(PROTO_SRC)).expect("proto.rs is UTF-8");
    let mut frozen_bytes = read_repo(REQUEST_FRAMES);
    frozen_bytes.extend_from_slice(&read_repo(RESPONSE_FRAMES));
    // Lossy on purpose: only the four-byte length prefixes are non-UTF-8, every body is ASCII JSON,
    // and a substring search over the whole file is exactly the question being asked.
    let frozen = String::from_utf8_lossy(&frozen_bytes).into_owned();

    // ATTRIBUTE lines only — a `rename` written inside a doc comment is prose ABOUT a pin, not a
    // pin, and `proto.rs` quotes its own attributes in several doc blocks.
    const OPEN: &str = "rename = \"";
    let mut pins: Vec<String> = Vec::new();
    for line in src.lines() {
        let trimmed = line.trim_start();
        if !trimmed.starts_with("#[serde(") {
            continue;
        }
        let mut rest = trimmed;
        while let Some(i) = rest.find(OPEN) {
            rest = &rest[i + OPEN.len()..];
            let Some(end) = rest.find('"') else { break };
            pins.push(rest[..end].to_string());
            rest = &rest[end..];
        }
    }
    pins.sort();
    pins.dedup();

    assert!(
        !pins.is_empty(),
        "no `#[serde(rename)]` attribute was found in {PROTO_SRC} — either every wire-tag pin was \
         deleted (a permanent wire break) or this scanner stopped matching the source's shape. \
         Both are failures; neither is a reason to weaken the scan."
    );

    let missing: Vec<&String> =
        pins.iter().filter(|p| !frozen.contains(&format!("\"{p}\""))).collect();
    assert!(
        missing.is_empty(),
        "{PROTO_SRC} pins wire tag(s) that no frozen frame carries: {missing:?}\nEither a pin's \
         ARGUMENT was changed — restore it, the frozen bytes are the authority — or a NEW pin \
         shipped without a captured frame, in which case capture one into {REQUEST_FRAMES} / \
         {RESPONSE_FRAMES} (big-endian u32 length, then the JSON body).\nPins declared: {pins:?}"
    );
}
