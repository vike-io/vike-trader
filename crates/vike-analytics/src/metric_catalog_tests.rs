use super::*;

/// Every [`MetricUnit`] variant, so the renderings can be iterated. The `match` in
/// `every_unit_renders_one_documented_way_and_percent_is_the_one_that_scales` is EXHAUSTIVE,
/// so a new variant is a compile error there rather than a rendering nobody pinned — which is
/// the only enumeration Rust offers over an enum and the reason this array is allowed to exist.
const ALL_UNITS: [MetricUnit; 4] =
    [MetricUnit::Money, MetricUnit::Percent, MetricUnit::Ratio, MetricUnit::Count];

/// ⚠ **THE PERCENT SCALING, pinned.** `0.031` is the value [`MetricUnit`]'s own doc argues
/// about: a drawdown of three percent, which the Percent unit must publish as `3.1000%` and
/// which every renderer that forgot the `* 100.0` published as `0.0310%` — three basis points,
/// a hundred times too small, on the row an operator sizes risk from.
///
/// The other three units are pinned on the SAME input, so the test also says what percent is
/// the exception TO: nothing else scales.
#[test]
fn every_unit_renders_one_documented_way_and_percent_is_the_one_that_scales() {
    for unit in ALL_UNITS {
        // Exhaustive on purpose: a fifth variant fails to compile here instead of shipping
        // with an unpinned rendering.
        let want = match unit {
            MetricUnit::Money => "0.03",
            MetricUnit::Percent => "3.1000%",
            MetricUnit::Ratio => "0.0310",
            MetricUnit::Count => "0",
        };
        assert_eq!(unit.render(0.031), want, "{unit:?}");
    }

    // ...and the factor is exactly a HUNDRED, on two values where no other factor could
    // produce these strings.
    assert_eq!(MetricUnit::Percent.render(1.0), "100.0000%");
    assert_eq!(MetricUnit::Percent.render(0.0001), "0.0100%");
    // A negative value keeps its sign and gains no parentheses or colour — `total_return` on a
    // losing run and `gross_loss`/`funding_paid` always are negative, and a renderer must not
    // have to special-case them to avoid an accounting bracket nobody asked for.
    assert_eq!(MetricUnit::Percent.render(-0.05), "-5.0000%");
    assert_eq!(MetricUnit::Money.render(-12.5), "-12.50");
}

/// ⚠ **The house sentinel renders as `inf`, and that is a DECISION rather than an oversight.**
/// [`crate::report::BacktestReport::profit_factor`] carries `f64::INFINITY` for "nothing lost",
/// and every door prints Rust's own `inf` for it today. Substituting a placeholder inside
/// [`MetricUnit::render`] would move a published row in all four doors at once, so the choice
/// belongs to whoever owns the metric, not to its unit — this test is what stops the
/// substitution happening by accident inside a formatting change.
#[test]
fn a_nonfinite_value_renders_as_rusts_own_token_and_gains_no_placeholder() {
    assert_eq!(MetricUnit::Ratio.render(f64::INFINITY), "inf");
    assert_eq!(MetricUnit::Ratio.render(f64::NEG_INFINITY), "-inf");
    assert_eq!(MetricUnit::Ratio.render(f64::NAN), "NaN");
}

/// Every catalog row renders through its own unit without panicking and without an empty cell —
/// the cheap end-to-end over [`METRICS`], so a row whose unit was chosen carelessly (a `Count`
/// on a fraction, say) is at least exercised.
#[test]
fn every_catalog_row_renders_through_its_unit() {
    for m in METRICS {
        let cell = m.unit.render(1.5);
        assert!(!cell.is_empty(), "{} rendered nothing", m.id);
        assert_eq!(
            cell.ends_with('%'),
            m.unit == MetricUnit::Percent,
            "{} carries the `%` suffix if and only if it is a Percent",
            m.id
        );
    }
}

/// An id is a KEY as well as a name, so a duplicate would make one of the two unreachable
/// through [`spec_for`] and silently shadow the other in any map built from this table.
#[test]
fn every_id_is_unique() {
    let mut seen: Vec<&str> = Vec::new();
    for m in METRICS {
        assert!(!seen.contains(&m.id), "{} is declared twice", m.id);
        seen.push(m.id);
    }
}

/// The two tables must not overlap: a name cannot be both answerable and declared absent, and
/// if it were, [`parse_metric_selection`]'s absent-check-first order would make the catalog row
/// dead.
#[test]
fn nothing_is_both_computed_and_declared_absent() {
    for (name, _) in ABSENT {
        assert!(
            spec_for(name).is_none(),
            "{name} is in both METRICS and ABSENT — the absent row would shadow it"
        );
    }
}

