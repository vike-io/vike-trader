use super::*;

/// The list is carried, not summarized: every term `0063` ruled on and this run does not
/// price, each naming itself — and no term it DOES price: a slice is charged its stored funding.
#[test]
fn the_not_modelled_list_names_every_unpriced_term() {
    let joined = NOT_MODELLED.join(" | ");
    for term in ["impact", "risk", "resolution", "snap_to_properties"] {
        assert!(joined.contains(term), "the not-modelled list names {term}: {joined}");
    }
    assert!(!joined.contains("funding"), "funding is charged, so it is not listed: {joined}");
}

/// **The two CLASSES stay distinguishable**, because a reader does different things about
/// them: a declared residual is not fixable by widening the wire, while a DTO omission is a
/// boolean nobody has built yet. A list that flattened the two would tell a reader that the
/// instrument grid is as underivable as an impact calibration, which is false — `0063` ruled the
/// information server-side.
///
/// This is the test that goes red when the boolean SHIPS: its row must leave [`NOT_MODELLED`] at
/// the same time, or the stamp starts under-reporting what it priced. `attach_funding`, the other
/// of `0063`'s two, left on 2026-11-08, when every slice started paying its stored funding.
#[test]
fn a_dto_omission_is_labelled_as_one_and_a_residual_is_not() {
    let omissions: Vec<&str> =
        NOT_MODELLED.iter().copied().filter(|e| e.contains("DTO omission")).collect();
    assert_eq!(omissions.len(), 1, "the one boolean 0063 called expressible that is still unbuilt");
    for e in &omissions {
        assert!(
            e.starts_with("snap_to_properties"),
            "an omission row names its boolean first: {e}"
        );
    }
    for e in NOT_MODELLED.iter().filter(|e| !e.contains("DTO omission")) {
        assert!(
            e.starts_with("impact") || e.starts_with("risk") || e.starts_with("resolution"),
            "a residual row is one of 0063's three, named first: {e}"
        );
    }
}

/// The mix SUMS across the runs it is fed, because a walk-forward's reported numbers come from
/// several out-of-sample windows.
#[test]
fn the_mix_accumulates_across_runs() {
    let mix_of = |maker_fills, taker_fills| {
        vike_analytics::TradingRecordSource::Backtest(vike_analytics::TradingRecordSimulator {
            maker_fills,
            taker_fills,
            ..Default::default()
        })
    };
    let a = TradingRecord { source: mix_of(2, 3), fees_paid: 1.5, ..Default::default() };
    let b = TradingRecord { source: mix_of(1, 0), fees_paid: 0.25, ..Default::default() };
    let mut mix = FillMix::of(&a);
    mix.add(&b);
    assert_eq!(mix.maker_fills, 3);
    assert_eq!(mix.taker_fills, 3);
    assert_eq!(mix.fees_paid, 1.75);
}
