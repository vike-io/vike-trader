//! The right-tab strip and the QA capture hooks: tab cycling and parsing, the autorun knob, and the
//! sweep ladder / grid seeding the sweep arm depends on.

use super::super::*;
use super::support::{seeded_store, state_new};
use std::sync::Arc;
use vike_data::DataFusionHist;

/// `next_tab` cycles through every `RightTab::ALL` entry in display order and wraps from
/// the last tab back to the first — the pure helper behind the Ctrl/Cmd+/ shortcut.
#[test]
fn next_tab_cycles_through_every_tab_and_wraps() {
    let start = RightTab::ALL[0];
    let mut t = start;
    let mut seen = vec![t];
    for _ in 0..RightTab::ALL.len() - 1 {
        t = next_tab(t);
        seen.push(t);
    }
    assert_eq!(seen, RightTab::ALL.to_vec(), "should visit every tab in display order");
    assert_eq!(next_tab(t), start, "the last tab should wrap back to the first");
}

#[test]
fn right_tab_default_is_sweep_and_every_tab_has_a_label() {
    assert_eq!(RightTab::default(), RightTab::Sweep);
    assert_eq!(RightTab::ALL.len(), 7);
    for tab in RightTab::ALL {
        assert!(!tab.label().is_empty());
    }
    // Labels are distinct (no two tabs share a caption).
    let labels: Vec<&str> = RightTab::ALL.iter().map(|t| t.label()).collect();
    let mut uniq = labels.clone();
    uniq.sort_unstable();
    uniq.dedup();
    assert_eq!(uniq.len(), labels.len());
}

/// The label and the glyph each tab wears, pinned to what `RightTab::label` and `RightTab::icon`
/// answered before they moved to `ui-theme.toml`'s `studio_tab` map (the AI tab's label is "AI Chat").
/// Changing a row of the table changes the Studio's rail, and this test is what says so by name; it
/// also holds that every tab is listed, so a new one is not left out of the pin.
#[test]
fn a_tab_wears_the_label_and_the_glyph_the_studio_tab_map_gives_it() {
    let pinned = [
        (RightTab::Sweep, "Sweep", icons::SWEEP),
        (RightTab::Strategy, "Strategy", icons::STRATEGY),
        (RightTab::Data, "Data", icons::DATA),
        (RightTab::Indicators, "Indicators", icons::INDICATORS),
        (RightTab::Saved, "Saved", icons::SAVED),
        (RightTab::Research, "Research", icons::RESEARCH),
        (RightTab::Chat, "AI Chat", icons::CHAT),
    ];
    assert_eq!(pinned.len(), RightTab::ALL.len(), "a tab is missing from the pin");
    for (tab, label, glyph) in pinned {
        assert_eq!(tab.label(), label, "{tab:?}: the label");
        assert_eq!(tab.icon(), glyph, "{tab:?}: the glyph");
    }
}

/// The map is the Studio's own: every tab reads the row of its own name, no two share one, no row of the
/// `studio_tab` map is left without a tab, and every row carries the label and the glyph `label` and
/// `icon` read from it.
#[test]
fn every_tab_has_its_own_studio_tab_row_and_every_row_its_tab() {
    for tab in RightTab::ALL {
        assert_eq!(tab.row().key, format!("{tab:?}").to_uppercase(), "{tab:?} reads another row");
        assert_eq!(
            RightTab::ALL.iter().filter(|o| o.row() == tab.row()).count(),
            1,
            "{tab:?}'s row is shared"
        );
    }
    for row in maps::studio_tab::ALL {
        assert!(RightTab::ALL.iter().any(|t| t.row() == *row), "{} is no tab", row.key);
        assert!(row.word.is_some_and(|w| !w.is_empty()), "{}: no label", row.key);
        assert!(row.icon().is_some(), "{}: no glyph of the registry", row.key);
    }
}

/// `from_qa_str` (the `VIKE_STUDIO_TAB` capture hook) round-trips every tab by its lowercase
/// name and rejects garbage rather than panicking.
#[test]
fn from_qa_str_parses_every_tab_and_rejects_garbage() {
    let names = ["sweep", "strategy", "data", "indicators", "saved", "research", "chat"];
    for (name, want) in names.iter().zip(RightTab::ALL) {
        assert_eq!(RightTab::from_qa_str(name), Some(want));
    }
    assert_eq!(RightTab::from_qa_str(""), None);
    assert_eq!(RightTab::from_qa_str("Sweep"), None, "exact lowercase only");
    assert_eq!(RightTab::from_qa_str("nonsense"), None);
}

