//! **The C++↔Rust envelope contract**, gated on a box with no ForexConnect SDK.
//!
//! FXCM's async fill/cancel/reject lane crosses a language boundary as JSON built by `snprintf`
//! format literals in `crates/bridges/fxcm/src/shim/fcshim.cpp`'s `EventListener`, and decoded by
//! `crates/bridges/fxcm/src/event_mapper.rs`'s `map_fxcm_event`. Nothing typed either half against
//! the other. **No compiler on any CI runner has ever seen that .cpp file** — it is compiled only
//! under `--features fxcm` on a machine that also has the proprietary SDK staged, which is one
//! developer box — so a rename of `trade_id` to `tradeId` in the shim was a change no gate could
//! observe: the workspace stayed green, `--features fxcm` stayed green, and the fills stopped
//! arriving the next time somebody ran the live smoke.
//!
//! This file closes that with three checks, none of which needs the SDK:
//!
//! 1. [`the_committed_envelopes_match_the_shims_own_format_literals`] — re-reads the emitter's
//!    format literals out of the C++ SOURCE and compares their key sets against the committed
//!    `tests/fixtures/shim_events.json`. Keyed on the emitted keys themselves, not on the file
//!    mentioning an identifier somewhere.
//! 2. [`every_committed_envelope_decodes_to_the_events_it_names`] — drives the REAL mapper over the
//!    committed envelopes and pins what each produces.
//! 3. [`each_envelope_key_is_actually_read`] — per-key ABLATION. Removing a key must change the
//!    decode. A key present in both halves and read by neither is the same silent gap wearing a
//!    matching name, and a key-set comparison alone cannot see it.
//!
//! Pure: no session, no FFI, no credentials, no network. It runs in the DEFAULT lane, so it gates
//! every PR rather than only the ones somebody remembers to run the feature lane on.

use std::collections::BTreeSet;
use std::path::PathBuf;

use serde_json::Value;
use vike_fxcm::event_mapper::{map_drained_event, map_fxcm_event};
use vike_model::events::Event;

fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// The committed envelope set, keyed by shim `kind`.
fn envelopes() -> serde_json::Map<String, Value> {
    let path = crate_dir().join("tests/fixtures/shim_events.json");
    let body = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("missing shim envelope fixture {}: {e}", path.display()));
    let root: Value =
        serde_json::from_str(&body).unwrap_or_else(|e| panic!("malformed {}: {e}", path.display()));
    root.get("envelopes")
        .and_then(|e| e.as_object())
        .cloned()
        .unwrap_or_else(|| panic!("{} has no `envelopes` object", path.display()))
}

/// Every JSON key the shim's `snprintf` format literal for `kind` actually writes.
///
/// Reads the C++ SOURCE and slices each emitter literal from its `{\"kind\":\"<kind>\"` opener to
/// the `}` that closes it, then harvests every `\"IDENT\":` inside. The literal is split across two
/// adjacent C string literals for the fill envelope; adjacent-literal concatenation leaves only
/// `",` + whitespace + `"` between the halves, which carries no `\"IDENT\":` pattern of its own, so
/// the harvest spans the join without special-casing it.
fn keys_emitted_by_the_shim(kind: &str) -> BTreeSet<String> {
    let path = crate_dir().join("src/shim/fcshim.cpp");
    let src = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read the shim source {}: {e}", path.display()));

    let opener = format!("{{\\\"kind\\\":\\\"{kind}\\\"");
    let start = src.find(&opener).unwrap_or_else(|| {
        panic!(
            "no `{opener}` emitter literal in {}. The shim stopped emitting the `{kind}` envelope, \
             or spelled its opener differently — either way the committed fixture and \
             `event_mapper` are now describing something that does not exist.",
            path.display()
        )
    });
    let rest = &src[start..];
    let end = rest.find('}').unwrap_or_else(|| {
        panic!("the `{kind}` emitter literal in {} is unterminated", path.display())
    });
    harvest_keys(&rest[..=end])
}

/// Pull every `\"IDENT\":` key out of a C++ string-literal slice.
fn harvest_keys(literal: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let needle = "\\\"";
    let mut i = 0;
    while let Some(off) = literal[i..].find(needle) {
        let key_start = i + off + needle.len();
        let Some(close) = literal[key_start..].find(needle) else { break };
        let key = &literal[key_start..key_start + close];
        let after = key_start + close + needle.len();
        // A KEY is followed by `:`; a VALUE (`\"%s\"`, `\"fill\"`) is followed by `,` or `}`.
        if literal[after..].starts_with(':')
            && !key.is_empty()
            && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            out.insert(key.to_string());
        }
        i = key_start;
    }
    out
}

