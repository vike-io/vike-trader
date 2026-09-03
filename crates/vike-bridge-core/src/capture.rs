//! Sanitized real-frame capture — the shared test-support surface behind the `capture` feature
//! (testing-arch plan §5, PR 5a; the mechanized successor to the hand-pasted
//! `deribit/tests/offline/combo_fill_fixtures.rs` probe→fixture flow, and the in-repo wire-fidelity
//! source of truth now that the Python-oracle r6 exporters are retiring).
//!
//! ## Contract
//! [`capture_frame`] takes a VERBATIM venue wire frame (`serde_json::Value`) and returns it with
//! wire STRUCTURE preserved exactly — field names, value types, nesting, casing — because the
//! structure is the drift-sensitive part a hand-authored fixture can never witness. Only the
//! known-sensitive leaf set (api keys, `listenKey`, signatures/tokens, account/user ids — see
//! [`REDACT_KEYS`]) is redacted, to STABLE placeholders: the placeholder depends on the KEY, never
//! the secret value, so two captures with different session keys sanitize to identical bytes and a
//! re-capture diff shows real wire drift, not secret churn. Venue order ids, trade ids, prices,
//! quantities, and timestamps are DELIBERATELY kept — they are the fixture's fidelity (the deribit
//! template commits real `trade_id:"258544119"`), and demo-account order ids identify nothing.
//!
//! ## File format (the `<kind>.json` a [`FrameCapture`] writes)
//! ```json
//! {
//!   "_provenance": { "captured_at_utc": "...", "venue": "...", "kind": "...", "source": "...",
//!                     "sanitizer": "...", "frame_count": 2, "redacted_paths": [".."] },
//!   "frames": [ { "..": "sanitized frames, emission order" } ]
//! }
//! ```
//! Written under `crates/bridges/<venue>/tests/fixtures/captured/` by the venue's opt-in
//! (`VIKE_CAPTURE_FIXTURES=1`) capture smoke arm, committed, and consumed by (a) that venue's
//! normal CI'd `captured_wire_replay` test and (b) the cross-bridge conformance harness's
//! captured-template sourcing (plan 5c). Key ORDER inside a frame is serde_json-normalized
//! (sorted) on write — JSON key order is not wire semantics, and sorting keeps re-capture diffs
//! minimal; names/types/nesting are untouched.

use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

/// The known-sensitive leaf KEY set, matched case-insensitively against the exact key (never a
/// substring — `positionIdx` must not trip on `id`). Small and enumerable by design: secrets
/// already never reach `Debug`/logs (CLAUDE.md credential rule), so this list only needs to cover
/// what a venue's own wire frames echo back. Extend it in the SAME commit that first captures a
/// venue whose frames carry a new sensitive key.
pub const REDACT_KEYS: &[&str] = &[
    // api credentials + signatures
    "apikey",
    "api_key",
    "apisecret",
    "api_secret",
    "secret",
    "signature",
    "sign",
    "passphrase",
    "password",
    // session/auth tokens
    "listenkey",
    "listen_key",
    "token",
    "accesstoken",
    "access_token",
    "authorization",
    // account / user identity
    "uid",
    "userid",
    "user_id",
    "accountid",
    "account_id",
    "memberid",
    "member_id",
];

/// The sanitizer identity stamped into every fixture's provenance — bump when the redaction
/// contract changes so a reviewer can tell which rules produced a committed file.
pub const SANITIZER_VERSION: &str = "vike-bridge-core::capture v1";

/// One sanitized frame: the venue/kind routing tags, the structure-verbatim frame, and the audit
/// trail of what was redacted (JSON-pointer paths).
#[derive(Debug, Clone)]
pub struct SanitizedFrame {
    pub venue: String,
    pub kind: String,
    pub frame: Value,
    pub redacted_paths: Vec<String>,
}

