//! THE `paramscan` VERBS' WIRE GOLDEN: committed length-prefixed frames of the four `paramscan` tags and the
//! `paramscan` key, round-tripped through this build's `Request` / `Response`.
//!
//! Until the owner's ruling 20 (phase 4, 2026-11-12) these frames were a FROZEN capture of a pre-rename
//! peer, and `crates/vike-backtesthub-client/src/proto.rs` kept the old `sweep` spelling on the wire with
//! five `#[serde(rename)]` pins. The ruling put the word `paramscan` on the wire too: the pins are gone,
//! a pre-rename peer can no longer be decoded (both ends deploy together), and the frames were
//! RE-RECORDED once, in the same change, to this build's tags (`RunParamscan`, `RunParamscanProfile`,
//! `ParamscanResult`, `ParamscanReport`, the `paramscan` key). They stay under `fixtures/`, out of reach of
//! an edit under `crates/`, and the framing read fails loudly on a corrupted length prefix.
//!
//! # Why the re-encode check is a SUBSET and not byte equality
//!
//! Byte equality would redden on a legitimate ADDITIVE field (v6's `params`, v7's `search`) and force a
//! re-record for a change that breaks no peer. So everything a recorded frame contains must still be
//! produced under the same names; a new key beside it is allowed, a renamed key never is.

use std::io::{Cursor, ErrorKind};
use std::path::PathBuf;

use serde_json::Value;
use vike_backtesthub_client::proto::{Request, Response};
use vike_hub_protocol::frame::read_frame_raw;
// `expected` ⊆ `actual`, recursively — the asymmetry is the whole point (module doc). Shared with the
// other protocol tag suites; `Err` names the first divergence's path rather than dumping two documents.
use vike_hub_protocol::test_support::contains;

/// The REQUEST frames: `RunParamscanProfile`, then the Studio verb whose own struct-variant
/// FIELD key is wire-visible too.
///
/// ⚠ Spelled as a REPO-RELATIVE path in a module-level `const` deliberately, and read at RUNTIME
/// rather than through `include_bytes!`: that shape is what puts it inside
/// `crates/vike-ops/tests/hygiene/path_key_gate.rs`'s scan (which excludes include arguments by
/// construction), so a moved or deleted fixture reddens a second, independent gate as well as this
/// file.
const REQUEST_FRAMES: &str = "fixtures/datahub_wire/v7_paramscan_rename_requests.frames";

/// The RESPONSE frames: `ParamscanResult` and `ParamscanReport`.
const RESPONSE_FRAMES: &str = "fixtures/datahub_wire/v7_paramscan_rename_responses.frames";

/// Repo-root-relative read. `CARGO_MANIFEST_DIR` (never CWD), the idiom every parity suite in this
/// workspace uses — see `crates/vike-execution/tests/parity/r5_parity.rs`.
fn read_repo(rel: &str) -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").join(rel);
    std::fs::read(&path).unwrap_or_else(|e| {
        panic!(
            "{rel} could not be read ({e}).\nThese bytes are the recorded frames of this build's \
             `paramscan` tags. Restore it from git."
        )
    })
}

/// Every frame BODY in the fixture at `rel`, read through the real framing reader — so a corrupted
/// length prefix fails HERE, loudly, instead of being silently re-interpreted.
fn frame_bodies(rel: &str) -> Vec<Vec<u8>> {
    let mut cursor = Cursor::new(read_repo(rel));
    let mut out = Vec::new();
    loop {
        match read_frame_raw(&mut cursor) {
            Ok(body) => out.push(body),
            Err(e) if e.kind() == ErrorKind::UnexpectedEof && !out.is_empty() => break,
            Err(e) => panic!("{rel} is not a well-framed stream after {} frames: {e}", out.len()),
        }
    }
    out
}

/// Decode each recorded frame as `T`, re-encode it, and require the recorded document to survive the
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
                "A RECORDED FRAME NO LONGER DECODES — a wire tag or key moved.\n\
                 fixture: {rel}\n  frame: {frame}\n  serde: {e}"
            )
        });
        let reencoded = serde_json::to_value(&decoded).expect("the wire types serialize");
        contains(&reencoded, &expected, "", "recorded frame").unwrap_or_else(|why| {
            panic!(
                "THIS BUILD NO LONGER PRODUCES A RECORDED FRAME'S BYTES.\nfixture: {rel}\n  {why}"
            )
        });
    }
}

/// The REQUEST half: the two recorded request frames decode into this build's `Request`, and this
/// build re-encodes them unchanged.
#[test]
fn the_recorded_request_frames_decode_and_re_encode() {
    assert_round_trip::<Request>(REQUEST_FRAMES, 2);
}

/// The RESPONSE half.
#[test]
fn the_recorded_response_frames_decode_and_re_encode() {
    assert_round_trip::<Response>(RESPONSE_FRAMES, 2);
}
