use super::{ClockDeclarationKind, ClockRisk, VenueAdapterDeclarationView, Withhold};
use crate::GridSource;

/// The view's bytes are the lease wire's arming-table facts, byte for byte: the frame
/// `fixtures/gatewayhub_wire/v1_responses.jsonl` freezes (its `dukascopy` row) reads into it and
/// writes back unchanged, and an older daemon's row without the two defaulted keys still reads.
#[test]
fn the_view_reads_and_writes_the_frozen_facts_row() {
    let frozen = r#"{"adapter_present":true,"addresses_accounts":true,"takes_reconcile_trigger":false,"grid_source":"NoGrid","process_exclusive":"jforex-sidecar","clock_declaration_word":"none"}"#;
    let view: VenueAdapterDeclarationView = serde_json::from_str(frozen).expect("decodes");
    assert_eq!(view.clock_declaration_word, ClockDeclarationKind::Unknown("none".into()));
    assert_eq!(serde_json::to_string(&view).expect("encodes"), frozen);
    let older = r#"{"adapter_present":false,"addresses_accounts":false,"takes_reconcile_trigger":false,"grid_source":"InHand"}"#;
    let view: VenueAdapterDeclarationView = serde_json::from_str(older).expect("decodes");
    assert_eq!((view.process_exclusive, view.clock_declaration_word.word()), (None, ""));
}

/// `absent()` is the one spelling of a venue this build does not carry (it was written twice).
#[test]
fn an_absent_venue_has_no_adapter_no_grid_and_the_word_absent() {
    let absent = VenueAdapterDeclarationView::absent();
    assert!(
        !absent.adapter_present && !absent.addresses_accounts && !absent.takes_reconcile_trigger
    );
    assert_eq!(absent.grid_source, GridSource::NoGrid);
    assert_eq!(
        (absent.process_exclusive, absent.clock_declaration_word),
        (None, ClockDeclarationKind::Absent)
    );
}

/// The clock word is the lease wire's: the three known words read and write as themselves, and any
/// other word reads as `Unknown` holding it and writes back unchanged, never an error.
#[test]
fn the_clock_word_reads_and_writes_the_wire_words() {
    let known = [
        (ClockDeclarationKind::Wired, "\"wired\""),
        (ClockDeclarationKind::NotWired, "\"not-wired\""),
        (ClockDeclarationKind::Absent, "\"absent\""),
    ];
    for (kind, json) in known {
        assert_eq!(serde_json::to_string(&kind).expect("encodes"), json);
        assert_eq!(serde_json::from_str::<ClockDeclarationKind>(json).expect("decodes"), kind);
    }
    for word in ["measured", "not-checked", "unknown", ""] {
        let json = serde_json::to_string(word).expect("a string");
        let kind: ClockDeclarationKind =
            serde_json::from_str(&json).expect("an unknown word decodes");
        assert_eq!(kind, ClockDeclarationKind::Unknown(word.to_string()));
        assert_eq!(serde_json::to_string(&kind).expect("encodes"), json);
    }
    assert_eq!(ClockDeclarationKind::default(), ClockDeclarationKind::Unknown(String::new()));
}

/// The enums' tags are their variant names, the lease wire's spelling.
#[test]
fn the_enum_tags_are_the_wire_words() {
    let tags = |v: serde_json::Value| v.as_str().expect("a unit tag").to_string();
    let risks = [ClockRisk::SignedTimestamp, ClockRisk::NonceWindow, ClockRisk::NoTimestamp]
        .map(|r| tags(serde_json::to_value(r).expect("encodes")));
    assert_eq!(risks, ["SignedTimestamp", "NonceWindow", "NoTimestamp"]);
    let withholds = [Withhold::PreflightFailed, Withhold::DataOnly]
        .map(|w| tags(serde_json::to_value(w).expect("encodes")));
    assert_eq!(withholds, ["PreflightFailed", "DataOnly"]);
    let grids = [GridSource::InHand, GridSource::PerSymbolFetch, GridSource::NoGrid]
        .map(|g| tags(serde_json::to_value(g).expect("encodes")));
    assert_eq!(grids, ["InHand", "PerSymbolFetch", "NoGrid"]);
}
