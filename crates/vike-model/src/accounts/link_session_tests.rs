use std::collections::BTreeSet;

use super::*;

fn every_field_set() -> LinkSessionRow {
    LinkSessionRow {
        venue: "aster".to_string(),
        route: "aster".to_string(),
        plane: "trade".to_string(),
        lane: "aster-userdata".to_string(),
        opened_ms: 1_000,
        closed_ms: Some(907_000),
        lived_s: Some(906.0),
        how: Some("server closed: 1000".to_string()),
        ours: true,
        next_open_s: Some(1.5),
    }
}

/// **The redaction check, moved from the compiler to here (owner's ruling, 2026-11-11).** The row the
/// store reads IS the row an Observe key reads, so this set is everything a history reader may see.
/// A field added to [`LinkSessionRow`] fails this test: decide it is words or numbers, never a URL or
/// a key, before adding its name here.
#[test]
fn a_row_reaches_the_wire_as_exactly_the_allowed_keys() {
    let json = serde_json::to_value(every_field_set()).expect("a row encodes");
    let keys: BTreeSet<&str> =
        json.as_object().expect("a row is an object").keys().map(String::as_str).collect();
    let allowed = BTreeSet::from([
        "venue",
        "route",
        "plane",
        "lane",
        "opened_ms",
        "closed_ms",
        "lived_s",
        "how",
        "ours",
        "next_open_s",
    ]);
    assert_eq!(keys, allowed, "a link-session field reaches an Observe-scope wire: {json}");
}

/// The bytes the wire copy wrote: the key order, and an open session's absent numbers absent.
#[test]
fn a_row_keeps_the_bytes_the_wire_copy_wrote() {
    assert_eq!(
        serde_json::to_string(&every_field_set()).expect("encode"),
        r#"{"venue":"aster","route":"aster","plane":"trade","lane":"aster-userdata","opened_ms":1000,"closed_ms":907000,"lived_s":906.0,"how":"server closed: 1000","ours":true,"next_open_s":1.5}"#
    );
    let open = LinkSessionRow {
        closed_ms: None,
        lived_s: None,
        how: None,
        ours: false,
        next_open_s: None,
        ..every_field_set()
    };
    let json = serde_json::to_string(&open).expect("encode");
    assert_eq!(
        json,
        r#"{"venue":"aster","route":"aster","plane":"trade","lane":"aster-userdata","opened_ms":1000,"ours":false}"#
    );
    assert_eq!(serde_json::from_str::<LinkSessionRow>(&json).expect("decode"), open);
    let bare = r#"{"venue":"aster","route":"aster","plane":"trade","lane":"x","opened_ms":1}"#;
    assert!(!serde_json::from_str::<LinkSessionRow>(bare).expect("an absent `ours` reads").ours);
}