/// Sanitize one raw venue frame (see the module doc for the exact contract). Pure — no I/O.
pub fn capture_frame(venue: &str, kind: &str, raw: &Value) -> SanitizedFrame {
    let mut frame = raw.clone();
    let mut redacted_paths = Vec::new();
    sanitize_in_place(&mut frame, &mut String::new(), &mut redacted_paths);
    SanitizedFrame { venue: venue.to_string(), kind: kind.to_string(), frame, redacted_paths }
}

/// Recursive walk: redact object values whose KEY is in [`REDACT_KEYS`], recurse everywhere else.
/// Type-preserving placeholders — a string secret stays a string (`"[redacted:<key>]"`), a numeric
/// id stays a number (`0`); a sensitive OBJECT/ARRAY value is over-redacted to the string
/// placeholder (safe direction). Bool/null under a sensitive key carry no identity — left as-is.
fn sanitize_in_place(v: &mut Value, path: &mut String, redacted: &mut Vec<String>) {
    match v {
        Value::Object(map) => {
            for (k, child) in map.iter_mut() {
                let base = path.len();
                path.push('/');
                path.push_str(k);
                if is_sensitive_key(k) {
                    if let Some(placeholder) = placeholder_for(k, child) {
                        *child = placeholder;
                        redacted.push(path.clone());
                    }
                } else {
                    sanitize_in_place(child, path, redacted);
                }
                path.truncate(base);
            }
        }
        Value::Array(items) => {
            for (i, child) in items.iter_mut().enumerate() {
                let base = path.len();
                path.push('/');
                path.push_str(&i.to_string());
                sanitize_in_place(child, path, redacted);
                path.truncate(base);
            }
        }
        _ => {}
    }
}

fn is_sensitive_key(key: &str) -> bool {
    let lc = key.to_ascii_lowercase();
    REDACT_KEYS.contains(&lc.as_str())
}

/// The stable placeholder for a sensitive leaf: a function of the KEY only (lowercased), never the
/// value — so re-captures with rotated secrets are byte-identical. `None` = leave as-is (bool/null).
fn placeholder_for(key: &str, value: &Value) -> Option<Value> {
    match value {
        Value::Number(_) => Some(json!(0)),
        Value::String(_) | Value::Object(_) | Value::Array(_) => {
            Some(Value::String(format!("[redacted:{}]", key.to_ascii_lowercase())))
        }
        Value::Bool(_) | Value::Null => None,
    }
}

/// A capture session: accumulates sanitized frames (per kind, in emission order) during a live
/// smoke's capture arm, then writes one `<kind>.json` per kind. `source` is the human provenance
/// line ("binance spot DEMO (demo-ws-api.binance.com) — binance_capture_smoke ladder").
#[derive(Debug)]
pub struct FrameCapture {
    venue: String,
    source: String,
    frames: Vec<SanitizedFrame>,
}

impl FrameCapture {
    pub fn new(venue: impl Into<String>, source: impl Into<String>) -> Self {
        Self { venue: venue.into(), source: source.into(), frames: Vec::new() }
    }

    /// Sanitize + record one frame under `kind`. Returns the sanitized frame (borrowed) so a
    /// caller can log it.
    pub fn capture(&mut self, kind: &str, raw: &Value) -> &SanitizedFrame {
        let f = capture_frame(&self.venue, kind, raw);
        self.frames.push(f);
        self.frames.last().expect("just pushed")
    }

    /// Number of frames captured so far under `kind`.
    pub fn count(&self, kind: &str) -> usize {
        self.frames.iter().filter(|f| f.kind == kind).count()
    }

