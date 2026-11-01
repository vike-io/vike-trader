use super::*;

/// ⚠ **The roster, the resolver and the refusal are ONE thing — proven, because a mutation
/// showed they were three.**
///
/// The hole this closes, in the order it was found: `DataCfg::on_gap` matched three string arms;
/// its refusal spelled the same three by hand; and `crates/vike-cli/src/surface.rs`'s
/// `gap_dispositions` row spelled them a third time. A CLI-side test compared the roster it
/// PARSED OUT OF THE REFUSAL to the surface row — and a fourth arm planted in the resolver left
/// that test GREEN, because the message it read is a copy too. Copy-against-copy is not a gate.
///
/// Now [`OnGap::parse`] walks [`OnGap::NAMES`] instead of matching, the refusal renders
/// [`OnGap::roster`], and this test holds the last edge: every name resolves, and every
/// resolved value names itself back. Combined with [`OnGap::name`] being an exhaustive `match`
/// on the VARIANT — a new variant fails to compile there — a spelling cannot be accepted
/// without a row, and a row cannot exist without a variant that answers for it.
#[test]
fn the_coverage_rosters_are_the_one_door_their_resolvers_walk() {
    for name in OnGap::NAMES {
        let v = OnGap::parse(name)
            .unwrap_or_else(|| panic!("`{name}` is in NAMES and parse refuses it"));
        assert_eq!(v.name(), name, "`{name}` resolves to a value that names itself differently");
    }
    for name in UniverseMode::NAMES {
        let v = UniverseMode::parse(name)
            .unwrap_or_else(|| panic!("`{name}` is in NAMES and parse refuses it"));
        assert_eq!(v.name(), name, "`{name}` resolves to a value that names itself differently");
    }
    // The acceptance is case- and space-insensitive, which is what the CLI's own
    // `flag_vocab` ascii_ci rule relies on — and it is the roster that decides, not a
    // second lowercase list.
    assert_eq!(OnGap::parse("  REFUSE "), Some(OnGap::Refuse));
    assert_eq!(UniverseMode::parse("Strict"), Some(UniverseMode::Strict));
    // ...and nothing outside the roster is accepted by any path.
    assert_eq!(OnGap::parse("fill"), None);
    assert_eq!(UniverseMode::parse("all"), None);
    // The rendered set is the roster verbatim, so no refusal can spell it by hand again.
    assert_eq!(OnGap::roster(), "refuse | warn | run");
    assert_eq!(UniverseMode::roster(), "declared | covered | strict");
}

/// The spellings are the shipped ones, in the shipped order: the CLI rows, the refusal sentences
/// and the docs asset all render these arrays, so a reorder is a visible change to published text.
#[test]
fn the_rosters_keep_their_shipped_spellings_and_order() {
    assert_eq!(OnGap::NAMES, ["refuse", "warn", "run"]);
    assert_eq!(UniverseMode::NAMES, ["declared", "covered", "strict"]);
    assert!(!UniverseMode::Declared.consults_the_store());
    assert!(UniverseMode::Covered.consults_the_store());
    assert!(UniverseMode::Strict.consults_the_store());
}