/// The keys of one committed envelope.
fn committed_keys(env: &Value) -> BTreeSet<String> {
    env.as_object().expect("each committed envelope is a JSON object").keys().cloned().collect()
}

/// (1) The committed envelopes ARE the shim's own grammar — key for key, in both directions.
///
/// Both directions matter and they fail for different reasons: a key the shim emits and the
/// fixture omits means this suite has been testing a frame the venue never sends; a key the fixture
/// carries and the shim does not means the decoder is being proven against a field that will never
/// arrive.
#[test]
fn the_committed_envelopes_match_the_shims_own_format_literals() {
    let committed = envelopes();
    assert!(!committed.is_empty(), "the fixture must carry at least one envelope");

    for (kind, env) in &committed {
        let emitted = keys_emitted_by_the_shim(kind);
        assert!(
            !emitted.is_empty(),
            "{kind}: harvested NO keys from the shim literal — the harvester is broken, which \
             would make every comparison below vacuous"
        );
        assert_eq!(
            committed_keys(env),
            emitted,
            "{kind}: the committed envelope and `fcshim.cpp`'s emitter disagree about which keys \
             cross the boundary. Whichever half changed, the other has to follow — and \
             `event_mapper.rs` reads the committed one."
        );
    }

    // …and the fixture is EXHAUSTIVE over the emitter: a fourth envelope kind added to the shim
    // must land here too, rather than being decoded by nobody.
    let src = std::fs::read_to_string(crate_dir().join("src/shim/fcshim.cpp")).unwrap();
    let emitted_kinds = src.match_indices("{\\\"kind\\\":\\\"").count();
    assert_eq!(
        emitted_kinds,
        committed.len(),
        "`fcshim.cpp` emits {emitted_kinds} envelope kind(s) but the fixture commits {}. A new \
         shim envelope needs a fixture row and a `map_fxcm_event` arm, or it is enqueued for a \
         decoder that drops it.",
        committed.len()
    );
}

/// (2) Each committed envelope decodes to the canonical events its `kind` names, with the
/// fixture's own values carried through — the shim's grammar folded by the REAL mapper.
#[test]
fn every_committed_envelope_decodes_to_the_events_it_names() {
    let committed = envelopes();

    let fill = &committed["fill"];
    let evs = map_fxcm_event(fill, "c-1");
    assert_eq!(evs.len(), 2, "a fill DUAL-publishes (bare Fill for the Account, then the wrap)");
    match &evs[0] {
        Event::Fill(f) => {
            assert_eq!(f.trade_id, "T90210");
            assert_eq!(f.client_order_id, "c-1");
            assert_eq!(f.symbol, "EURUSD", "the slash is stripped for the canonical symbol");
            assert_eq!(f.side, -1, "the shim's `\"S\"` is a SELL");
            assert_eq!(f.last_qty, 10_000.0, "`amount` is BASE UNITS, not the submitted lot count");
            assert!((f.last_px - 1.0912).abs() < 1e-9);
            assert!((f.commission - -0.08).abs() < 1e-9);
        }
        other => panic!("expected the bare Fill first, got {other:?}"),
    }
    assert!(matches!(&evs[1], Event::OrderFilled(w) if w.fill.trade_id == "T90210"));

    let canceled = map_fxcm_event(&committed["canceled"], "c-1");
    assert!(matches!(&canceled[..], [Event::OrderCanceled(c)] if c.client_order_id == "c-1"));

    let rejected = map_fxcm_event(&committed["rejected"], "c-2");
    assert!(matches!(&rejected[..], [Event::OrderRejected(r)]
            if r.client_order_id == "c-2" && r.reason == "rejected"));
}

