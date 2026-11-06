use super::*;

fn gb(i: usize) -> Bar {
    let base = 100.0 + (i as f64 * 0.31).sin() * 3.0;
    Bar {
        t: i as f64,
        ot: 1_700_000_000_000 + i as i64 * 60_000,
        o: base,
        h: base + 1.0,
        l: base - 1.0,
        c: base + 0.3,
        v: 500.0 + i as f64,
    }
}

/// ⚠ **Building an overlay must not be QUADRATIC in history** — a hang is a correctness problem
/// from the user's side, not a performance nicety.
///
/// `recompute_full` used to fold bar-by-bar through `push_bar`, and `on_bar` for a
/// `hist_indicator!` indicator re-runs the batch kernel over all retained history. That makes
/// construction O(bars x retained x period). At 200,000 bars with `high_low_52w` at period 1000
/// it measured at OVER AN HOUR before being killed — the GUI simply freezes when a user adds
/// that indicator to a long chart.
///
/// This bounds it by WALL CLOCK deliberately, rather than by asserting an implementation shape.
/// The budget is deliberately loose (seconds, not milliseconds) so it cannot flake on a loaded
/// CI box while still being thousands of times under the pre-fix behaviour — the failure this
/// guards is minutes-to-hours, not tens of milliseconds.
///
/// NON-VACUOUS: `high_low_52w` at its MAXIMUM period is the deepest kernel in the registry, and
/// the bar count is large enough that the old path could not finish. Revert `recompute_full`'s
/// batch arm and this test does not fail — it hangs, which is the point.
#[test]
fn building_a_deep_overlay_over_a_long_history_is_not_quadratic() {
    let spec = get("high_low_52w").expect("high_low_52w is registered");
    let bars: Vec<Bar> = (0..30_000).map(gb).collect();

    let t0 = std::time::Instant::now();
    let mut a = Active::new(1, spec, &[]);
    a.set_params(vec![1000.0], &bars);
    let elapsed = t0.elapsed();

    assert_eq!(a.outputs[0].series.len(), bars.len(), "every bar gets a value");
    assert!(
        elapsed.as_secs() < 20,
        "building a period-1000 overlay over {} bars took {elapsed:?} — the bar-by-bar fold is \
             back, and it is O(bars x retained x period). A user adding this indicator to a long \
             chart sees a frozen window.",
        bars.len()
    );
}

/// The batch arm must produce the SAME series the fold does — it is a speed change only.
///
/// Compares `recompute_full`'s output against `Indicator::vectorize` directly, bit-for-bit, for
/// both a trimming indicator (takes the new arm) and a hand-written incremental one (takes the
/// old arm, and must keep taking it — seeding a path-dependent impl from a suffix would be a
/// wrong number).
#[test]
fn the_batch_build_matches_the_fold_bit_for_bit() {
    let bars: Vec<Bar> = (0..2_000).map(gb).collect();
    let model: Vec<vike_marketdata::Bar> = bars.iter().map(to_model_bar).collect();

    for name in ["midpoint", "doji", "linearreg", "sma", "obv", "vwap"] {
        let spec = get(name).unwrap_or_else(|| panic!("{name} is registered"));
        let a = Active::new(1, spec, &bars);
        let expected = spec.build().vectorize(&model);
        for (k, slot) in a.outputs.iter().enumerate() {
            assert_eq!(slot.series.len(), bars.len(), "{name} line {k} length");
            for (i, v) in slot.series.iter().enumerate() {
                assert_eq!(
                    v.to_bits(),
                    expected[k][i].to_bits(),
                    "{name} line {k} bar {i}: built {v}, vectorize {}",
                    expected[k][i]
                );
            }
        }
    }
}
