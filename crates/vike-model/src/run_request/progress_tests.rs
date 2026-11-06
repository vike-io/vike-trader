use super::*;

/// Every spelling `--progress` accepts, round-tripped, and the default is `Auto`. The refusal
/// SENTENCE is the caller's (`crates/vike-backtest/src/search/select.rs`'s `resolve_progress`);
/// the grammar is here.
#[test]
fn every_progress_spelling_parses_and_the_roster_is_complete() {
    for name in ProgressMode::NAMES {
        assert!(ProgressMode::from_str_ci(name).is_some(), "{name} is in NAMES and must parse");
        assert!(ProgressMode::from_str_ci(&name.to_ascii_uppercase()).is_some(), "{name}: ci");
    }
    assert_eq!(ProgressMode::from_str_ci("verbose"), None);
    assert_eq!(ProgressMode::default(), ProgressMode::Auto);

    // ⚠ This block's comment used to read "a fourth variant added without a NAMES row reddens
    // here" over `for mode in [ProgressMode::Auto, ProgressMode::Off, ProgressMode::Json]` — a
    // hand-typed literal, which is the one shape that cannot deliver that. A fourth variant
    // forces no edit in `from_str_ci` either (its `_ => None` arm is mandatory), so the roster
    // could have accepted a mode the refusal never names while this stayed green. The ARGUMENT
    // was right and is kept: the roster is what the refusal renders, so an accepted-but-
    // unlisted mode would be invisible in the message.
    //
    // The forcing function is the exhaustive `match` below, not `ALL_MODES` — the idiom
    // `crates/vike-exec/tests/recon/recon_policy_pin.rs` uses for `DivergenceKind`, where the
    // array "is what makes the assertion iterate" and the match is what makes a new variant a
    // COMPILE error. Each of the three ways to add a variant halfway is caught: no match arm is
    // a compile error here; an arm whose spelling is not in `NAMES` fails the `contains`; and a
    // variant absent from `ALL_MODES` fails the length agreement at the end.
    const ALL_MODES: [ProgressMode; 3] =
        [ProgressMode::Auto, ProgressMode::Off, ProgressMode::Json];
    for mode in ALL_MODES {
        let spelling = match mode {
            ProgressMode::Auto => "auto",
            ProgressMode::Off => "none",
            ProgressMode::Json => "json",
        };
        assert!(
            ProgressMode::NAMES.contains(&spelling),
            "{mode:?} spells {spelling:?}, which NAMES does not offer"
        );
        assert_eq!(
            ProgressMode::from_str_ci(spelling),
            Some(mode),
            "{spelling:?} is listed for {mode:?} and must parse back to it"
        );
    }
    assert_eq!(
        ALL_MODES.len(),
        ProgressMode::NAMES.len(),
        "a variant or a spelling was added without the other — every mode owes one name"
    );
}

/// The spellings are the shipped ones, in the shipped order: the usage line and the refusal
/// render this array joined by `|`.
#[test]
fn the_progress_roster_keeps_its_shipped_order() {
    assert_eq!(ProgressMode::NAMES, ["auto", "none", "json"]);
}
