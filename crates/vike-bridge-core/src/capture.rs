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

use serde_json::{Map, Value, json};

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
    // ⚠ **THE PARENT half of that identity, added 2026-09-20 BEFORE the capture that needs it.**
    //
    // The seven names above are all spellings of *which account am I*. A CEX answers a second
    // question beside it — *whose sub-account is this* — under a name none of them match, because
    // matching is EXACT (lowercased) and never a substring: `parentUid` lowercases to `parentuid`,
    // which was in no entry. So the venue field that says an account is somebody's SUB-ACCOUNT
    // would have been committed in the clear, in a fixture directory that ships to the public
    // mirror.
    //
    // That is not hypothetical and it is not a future venue: it is exactly the field the blind-CEX
    // work goes looking for. `vike_mount::book_identity`'s bybit/okx rows say the store names no
    // account and only an authenticated call can, and the parent id is what distinguishes a
    // sub-account from its master — so the FIRST capture taken to answer that question is the one
    // that would have leaked it. The module doc above asks for exactly this: *extend it in the SAME
    // commit that first captures a venue whose frames carry a new sensitive key* — this is that
    // commit, landed before the capture rather than after.
    //
    // ⚠ It stays an ENUMERATION, so the next unrecognised spelling passes through just as these
    // did. `_provenance.redacted_paths` is what a reviewer reads to see what was actually caught,
    // and reading the file before committing it remains the rule rather than a formality.
    "parentuid",
    "parent_uid",
    "mainuid",
    "main_uid",
    "masteruid",
    "master_uid",
    "subacct",
    "subaccount",
    "sub_account",
    // ⚠ **THE HUMAN half of that identity, added 2026-09-20 — again BEFORE the capture, and again
    // because a probe measured it rather than because a document predicted it.**
    //
    // `private/get_account_summary` with `extended: true` on a DERIBIT demo account answered
    // `email`, `username` and `system_name` — and `_provenance.redacted_paths` came back EMPTY,
    // because the sixteen names above are machine ids and none of these three is one. A capture of
    // that body would have committed an operator's real email address into a fixture directory that
    // ships to the public mirror.
    //
    // They are redacted for the same reason `uid` is: they NAME the account. `username` and
    // `system_name` are what the venue's own UI calls the account, and an email is worse than an
    // account id — it identifies the human, is reused off this venue, and is the one field here
    // whose exposure is not bounded by the account.
    "email",
    "username",
    "system_name",
    "systemname",
    // ⚠ **AND THE ONE THIS MECHANISM CANNOT COVER, stated because silence would read as safety.**
    //
    // Deribit's ACCOUNT id is spelled `id` — `result.id` in that same body, beside the JSON-RPC
    // envelope's own request `id`. `id` is NOT in this list and must never join it: it is the key
    // every venue order id, trade id and combo leg rides, and those are the fixture's FIDELITY (the
    // module doc above says so, and the deribit template commits a real `trade_id`). Redaction here
    // is by KEY and not by PATH, so there is no spelling that catches one `id` and spares the other.
    //
    // What that leaves is a REVIEW obligation rather than a rule: a deribit account-summary capture
    // carries an account number in the clear, and reading the file before committing it — which the
    // module doc already asks for — is the only thing standing in front of it. It is a number, not
    // a name, and it is now the only identifier in that body that is.
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
                    "captured_at_utc": vike_model::runs::utc_rfc3339(now_ms.div_euclid(1000)),
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

// ⚠ `fn fmt_utc_ms(ms: i64)` stood here and is DELETED, not moved. It was a hand copy of Howard
// Hinnant's civil-from-days — the same algorithm `crates/vike-model/src/time.rs` was consolidated
// to be the ONE home for. That consolidation lists where it cleaned the copies out (the `ig` and
// `polymarket` bridges, the backtest schedule and harness, the collectors); this file was simply
// missed, and this crate has named `vike-model` all along, so nothing had to be added to remove it.
//
// The replacement is `vike_model::runs::utc_rfc3339`, which takes SECONDS, hence the
// `div_euclid(1000)` at the call site. MEASURED 2026-09-23 before the swap: both spellings were
// run against `date -u` over 4,010 timestamps spanning 1970-2140 (including 2000-02-29/03-01,
// 2024-02-29 and 2100-02-28/03-01, the century-leap rule in both directions) and 605 pre-1970
// instants back to 1891 — zero disagreements, with a mutation planted to prove the comparison
// could fail at all.
//
// ONE behavioural difference, taken deliberately: for a year outside `0..=9999` the survivor
// returns the raw number instead of a date string, because a run manifest must never be killed by
// its own metadata. A capture timestamp is `vike_model::clock::now_ms`, so that branch is
// unreachable here in practice, and where it is reachable the fallback is the better answer.

#[path = "capture_tests.rs"]
#[cfg(test)]
mod capture_tests;