/// The parse table, INCLUDING the backwards-compatibility row that matters most: `"1"` must
/// keep meaning one backtest, because every capture script and dev shell in the tree already
/// spells it that way and the read it replaced was `== Ok("1")`.
#[test]
fn the_autorun_knob_parses_its_two_spellings_and_ignores_everything_else() {
    assert_eq!(QaAutorun::from_qa_str(Some("1")), QaAutorun::Run);
    assert_eq!(QaAutorun::from_qa_str(Some("sweep")), QaAutorun::Sweep);
    for garbage in [None, Some(""), Some("0"), Some("true"), Some("Sweep"), Some("run")] {
        assert_eq!(
            QaAutorun::from_qa_str(garbage),
            QaAutorun::Off,
            "{garbage:?} must be inert, never a surprise run"
        );
    }
    assert_eq!(QaAutorun::default(), QaAutorun::Off);
}

#[test]
fn the_sweep_ladder_widens_a_seeded_default_into_three_candidates() {
    assert_eq!(qa_sweep_ladder("10"), "5, 10, 15");
    assert_eq!(qa_sweep_ladder(" 4 "), "2, 4, 6");
    // Rungs come out ASCENDING, so a negative default reads low-to-high like every other axis.
    assert_eq!(qa_sweep_ladder("-8"), "-12, -8, -4");
    // A whole default stays whole — see the rounding comment for why a fractional lookback is
    // a lie rather than a nicety. 5 would otherwise widen to `2.5, 5, 7.5`.
    assert_eq!(qa_sweep_ladder("5"), "3, 5, 8");
    assert_eq!(qa_sweep_ladder("20"), "10, 20, 30");
    // ...and a fractional default is left fractional, because nothing is being misreported.
    assert_eq!(qa_sweep_ladder("2.5"), "1.25, 2.5, 3.75");
}

/// The pass-through arms — see [`qa_sweep_ladder`]'s doc for why each would produce a
/// degenerate axis rather than a wider one.
#[test]
fn the_sweep_ladder_leaves_a_value_it_cannot_widen_alone() {
    for v in ["0", "BTCUSDT", "", "nan", "inf"] {
        assert_eq!(qa_sweep_ladder(v), v, "{v:?} must pass through unchanged");
    }
}

/// Every ladder must still parse back through the CSV rule `start_sweep` applies, or the
/// widening would hand the sweep an empty grid and it would return early having done nothing —
/// the exact empty-frame failure this hook exists to remove. Driven through the real parse
/// rather than a restatement of it.
#[test]
fn a_widened_row_still_parses_as_a_sweep_axis() {
    let axis: Vec<f64> =
        qa_sweep_ladder("10").split(',').filter_map(|s| s.trim().parse().ok()).collect();
    assert_eq!(axis, vec![5.0, 10.0, 15.0]);
    assert!(axis.len() > 1, "a one-candidate axis is the single-combination sweep again");
}

/// The autorun is spent on the first frame WHATEVER it was, INCLUDING an arm that could not
/// start anything — a flag left armed fires a surprise run minutes later, the moment
/// `picker.refresh` auto-selects row 0 as data appears (the review finding the original `bool`
/// hook records, and the reason the disarm is a `replace` ahead of the ladder rather than an
/// assignment inside each arm).
///
/// Driven over an EMPTY store, which is what makes it the could-not-act case: `new_with_qa`
/// populates the picker synchronously at construction, so a SEEDED store already has row 0
/// selected and both arms would dispatch. Same idiom as `an_empty_store_leaves_no_slice`.
#[test]
fn every_autorun_arm_disarms_itself_even_when_it_could_not_act() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(DataFusionHist::open(dir.path()).unwrap()); // no series -> no selection
    for arm in [QaAutorun::Run, QaAutorun::Sweep] {
        let mut st = state_new(&dir, store.clone());
        assert!(st.picker.selected().is_none(), "the empty-store premise broke");
        st.qa_autorun = arm;
        st.take_qa_autorun();
        assert_eq!(st.qa_autorun, QaAutorun::Off, "{arm:?} left itself armed");
        assert!(!st.running, "{arm:?} dispatched with nothing selected");
        assert!(st.sweep_rx.is_none(), "{arm:?} dispatched with nothing selected");
    }
}

/// ...and the twin: over a POPULATED store both arms DO act, so the test above is proving a
/// disarm rather than an inert code path.
#[test]
fn both_autorun_arms_dispatch_when_a_slice_is_selectable() {
    let (_dir, store) = seeded_store();
    let mut run = state_new(&_dir, store.clone());
    run.qa_autorun = QaAutorun::Run;
    run.take_qa_autorun();
    assert!(run.running, "the Run arm did not start a backtest");

    let mut sweep = state_new(&_dir, store);
    sweep.qa_autorun = QaAutorun::Sweep;
    sweep.take_qa_autorun();
    assert!(sweep.sweep_rx.is_some(), "the Sweep arm did not start a sweep");
}

