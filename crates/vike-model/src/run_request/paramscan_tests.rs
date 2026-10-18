use super::*;

/// A pre-parse over a fixed `(name, shape)` list, the way a caller hands its lookup over.
fn answer(found: &[(&str, SectionShape)]) -> Option<bool> {
    declares_a_grid(|name| found.iter().find(|(n, _)| *n == name).map(|(_, s)| *s))
}

const FULL: SectionShape = SectionShape::Table { empty: false };
const EMPTY: SectionShape = SectionShape::Table { empty: true };

/// The two spellings, and their ORDER: the current name is looked up first.
#[test]
fn the_two_spellings_are_paramscan_then_sweep() {
    assert_eq!(PARAMSCAN_SECTIONS, ["paramscan", "sweep"]);
}

/// "Present AND non-empty", under each spelling alone.
#[test]
fn a_grid_is_a_present_non_empty_table_under_either_spelling() {
    assert_eq!(answer(&[]), Some(false), "absent is no grid");
    for name in PARAMSCAN_SECTIONS {
        assert_eq!(answer(&[(name, FULL)]), Some(true), "[{name}] with keys is a grid");
        assert_eq!(answer(&[(name, EMPTY)]), Some(false), "a bare [{name}] is no grid");
        assert_eq!(
            answer(&[(name, SectionShape::NotATable)]),
            None,
            "{name} of the wrong shape is the engine's to refuse"
        );
    }
}

/// The first spelling present wins, exactly as the pre-parse it replaced did (`.get("paramscan")
/// .or_else(|| .get("sweep"))`): an EMPTY `paramscan` is not skipped in favour of a full `sweep`.
#[test]
fn the_first_spelling_present_decides() {
    assert_eq!(answer(&[("paramscan", EMPTY), ("sweep", FULL)]), Some(false));
    assert_eq!(answer(&[("paramscan", FULL), ("sweep", EMPTY)]), Some(true));
    assert_eq!(answer(&[("paramscan", SectionShape::NotATable), ("sweep", FULL)]), None);
    assert_eq!(answer(&[("sweep", FULL), ("elsewhere", FULL)]), Some(true));
}
