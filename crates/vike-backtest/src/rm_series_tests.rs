use super::*;
use vike_data::store::removal::{RemovalPlan, SeriesSelector};

fn plan(matched: usize) -> RemovalPlan {
    let series = (0..matched)
        .map(|i| {
            vike_data::store::removal::PlannedSeries::new(
                vike_data::SeriesId::per_symbol(
                    "bar",
                    "hyperliquid",
                    format!("S{i}"),
                    Some("1h".to_string()),
                ),
                Default::default(),
                vec!["panel_bars:1".to_string()],
                Some("panel_bars:"),
            )
        })
        .collect();
    RemovalPlan {
        selector: SeriesSelector::new("bar", "hyperliquid"),
        produced_by: Some("panel_bars:".to_string()),
        series,
    }
}

/// `--yes` is the non-interactive form and the ONLY one — a deliberate, greppable token in
/// shell history.
#[test]
fn yes_confirms_with_or_without_a_terminal() {
    confirm_removal(&plan(3), true, false).unwrap();
    confirm_removal(&plan(3), true, true).unwrap();
}

/// ⚠ **The rule this verb exists to keep.** No `--yes` and no terminal REFUSES; it never falls
/// back to reading, because `yes | backtest data rm …` is indistinguishable from a person
/// once you have decided to read a pipe.
#[test]
fn no_yes_and_no_terminal_refuses_rather_than_reading() {
    let err = confirm_removal(&plan(3), false, false).unwrap_err();
    assert!(err.contains("not a terminal"), "{err}");
    assert!(err.contains("3 series"), "the refusal names what it did not delete: {err}");
}