    /// Write one `<kind>.json` per captured kind into `fixtures_dir` (created if absent), each a
    /// full deterministic rewrite (re-capture = reviewable diff). Returns the written paths.
    /// `now_ms` is injected for testability; production callers pass `vike_model::clock::now_ms()`.
    pub fn write_all(&self, fixtures_dir: &Path, now_ms: i64) -> io::Result<Vec<PathBuf>> {
        std::fs::create_dir_all(fixtures_dir)?;
        let kinds: Vec<String> = {
            // first-seen order, deduped — one file per kind
            let mut seen = BTreeSet::new();
            self.frames
                .iter()
                .filter(|f| seen.insert(f.kind.clone()))
                .map(|f| f.kind.clone())
                .collect()
        };
        let mut written = Vec::new();
        for kind in kinds {
            let of_kind: Vec<&SanitizedFrame> =
                self.frames.iter().filter(|f| f.kind == kind).collect();
            let redacted: BTreeSet<&str> =
                of_kind.iter().flat_map(|f| f.redacted_paths.iter().map(String::as_str)).collect();
            let mut root = Map::new();
            root.insert(
                "_provenance".into(),
                json!({
                    "captured_at_utc": fmt_utc_ms(now_ms),
                    "venue": self.venue,
                    "kind": kind,
                    "source": self.source,
                    "sanitizer": SANITIZER_VERSION,
                    "frame_count": of_kind.len(),
                    "redacted_paths": redacted.iter().collect::<Vec<_>>(),
                }),
            );
            root.insert(
                "frames".into(),
                Value::Array(of_kind.iter().map(|f| f.frame.clone()).collect()),
            );
            let path = fixtures_dir.join(format!("{kind}.json"));
            let mut body =
                serde_json::to_string_pretty(&Value::Object(root)).expect("fixture serializes");
            body.push('\n');
            std::fs::write(&path, body)?;
            written.push(path);
        }
        Ok(written)
    }
}

/// A committed captured fixture, loaded back for replay: the provenance stamp + the sanitized
/// frames in emission order.
#[derive(Debug, Clone)]
pub struct CapturedFixture {
    pub provenance: Value,
    pub frames: Vec<Value>,
}

/// Load `<fixtures_dir>/<kind>.json`. `None` when the file is absent; panics (with the path) on a
/// malformed file — a committed fixture that no longer parses is a broken gate, not a skip.
pub fn load_captured(fixtures_dir: &Path, kind: &str) -> Option<CapturedFixture> {
    let path = fixtures_dir.join(format!("{kind}.json"));
    let body = std::fs::read_to_string(&path).ok()?;
    let root: Value = serde_json::from_str(&body)
        .unwrap_or_else(|e| panic!("malformed captured fixture {}: {e}", path.display()));
    let provenance = root
        .get("_provenance")
        .cloned()
        .unwrap_or_else(|| panic!("captured fixture {} missing _provenance", path.display()));
    let frames = root
        .get("frames")
        .and_then(|f| f.as_array())
        .unwrap_or_else(|| panic!("captured fixture {} missing frames[]", path.display()))
        .clone();
    Some(CapturedFixture { provenance, frames })
}

// -------------------------------------------------------------------------------------------------
// Deterministic structural mutation — the shared parser-robustness enumerator (testing-arch parser
// workstream). Deliberately NOT random: the same frame always yields the same mutation list, so a
// failing mutation is reproducible from its index alone, with no seed bookkeeping. The per-venue
// property harnesses (tests/mapper_props.rs in binance/bybit) run every mutation of every
// committed captured fixture through the real decoders and assert totality.
// -------------------------------------------------------------------------------------------------

/// One step in a JSON path — an object key or an array index.
#[derive(Debug, Clone)]
enum Step {
    Key(String),
    Idx(usize),
}

fn node_at<'a>(root: &'a Value, path: &[Step]) -> Option<&'a Value> {
    let mut cur = root;
    for s in path {
        cur = match s {
            Step::Key(k) => cur.get(k.as_str())?,
            Step::Idx(i) => cur.get(*i)?,
        };
    }
    Some(cur)
}

fn node_at_mut<'a>(root: &'a mut Value, path: &[Step]) -> Option<&'a mut Value> {
    let mut cur = root;
    for s in path {
        cur = match s {
            Step::Key(k) => cur.get_mut(k.as_str())?,
            Step::Idx(i) => cur.get_mut(*i)?,
        };
    }
    Some(cur)
}

