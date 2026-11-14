use super::*;

/// A pre-parse over a fixed `(name, shape)` list, the way a caller hands its lookup over.
fn answer(found: &[(&str, ParamscanSectionShape)]) -> Option<bool> {
    declares_a_grid(|name| found.iter().find(|(n, _)| *n == name).map(|(_, s)| *s))
}

const FULL: ParamscanSectionShape = ParamscanSectionShape::Table { empty: false };
const EMPTY: ParamscanSectionShape = ParamscanSectionShape::Table { empty: true };

/// One spelling since the owner's ruling 20 (phase 4).
#[test]
fn the_one_spelling_is_paramscan() {
    assert_eq!(PARAMSCAN_SECTIONS, ["paramscan"]);
}

/// "Present AND non-empty".
#[test]
fn a_grid_is_a_present_non_empty_table() {
    assert_eq!(answer(&[]), Some(false), "absent is no grid");
    for name in PARAMSCAN_SECTIONS {
        assert_eq!(answer(&[(name, FULL)]), Some(true), "[{name}] with keys is a grid");
        assert_eq!(answer(&[(name, EMPTY)]), Some(false), "a bare [{name}] is no grid");
        assert_eq!(
            answer(&[(name, ParamscanSectionShape::NotATable)]),
            None,
            "{name} of the wrong shape is the engine's to refuse"
        );
    }
}

/// A `[sweep]` table is no grid since ruling 20: the pre-parse looks under `paramscan` only.
#[test]
fn a_sweep_table_is_no_grid() {
    let old_word = "sweep";
    assert_eq!(answer(&[(old_word, FULL)]), Some(false));
    assert_eq!(answer(&[("paramscan", EMPTY), (old_word, FULL)]), Some(false));
}