/// Why removing `(kind, key)` cannot change the decode. `None` = it must change.
///
/// Checked in BOTH directions by the ablation below, so this is a ratchet rather than a mute
/// button: an exempt pair whose removal DOES change the decode fails the test too, because a stale
/// exemption silently un-covers a key that had become observable.
fn unobservable(kind: &str, key: &str) -> Option<&'static str> {
    match (kind, key) {
        (_, "ts") => Some(
            "`fcshim.cpp` writes the literal `\\\"ts\\\":0` in all three envelopes — it never reads \
             a venue clock. An FXCM fill's timestamp is therefore whatever the core stamps, and the \
             key cannot vary, so its absence is unobservable BY CONSTRUCTION.",
        ),
        ("rejected", "reason") => Some(
            "the shim's only reason string is the constant `\"rejected\"`, which is byte-identical \
             to `map_fxcm_event`'s own fallback — so the key carries no information and dropping it \
             is indistinguishable from keeping it. A shim that ever emits a REAL venue reason makes \
             this exemption stale, and the staleness half of the assertion is what says so.",
        ),
        _ => None,
    }
}

/// (3) PER-KEY ABLATION — every key on the wire is a key the decoder READS.
///
/// A matching key set (check 1) proves the two halves agree on the vocabulary; it says nothing
/// about whether the Rust half does anything with a given word. Removing a key must change the
/// decode, and the ablation runs through the seam that actually consumes each key: `order_id` is
/// the ROUTING key (`map_drained_event`), everything else is the decode's (`map_fxcm_event`) —
/// driving the outer seam exercises both.
#[test]
fn each_envelope_key_is_actually_read() {
    let committed = envelopes();
    let routes = |oid: &str| {
        let mut m = std::collections::HashMap::new();
        m.insert(oid.to_string(), "c-1".to_string());
        m
    };
    // Compared as Debug text: the assertion is "the decode changed", and every canonical event
    // renders its whole field set, so this is the same comparison without depending on which
    // `Event` variants happen to carry a derived `PartialEq`.
    let decode = |env: &Value, oid: &str| format!("{:?}", map_drained_event(env, &routes(oid)));

    for (kind, env) in &committed {
        let obj = env.as_object().unwrap();
        let oid = obj["order_id"].as_str().unwrap();
        let baseline = decode(env, oid);
        assert_ne!(baseline, "[]", "{kind}: the intact envelope must decode to something");

        for key in obj.keys() {
            let mut ablated = obj.clone();
            ablated.remove(key);
            let changed = decode(&Value::Object(ablated), oid) != baseline;
            match unobservable(kind, key) {
                Some(_why) => assert!(
                    !changed,
                    "{kind}: `{key}` is listed UNOBSERVABLE, but dropping it changed the decode — \
                     the exemption is stale. Delete its row; the ablation covers this key now."
                ),
                None => assert!(
                    changed,
                    "{kind}: dropping `{key}` changed NOTHING, so nothing reads it. Either \
                     `event_mapper` should be reading it, or `fcshim.cpp` should stop paying to \
                     emit it — a key carried by both halves and consumed by neither is exactly the \
                     gap this suite exists to find. If it is genuinely unobservable, give it an \
                     `unobservable()` row with the reason, the way `ts` has one."
                ),
            }
        }
    }
}

/// The harvester is the load-bearing half of check (1), so it gets its own positive AND negative
/// control: it must find the keys in a literal shaped like the shim's, and must NOT mistake a
/// VALUE for a key. Without this, a harvester that silently returned the empty set would make the
/// grammar comparison pass on anything (the `!emitted.is_empty()` assert there is the other guard).
#[test]
fn the_key_harvester_reads_keys_and_not_values() {
    let literal = r#"{\"kind\":\"fill\",\"order_id\":\"%s\",\"amount\":%d}"#;
    assert_eq!(
        harvest_keys(literal).into_iter().collect::<Vec<_>>(),
        vec!["amount".to_string(), "kind".to_string(), "order_id".to_string()],
        "keys are the tokens followed by `:` — `fill` and `%s` are VALUES and must not be harvested"
    );
    assert!(
        harvest_keys(r#"no literal here"#).is_empty(),
        "a slice with no escaped-quote pairs harvests nothing rather than guessing"
    );
}

/// The path the two checks above read must be the path that actually holds the shim — a citation
/// that has rotted makes every assertion above vacuous, and `unwrap_or_else(|| panic!(…))` on a
/// missing file is easy to mistake for a legitimately skipped test.
#[test]
fn the_shim_source_and_the_fixture_both_exist_where_this_suite_looks() {
    for rel in ["src/shim/fcshim.cpp", "tests/fixtures/shim_events.json"] {
        let p: PathBuf = crate_dir().join(rel);
        assert!(p.is_file(), "this suite reads {}, which is not there", p.display());
    }
}