/// Depth-first collection of every LEAF path (string/number/bool/null) and every ARRAY path.
/// Object iteration is sorted by construction (this workspace's serde_json has no
/// `preserve_order` feature, so `Map` is a BTreeMap — the same property `write_all` relies on
/// for stable fixture bytes), which is what makes the enumeration deterministic.
fn collect_paths(
    v: &Value,
    path: &mut Vec<Step>,
    leaves: &mut Vec<Vec<Step>>,
    arrays: &mut Vec<Vec<Step>>,
) {
    match v {
        Value::Object(map) => {
            for (k, child) in map {
                path.push(Step::Key(k.clone()));
                collect_paths(child, path, leaves, arrays);
                path.pop();
            }
        }
        Value::Array(items) => {
            arrays.push(path.clone());
            for (i, child) in items.iter().enumerate() {
                path.push(Step::Idx(i));
                collect_paths(child, path, leaves, arrays);
                path.pop();
            }
        }
        _ => leaves.push(path.clone()),
    }
}

/// DETERMINISTIC enumeration of structural near-misses of a JSON frame, for feeding a venue
/// decoder every almost-right wire shape a drifting or hostile venue could send. Per LEAF:
/// the key/element REMOVED, the value NULLED, a string↔number TYPE FLIP, the empty string, and
/// the number edges `1e308` / `-1` / `0`. Per ARRAY: truncated to empty. A mutation equal to the
/// original frame is skipped (nulling a null mutates nothing), so every returned frame differs
/// from the input. Same frame in, same `Vec` out — see `collect_paths` on why.
pub fn frame_mutations(frame: &Value) -> Vec<Value> {
    let mut leaves = Vec::new();
    let mut arrays = Vec::new();
    collect_paths(frame, &mut Vec::new(), &mut leaves, &mut arrays);

    let mut out = Vec::new();
    for path in &leaves {
        // Removal (a leaf at the root has no parent to remove it from).
        if let Some((last, parent)) = path.split_last() {
            let mut m = frame.clone();
            match (last, node_at_mut(&mut m, parent)) {
                (Step::Key(k), Some(Value::Object(map))) => {
                    map.remove(k);
                    out.push(m);
                }
                (Step::Idx(i), Some(Value::Array(items))) => {
                    items.remove(*i);
                    out.push(m);
                }
                _ => {}
            }
        }
        // Replacement, chosen by what the leaf currently is.
        let Some(current) = node_at(frame, path) else { continue };
        let replacements: Vec<Value> = match current {
            Value::String(_) => vec![Value::Null, json!(0), json!("")],
            Value::Number(n) => {
                vec![Value::Null, Value::String(n.to_string()), json!(1e308), json!(-1), json!(0)]
            }
            Value::Bool(_) | Value::Null => vec![Value::Null, json!("true"), json!(0)],
            // collect_paths never records an object/array as a leaf.
            Value::Object(_) | Value::Array(_) => vec![],
        };
        for r in replacements {
            if *current == r {
                continue; // nulling a null / zeroing a zero is not a mutation
            }
            let mut m = frame.clone();
            if let Some(slot) = node_at_mut(&mut m, path) {
                *slot = r;
                out.push(m);
            }
        }
    }
    for path in &arrays {
        match node_at(frame, path) {
            Some(Value::Array(items)) if !items.is_empty() => {}
            _ => continue, // truncating an already-empty array mutates nothing
        }
        let mut m = frame.clone();
        if let Some(Value::Array(items)) = node_at_mut(&mut m, path) {
            items.clear();
            out.push(m);
        }
    }
    out
}