/// The Studio's ONE annualization derivation, over the two states a fixture can reach.
///
/// The seeded store is 1m bars — the interval the defect was worst on (a bare `252.0`
/// understated its Sharpe by `sqrt(1440) ≈ 37.9x`) and the one the CI roundtrip fixture uses.
/// `new_with_qa` populates the picker synchronously, so row 0 is already selected here; the
/// empty store is the app's own starting state, where nothing is.
///
/// The THIRD state — a tick slice — takes the same arm as "nothing picked" by construction
/// (`SeriesRow::interval` is `None` for both), so it is covered by the match rather than by a
/// fixture: seeding a quote/trade series here would exercise the picker's tick listing, not
/// this function's branch.
#[test]
fn the_display_factor_follows_the_picked_slice_and_falls_back_honestly() {
    let (_dir, store) = seeded_store();
    let picked = state_new(&_dir, store);
    assert_eq!(
        picked.picker.selected_row().and_then(|r| r.interval.as_deref()),
        Some("1m"),
        "the seeded-store premise broke — without a 1m pick this proves nothing",
    );
    assert_eq!(picked.display_periods_per_year(), periods_per_year_for_interval("1m"));
    assert_ne!(
        picked.display_periods_per_year(),
        DEFAULT_PERIODS_PER_YEAR,
        "a 1m slice annualized on the daily anchor IS the bug this derivation removes",
    );

    let dir = tempfile::tempdir().unwrap();
    let empty = Arc::new(DataFusionHist::open(dir.path()).unwrap()); // no series -> no pick
    let unpicked = state_new(&dir, empty);
    assert!(unpicked.picker.selected_row().is_none(), "the empty-store premise broke");
    assert_eq!(
        unpicked.display_periods_per_year(),
        DEFAULT_PERIODS_PER_YEAR,
        "with nothing picked the display keeps the anchor it has always had",
    );
}

/// The sweep arm's own contract: it must SEED the grid (a fresh workspace has an empty one and
/// `start_sweep` returns early on that) and widen what it seeded.
#[test]
fn the_sweep_autorun_seeds_and_widens_the_grid_before_dispatching() {
    let (_dir, store) = seeded_store();
    let mut st = state_new(&_dir, store);
    st.picker.refresh(st.store.as_ref());
    st.picker.select(0);
    assert!(st.grid.is_empty(), "a fresh state must start with the empty grid");
    st.qa_autorun = QaAutorun::Sweep;
    st.take_qa_autorun();
    assert!(!st.grid.is_empty(), "the sweep autorun must seed the grid it is about to sweep");
    for (name, csv) in &st.grid {
        let n = csv.split(',').filter(|s| s.trim().parse::<f64>().is_ok()).count();
        assert!(n > 1, "{name}: a one-candidate axis is the single-combination sweep ({csv:?})");
    }
    assert!(st.sweep_rx.is_some(), "the sweep autorun must actually dispatch a sweep");
}

/// ⚠ THE TEST THAT MAKES THE SWEEP POSE REAL. Everything above could pass while the capture
/// still rendered an empty form, because the whole chain hangs off one property of the SOURCE:
/// `discover_params` reports what `param()` declared, and the shipped `DEFAULT_SCRIPT`
/// declares its lookbacks with `const`. Driven through the REAL discovery rather than by
/// eyeballing the string, and it asserts the contrast in BOTH directions so the day somebody
/// makes the default sweepable this test says the swap is no longer needed.
#[test]
fn the_sweep_capture_script_declares_params_and_the_shipped_default_does_not() {
    let found = vike_script::discover_params(crate::panes::editor::SWEEP_CAPTURE_SCRIPT)
        .expect("the capture script must COMPILE — a sweep over it runs the real engine");
    let names: Vec<&str> = found.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, vec!["fast", "slow"], "the swept axes drifted from the script");
    assert_eq!(found[0].1, 5.0, "the mid rung must reproduce DEFAULT_SCRIPT's own lookback");
    assert_eq!(found[1].1, 20.0);

    let default_params = vike_script::discover_params(&EditorPane::default().source)
        .expect("the shipped default must still compile");
    assert!(
        default_params.is_empty(),
        "DEFAULT_SCRIPT now declares params ({default_params:?}) — the capture script's whole \
             reason to exist was that it did not, so re-read editor.rs's SWEEP_CAPTURE_SCRIPT doc \
             and consider dropping the swap"
    );
}

#[test]
fn seed_grid_from_discovered_params() {
    let (_dir, store) = seeded_store();
    let mut st = state_new(&_dir, store);
    st.editor.source = "let fast = param(\"fast\", 5.0);\nfn on_bar() {}".to_string();
    st.seed_sweep_grid(); // discover_params -> grid rows
    assert_eq!(st.grid.len(), 1);
    assert_eq!(st.grid[0].0, "fast");
}