/// `Compact` must be exactly the report's own scalars, which is what makes the keyword
/// byte-identical to the pre-widening bare `--metrics`.
#[test]
fn compact_is_the_reports_own_scalars() {
    let ids = MetricSelection::Compact.ids();
    assert_eq!(
        ids,
        vec![
            "final_equity",
            "total_return",
            "n_trades",
            "win_rate",
            "sharpe",
            "max_drawdown",
            "profit_factor",
            "funding_paid",
        ]
    );
    assert!(!MetricSelection::Compact.needs_extended());
}

/// A named selection renders in CATALOG order, not in typing order — otherwise two operators
/// asking for the same set get tables that diff against each other.
#[test]
fn a_named_selection_renders_in_catalog_order_not_typing_order() {
    let sel = parse_metric_selection("sortino,sharpe").unwrap();
    assert_eq!(sel.ids(), vec!["sharpe", "sortino"]);
    assert!(sel.needs_extended(), "sortino lives in the extended block");
}

/// The keyword set is case-insensitive, because a shell history carries whatever was typed.
#[test]
fn keywords_are_case_insensitive() {
    assert_eq!(parse_metric_selection("FULL").unwrap(), MetricSelection::Full);
    assert_eq!(parse_metric_selection("Compact").unwrap(), MetricSelection::Compact);
    assert_eq!(parse_metric_selection("Honesty").unwrap(), MetricSelection::Honesty);
    assert_eq!(parse_metric_selection("realism").unwrap(), MetricSelection::Realism);
}

/// Whitespace around a comma survives, so a quoted list is not a refusal about quoting.
#[test]
fn a_quoted_list_with_spaces_parses() {
    assert_eq!(
        parse_metric_selection(" sharpe , sortino ").unwrap(),
        MetricSelection::Named(vec!["sharpe", "sortino"])
    );
}

/// Each refusal fires for its OWN reason and says which — the property that makes them worth
/// publishing rather than collapsing into one message.
#[test]
fn each_refusal_names_what_it_refused() {
    let e = parse_metric_selection("").unwrap_err();
    assert!(e.contains("needs a selection"), "{e}");

    let e = parse_metric_selection(",,").unwrap_err();
    assert!(e.contains("names nothing"), "{e}");

    let e = parse_metric_selection("full,sharpe").unwrap_err();
    assert!(e.contains("never both"), "{e}");

    let e = parse_metric_selection("sharpe,sharpe").unwrap_err();
    assert!(e.contains("twice"), "{e}");

    let e = parse_metric_selection("shrapnel").unwrap_err();
    assert!(e.contains("does not know") && e.contains("shrapnel"), "{e}");

    // The whole point of ABSENT: the reason, not "unknown metric".
    let e = parse_metric_selection("exposure").unwrap_err();
    assert!(e.contains("POSITION SIZES") && e.contains("declared absent"), "{e}");
}

/// ⚠ **The migration typo, answered by name.** `--metrics` shipped as a bare switch, so
/// `show --metrics @last` used to work and now feeds the run selector in as the selection. The
/// refusal must say THAT, not "unknown metric @last", which sends an operator looking for a
/// metric called `@last`.
#[test]
fn a_run_selector_in_the_value_slot_is_named_as_such() {
    for spec in ["@last", "@baseline/main", "1756000000-1-0/x"] {
        let e = parse_metric_selection(spec).unwrap_err();
        assert!(e.contains("looks like a run selector"), "{spec}: {e}");
        assert!(
            e.contains("--metrics compact"),
            "the refusal must name the spelling that restores the old output: {e}"
        );
    }
}

/// ...and a legitimate id is not caught by that shape check. The guard keys on `@` and `/`,
/// which no catalog id contains — this is the test that says so rather than assuming it.
#[test]
fn no_catalog_id_is_mistaken_for_a_run_selector() {
    for m in METRICS {
        assert!(
            !m.id.starts_with('@') && !m.id.contains('/'),
            "{} would be refused as a run selector",
            m.id
        );
        assert!(parse_metric_selection(m.id).is_ok(), "{} does not parse as itself", m.id);
    }
}

/// The listing names every id and every absent row — it is the operator's only door onto the
/// catalog, so a metric missing from it is a metric nobody can find.
#[test]
fn the_listing_names_every_id_and_every_absent_row() {
    let text = metric_list_text();
    for m in METRICS {
        assert!(text.contains(m.id), "{} is missing from --metrics-list", m.id);
    }
    for (name, _) in ABSENT {
        assert!(text.contains(name), "absent row {name} is missing from --metrics-list");
    }
}