/// Epoch ms → `"YYYY-MM-DDTHH:MM:SSZ"` (UTC), no chrono dependency. Civil-from-days per Howard
/// Hinnant's algorithm — exact for the whole i64-ms range this will ever see.
fn fmt_utc_ms(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let days = secs.div_euclid(86_400);
    let sod = secs.rem_euclid(86_400);
    let (h, m, s) = (sod / 3600, (sod % 3600) / 60, sod % 60);
    // civil_from_days
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mo <= 2 { y + 1 } else { y };
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A synthetic frame shaped like a real venue envelope: nesting, arrays, mixed leaf types,
    /// and sensitive leaves in three casings/types.
    fn raw() -> Value {
        json!({
            "e": "executionReport",
            "listenKey": "pqia91ma19a5s61cv6a81va65sdf19v8a65a1a5s61cv6a81va65sdf19v8a65a1",
            "event": {
                "apiKey": "AbC123",
                "user_id": 84789,
                "o": {
                    "s": "BTCUSDT",
                    "c": "vtr-coid-1",
                    "i": 4200421,
                    "L": "50123.45000000",
                    "l": "0.00012000",
                    "m": false,
                    "n": null,
                    "fills": [ {"t": 998877, "TOKEN": "sess-abc"} ]
                }
            }
        })
    }

    #[test]
    fn structure_is_preserved_verbatim_apart_from_sensitive_leaves() {
        let out = capture_frame("binance", "ws_fill", &raw()).frame;
        // Non-sensitive leaves: names, nesting, casing, TYPES all verbatim.
        assert_eq!(out["e"], json!("executionReport"));
        assert_eq!(out["event"]["o"]["s"], json!("BTCUSDT"));
        assert_eq!(out["event"]["o"]["c"], json!("vtr-coid-1"));
        assert_eq!(out["event"]["o"]["i"], json!(4200421), "venue order id KEPT (number)");
        assert_eq!(out["event"]["o"]["L"], json!("50123.45000000"), "string-typed px KEPT");
        assert_eq!(out["event"]["o"]["m"], json!(false));
        assert_eq!(out["event"]["o"]["n"], Value::Null);
        assert_eq!(out["event"]["o"]["fills"][0]["t"], json!(998877), "trade id KEPT");
    }

    #[test]
    fn sensitive_leaves_redact_to_stable_type_preserving_placeholders() {
        let sf = capture_frame("binance", "ws_fill", &raw());
        let out = &sf.frame;
        assert_eq!(out["listenKey"], json!("[redacted:listenkey]"));
        assert_eq!(out["event"]["apiKey"], json!("[redacted:apikey]"), "case-insensitive key");
        assert_eq!(out["event"]["user_id"], json!(0), "numeric id stays a NUMBER");
        assert_eq!(out["event"]["o"]["fills"][0]["TOKEN"], json!("[redacted:token]"));
        let mut paths = sf.redacted_paths.clone();
        paths.sort();
        assert_eq!(
            paths,
            vec!["/event/apiKey", "/event/o/fills/0/TOKEN", "/event/user_id", "/listenKey",]
        );
    }

    /// The stability law: two captures differing ONLY in secret values sanitize identically —
    /// a re-capture diff shows wire drift, never secret churn.
    #[test]
    fn output_is_stable_across_rotated_secrets() {
        let mut b = raw();
        b["listenKey"] = json!("ROTATED-KEY-xyz");
        b["event"]["apiKey"] = json!("other");
        b["event"]["user_id"] = json!(123456789);
        b["event"]["o"]["fills"][0]["TOKEN"] = json!("sess-def");
        let a = capture_frame("binance", "ws_fill", &raw()).frame;
        let b = capture_frame("binance", "ws_fill", &b).frame;
        assert_eq!(a, b);
    }

    /// Substring keys must NOT trip: `positionIdx` contains `id`, `signQty` contains `sign`
    /// only as a prefix — exact-key match only.
    #[test]
    fn non_sensitive_lookalike_keys_are_untouched() {
        let v = json!({"positionIdx": 1, "signQty": "0.5", "sideToken2": "x", "validUntil": 9});
        let out = capture_frame("bybit", "k", &v);
        assert_eq!(out.frame, v);
        assert!(out.redacted_paths.is_empty());
    }

    /// A sensitive OBJECT value over-redacts to the string placeholder (safe direction).
    #[test]
    fn sensitive_object_value_is_over_redacted_whole() {
        let v = json!({"token": {"inner": "s3cret"}, "ok": 1});
        let out = capture_frame("x", "k", &v).frame;
        assert_eq!(out["token"], json!("[redacted:token]"));
        assert_eq!(out["ok"], json!(1));
    }

    #[test]
    fn write_all_stamps_provenance_and_round_trips_in_emission_order() {
        let dir = std::env::temp_dir().join(format!(
            "vike-capture-test-{}-{}",
            std::process::id(),
            line!()
        ));
        let _ = std::fs::remove_dir_all(&dir);

        let mut cap = FrameCapture::new("binance", "unit-test double, not a venue");
        cap.capture("ws_fill", &json!({"seq": 1, "listenKey": "a"}));
        cap.capture("ws_accepted", &json!({"x": "NEW"}));
        cap.capture("ws_fill", &json!({"seq": 2}));
        assert_eq!(cap.count("ws_fill"), 2);

        // 2026-07-22T00:00:00Z = 1784678400000 ms — pins the no-chrono UTC formatter too.
        let written = cap.write_all(&dir, 1_784_678_400_000).unwrap();
        assert_eq!(written.len(), 2, "one file per kind");

        let fx = load_captured(&dir, "ws_fill").expect("ws_fill.json");
        assert_eq!(fx.frames.len(), 2);
        assert_eq!(fx.frames[0]["seq"], json!(1), "emission order preserved");
        assert_eq!(fx.frames[1]["seq"], json!(2));
        assert_eq!(fx.frames[0]["listenKey"], json!("[redacted:listenkey]"));
        let p = &fx.provenance;
        assert_eq!(p["captured_at_utc"], json!("2026-07-22T00:00:00Z"));
        assert_eq!(p["venue"], json!("binance"));
        assert_eq!(p["kind"], json!("ws_fill"));
        assert_eq!(p["sanitizer"], json!(SANITIZER_VERSION));
        assert_eq!(p["frame_count"], json!(2));
        assert_eq!(p["redacted_paths"], json!(["/listenKey"]));

        assert!(load_captured(&dir, "absent_kind").is_none(), "absent file → None");

        // Deterministic apart from captured_at: a second write with the same clock is byte-equal.
        let body1 = std::fs::read_to_string(dir.join("ws_fill.json")).unwrap();
        cap.write_all(&dir, 1_784_678_400_000).unwrap();
        let body2 = std::fs::read_to_string(dir.join("ws_fill.json")).unwrap();
        assert_eq!(body1, body2, "stable bytes for stable input");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The mutation enumerator: non-empty on a nested frame, known members present, never the
    /// identity, and deterministic (same frame in, same list out).
    #[test]
    fn frame_mutations_enumerates_structural_near_misses() {
        let frame = json!({"a": {"b": "x"}, "arr": [1, 2]});
        let muts = frame_mutations(&frame);
        assert!(!muts.is_empty());
        assert!(muts.contains(&json!({"a": {}, "arr": [1, 2]})), "key removal missing");
        assert!(muts.contains(&json!({"a": {"b": "x"}, "arr": []})), "array truncation missing");
        assert!(
            muts.contains(&json!({"a": {"b": 0}, "arr": [1, 2]})),
            "string→number flip missing"
        );
        assert!(muts.iter().all(|m| m != &frame), "a mutation equals the original frame");
        assert_eq!(muts, frame_mutations(&frame), "the enumeration must be deterministic");
    }

    /// Number leaves get the numeric edge set; a scalar root (no parent) still mutates.
    #[test]
    fn frame_mutations_number_edges_and_scalar_root() {
        let frame = json!({"q": 5});
        let muts = frame_mutations(&frame);
        assert!(muts.contains(&json!({})), "removal of the only key");
        assert!(muts.contains(&json!({"q": null})));
        assert!(muts.contains(&json!({"q": "5"})), "number→string flip");
        assert!(muts.contains(&json!({"q": 1e308})));
        assert!(muts.contains(&json!({"q": -1})));
        assert!(muts.contains(&json!({"q": 0})));

        let root = json!(true);
        let root_muts = frame_mutations(&root);
        assert!(root_muts.contains(&json!(null)));
        assert!(root_muts.iter().all(|m| m != &root));
    }
}
