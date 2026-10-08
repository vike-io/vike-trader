use super::*;

fn spec(raw: &str) -> Spec {
    parse_spec(raw).unwrap_or_else(|e| panic!("{raw}: {e}"))
}

fn gate_args(
    spec: Spec,
    require_days: i64,
    max_gap_ms: Option<i64>,
    kinds: &[&str],
) -> super::super::GateArgs {
    super::super::GateArgs {
        spec,
        require_days,
        max_gap_ms,
        kinds: kinds.iter().map(|k| (*k).to_string()).collect(),
    }
}

fn bars(days: i64) -> Candidate {
    Candidate {
        kind: "bar".to_string(),
        interval: Some("1h".to_string()),
        first_ts: 0,
        last_ts: days * MS_PER_DAY,
        rows: 24 * days as u64,
        gaps: None,
        gaps_error: None,
    }
}

/// §7.1's two spellings both parse, and the parts land where the selector reads them. The
/// interval is OPTIONAL here where `crate::cmd::data`'s `check_spec` requires one — the whole
/// reason this verb has a parser of its own.
#[test]
fn the_spec_grammar_carries_an_optional_interval_and_a_group_alternative() {
    assert_eq!(
        spec("binance:BTCUSDT:1h"),
        Spec {
            venue: "binance".into(),
            name: "BTCUSDT".into(),
            grouped: false,
            interval: Some("1h".into()),
        }
    );
    assert_eq!(
        spec("binance:BTCUSDT"),
        Spec { venue: "binance".into(), name: "BTCUSDT".into(), grouped: false, interval: None }
    );
    assert_eq!(
        spec("polymarket:@election-2026"),
        Spec {
            venue: "polymarket".into(),
            name: "election-2026".into(),
            grouped: true,
            interval: None,
        }
    );
    // ...and the round trip, so the header line names what was actually selected.
    for raw in ["binance:BTCUSDT:1h", "binance:BTCUSDT", "polymarket:@election-2026"] {
        assert_eq!(spec(raw).text(), raw);
    }
}

/// The shapes that are refused, each for its own stated reason. The ANTI-VACUITY control is the
/// test above: every refusal here has a sibling spelling that parses, so a parser that refused
/// everything would redden that one.
#[test]
fn a_malformed_spec_is_refused_with_the_shape_it_should_have_had() {
    for (raw, needle) in [
        ("BTCUSDT", "not a series spec"),
        ("binance:BTCUSDT:1h:extra", "not a series spec"),
        ("binance:", "not a series spec"),
        (":BTCUSDT", "not a series spec"),
        ("binance:@", "EMPTY group"),
        ("polymarket:@election-2026:1h", "GROUPED"),
    ] {
        let e = parse_spec(raw).expect_err(raw);
        assert!(e.contains(needle), "{raw}: {e}");
    }
}

/// **The MIRROR of that grouped refusal, and the half that was missing.** A spec naming a bar
/// step and a `--require-kind` naming a kind that has none can never both be satisfied, so the
/// line is refused before a socket opens — rather than costing an `inventory()` round trip and
/// then BREACHING over a tape that is already on disk.
///
/// The ANTI-VACUITY control is the second half: three satisfiable shapes, each one character
/// from a refused one, so a predicate that refused everything reddens here.
#[test]
fn a_required_kind_the_spec_could_never_select_is_refused_before_a_socket_opens() {
    let kinds = |k: &[&str]| -> Vec<String> { k.iter().map(|s| (*s).to_string()).collect() };
    let refuse = |raw: &str, k: &[&str]| -> String {
        refuse_a_kind_the_spec_can_never_select(&spec(raw), &kinds(k))
            .expect_err(&format!("{raw} {k:?} can only ever select nothing"))
    };
    let e = refuse("binance:BTCUSDT:1h", &["bar", "trade"]);
    assert!(e.contains("`1h`"), "it quotes the step that did it: {e}");
    assert!(e.contains("--require-kind trade"), "…and the criterion it contradicts: {e}");
    assert!(e.contains("`binance:BTCUSDT` gates"), "…and the spec that WOULD work: {e}");
    assert!(e.contains("BREACH"), "…and what would otherwise have happened: {e}");
    // Every non-bar kind, not a roster of tick kinds: this side validates no kind against any
    // table, so `properties` and a kind nobody has ever declared are refused identically.
    for kind in ["quote", "trade", "book", "depth", "properties", "a_kind_nobody_declared"] {
        assert!(
            refuse("binance:BTCUSDT:1h", &[kind]).contains(kind),
            "{kind} carries no bar step either"
        );
    }

    for (raw, k) in [
        ("binance:BTCUSDT:1h", &["bar"][..]),
        ("binance:BTCUSDT", &["bar", "trade"][..]),
        ("polymarket:@election-2026", &["book"][..]),
    ] {
        assert!(
            refuse_a_kind_the_spec_can_never_select(&spec(raw), &kinds(k)).is_ok(),
            "{raw} {k:?} is satisfiable and must not be refused"
        );
    }
}

/// **The pin behind [`INTERVAL_BEARING_KIND`]** — the store's own answer, not this file's.
/// `vike-data` is a DEV-dependency here, so no production path can read the layout authority;
/// the TEST build can, which is what turns a local constant into a pin. Its twin one crate
/// down is `crates/vike-data/src/store/store_kind.rs`'s own
/// `grouping_requires_a_row_level_symbol_column`.
#[test]
fn the_interval_bearing_kind_is_the_one_the_store_itself_declares() {
    let bearing: Vec<&str> = vike_data::store::store_kind::STORE_KINDS
        .iter()
        .filter(|k| k.partition == vike_data::store::store_kind::Partition::SymbolInterval)
        .map(|k| k.kind)
        .collect();
    assert_eq!(
        bearing,
        vec![INTERVAL_BEARING_KIND],
        "the refusal above is only correct while EXACTLY ONE stored kind sub-partitions by \
             interval — a second one means a spec's third part can select more than bars, and the \
             refusal has to name the set instead of the value"
    );
}

/// The selector is EXACT on every dimension the spec named and silent about the one it did not.
/// The `@` is load-bearing: a group and a symbol spelling the same word are different series,
/// and a selector that ignored it would gate the wrong one.
#[test]
fn the_selector_is_exact_and_an_absent_interval_matches_every_step() {
    let s = spec("binance:BTCUSDT:1h");
    assert!(s.matches("binance", "BTCUSDT", false, Some("1h")));
    assert!(!s.matches("binance", "BTCUSDT", false, Some("4h")), "the step was named");
    assert!(!s.matches("bybit", "BTCUSDT", false, Some("1h")), "the venue was named");
    assert!(!s.matches("binance", "BTCUSD", false, Some("1h")), "no substring match");
    assert!(!s.matches("BINANCE", "BTCUSDT", false, Some("1h")), "and no case folding");

    let loose = spec("binance:BTCUSDT");
    assert!(loose.matches("binance", "BTCUSDT", false, Some("1h")));
    assert!(loose.matches("binance", "BTCUSDT", false, None), "a tick series has no step");

    let group = spec("polymarket:@election-2026");
    assert!(group.matches("polymarket", "election-2026", true, None));
    assert!(
        !group.matches("polymarket", "election-2026", false, None),
        "a GROUP and a SYMBOL of the same spelling are different series"
    );
    assert!(!loose.matches("binance", "BTCUSDT", true, None), "…and the reverse");
}

/// `--require-days` takes a whole positive count, and ZERO is refused rather than accepted as a
/// gate that passes over an empty store.
#[test]
fn require_days_refuses_the_count_that_would_assert_nothing() {
    assert_eq!(parse_require_days("365"), Ok(365));
    assert_eq!(parse_require_days(" 7 "), Ok(7));
    let z = parse_require_days("0").expect_err("zero");
    assert!(z.contains("asserts nothing"), "{z}");
    let n = parse_require_days("-3").expect_err("negative");
    assert!(n.contains("asserts nothing"), "{n}");
    let x = parse_require_days("a week").expect_err("prose");
    assert!(x.contains("whole number of days"), "{x}");
}

/// ⚠ **A count that does not FIT is the zero refusal wearing arithmetic, and it is reached by
/// a route that one does not cover.** The threshold is `n * MS_PER_DAY`, and a RELEASE build
/// wraps rather than panicking: 2e11 days is 1.728e19ms, which wraps to about -1.12e18, and
/// every span is then `>=` it — a gate exiting 0 having asserted nothing. The control is the
/// boundary itself, which must still parse.
#[test]
fn require_days_refuses_a_count_whose_threshold_cannot_be_computed() {
    let biggest = i64::MAX / MS_PER_DAY;
    assert_eq!(parse_require_days(&biggest.to_string()), Ok(biggest), "the boundary parses");
    let e = parse_require_days(&(biggest + 1).to_string()).expect_err("one day past it");
    assert!(e.contains("does not fit"), "{e}");
    assert!(e.contains(&biggest.to_string()), "…and names the largest that does: {e}");
    let n = parse_require_days("200000000000").expect_err("a nanosecond count, fat-fingered");
    assert!(n.contains("does not fit"), "{n}");

    // ...and the judgement itself fails CLOSED for a count the parser never let through: a
    // saturating multiply breaches, where the wrapping one this replaced PASSED over two days.
    let g = gate_args(spec("binance:BTCUSDT:1h"), 200_000_000_000, None, &["bar"]);
    assert_eq!(rung(&judge(&g, &[bars(2)])), Exit::Breach);
}

/// `--max-gap` reaches `vike_model::time::parse_span` — this workspace's duration grammar, not a
/// second one — and narrows it to the subset a wall-clock tolerance can be. The two rejected
/// variants each say why in terms an operator can act on.
#[test]
fn max_gap_reuses_the_workspace_span_grammar_and_narrows_it_to_fixed_time() {
    assert_eq!(parse_max_gap("4h"), Ok(4 * 3_600_000));
    assert_eq!(parse_max_gap("1d"), Ok(MS_PER_DAY));
    assert_eq!(parse_max_gap("2w"), Ok(14 * MS_PER_DAY));
    let bars = parse_max_gap("500bars").expect_err("a bar count");
    assert!(bars.contains("BAR COUNT") && bars.contains("interval"), "{bars}");
    let months = parse_max_gap("3mo").expect_err("a calendar span");
    assert!(months.contains("CALENDAR") && months.contains("31 January"), "{months}");
    // ...and the grammar's own refusals arrive verbatim, prefixed with the flag that carried
    // them, rather than being re-derived here.
    let bare = parse_max_gap("90").expect_err("a bare number");
    assert!(bare.contains("--max-gap") && bare.contains("no unit"), "{bare}");
}

/// A hole's length is `to - from + 1`, because `series_gaps` returns INCLUSIVE bounds — so one
/// missing UTC day measures exactly 24h. The control is the second row: two missing days
/// measure 48h, which an implementation that clamped to a day would get wrong.
#[test]
fn a_holes_length_is_inclusive_so_one_missing_day_is_exactly_a_day() {
    let one = Candidate { gaps: Some(vec![(3 * MS_PER_DAY, 4 * MS_PER_DAY - 1)]), ..bars(10) };
    assert_eq!(one.largest_gap_ms(), Some(MS_PER_DAY));
    let two = Candidate { gaps: Some(vec![(3 * MS_PER_DAY, 5 * MS_PER_DAY - 1)]), ..bars(10) };
    assert_eq!(two.largest_gap_ms(), Some(2 * MS_PER_DAY));
    // The LARGEST, not the first or the sum.
    let many = Candidate {
        gaps: Some(vec![
            (3 * MS_PER_DAY, 4 * MS_PER_DAY - 1),
            (8 * MS_PER_DAY, 11 * MS_PER_DAY - 1),
            (20 * MS_PER_DAY, 21 * MS_PER_DAY - 1),
        ]),
        ..bars(30)
    };
    assert_eq!(many.largest_gap_ms(), Some(3 * MS_PER_DAY));
    // An EMPTY probe is a real answer — no holes — and is NOT the same as an unmade one.
    let clean = Candidate { gaps: Some(Vec::new()), ..bars(10) };
    assert_eq!(clean.largest_gap_ms(), Some(0));
    assert_eq!(bars(10).largest_gap_ms(), None, "no probe was made");
}

/// The days criterion judges the recorded SPAN, and the spec's own example is the pin: 365 days
/// required, 365 days present is a pass and one day short is a breach.
#[test]
fn the_days_criterion_is_the_span_and_its_boundary_is_inclusive() {
    let g = gate_args(spec("binance:BTCUSDT:1h"), 365, None, &["bar"]);
    let pass = judge(&g, &[bars(365)]);
    assert_eq!(pass[1].criterion, REQUIRE_DAYS);
    assert_eq!(pass[1].outcome, Outcome::Pass, "{:?}", pass[1]);
    let breach = judge(&g, &[bars(364)]);
    assert_eq!(breach[1].outcome, Outcome::Breach, "{:?}", breach[1]);
    assert!(breach[1].observed.starts_with("364d"), "{:?}", breach[1]);
    assert_eq!(rung(&breach), Exit::Breach);
    assert_eq!(rung(&pass), Exit::Ok);
}

/// **The failure §8.4 names, and the reason `--max-gap` is not decoration.** A store whose span
/// is a full year and whose middle three weeks are missing PASSES the days criterion — and
/// breaches the gap one. Both halves are asserted, because the first alone reads like a bug and
/// the second alone would not prove the first is the behaviour on purpose.
#[test]
fn a_year_wide_span_with_three_weeks_missing_passes_days_and_breaches_the_gap() {
    let holed =
        Candidate { gaps: Some(vec![(100 * MS_PER_DAY, 121 * MS_PER_DAY - 1)]), ..bars(365) };
    let days_only = gate_args(spec("binance:BTCUSDT:1h"), 365, None, &["bar"]);
    assert_eq!(rung(&judge(&days_only, std::slice::from_ref(&holed))), Exit::Ok);

    let both = gate_args(spec("binance:BTCUSDT:1h"), 365, Some(4 * 3_600_000), &["bar"]);
    let js = judge(&both, &[holed]);
    assert_eq!(js[1].outcome, Outcome::Pass, "the span is still a year: {:?}", js[1]);
    assert_eq!(js[2].criterion, MAX_GAP);
    assert_eq!(js[2].outcome, Outcome::Breach, "{:?}", js[2]);
    assert_eq!(rung(&js), Exit::Breach);
}

/// A DECLARED kind the store does not hold is a BREACH, not "nothing was evaluated" — the
/// operator said it was required. The observed cell names the kinds the spec DOES hold, because
/// a bare `0` cannot be acted on without a second command.
#[test]
fn a_required_kind_the_store_lacks_breaches_and_names_what_it_does_hold() {
    let g = gate_args(spec("binance:BTCUSDT"), 1, None, &["trade"]);
    let js = judge(&g, &[bars(30)]);
    assert_eq!(js.len(), 1, "no series of the declared kind, so no per-series criteria: {js:?}");
    assert_eq!(js[0].criterion, KIND_PRESENT);
    assert_eq!(js[0].outcome, Outcome::Breach);
    assert!(js[0].observed.contains("bar"), "it names what IS there: {:?}", js[0]);
    assert_eq!(rung(&js), Exit::Breach);
}

/// A kind the gate is NOT about is never judged for days or gaps, which is what keeps a
/// one-row `properties` grid from reddening every instrument that has one. The control is the
/// second half: declaring that kind DOES judge it.
#[test]
fn an_undeclared_kind_is_evidence_for_presence_and_is_never_judged_itself() {
    let props = Candidate {
        kind: "properties".to_string(),
        interval: None,
        first_ts: MS_PER_DAY,
        last_ts: MS_PER_DAY,
        rows: 1,
        gaps: None,
        gaps_error: None,
    };
    let g = gate_args(spec("binance:BTCUSDT"), 30, None, &["bar"]);
    let js = judge(&g, &[bars(90), props.clone()]);
    assert_eq!(js.len(), 2, "one presence criterion and one bar series: {js:?}");
    assert_eq!(rung(&js), Exit::Ok);

    let declared = gate_args(spec("binance:BTCUSDT"), 30, None, &["bar", "properties"]);
    let js = judge(&declared, &[bars(90), props]);
    assert_eq!(js.len(), 4, "two presence criteria and two series: {js:?}");
    assert_eq!(rung(&js), Exit::Breach, "a one-row grid does not span 30 days");
}

/// A failed gap probe is UNEVALUATED and lands on the nothing-was-evaluated rung — never a
/// pass. That is the deliberate divergence from `execute_list`'s degrade-the-row rule, and the
/// row carries the reason so a person is not left believing the gate checked something.
#[test]
fn a_failed_gap_probe_is_unevaluated_rather_than_a_pass() {
    let broken =
        Candidate { gaps: None, gaps_error: Some("cannot read manifest".to_string()), ..bars(400) };
    let g = gate_args(spec("binance:BTCUSDT:1h"), 365, Some(MS_PER_DAY), &["bar"]);
    let js = judge(&g, &[broken]);
    assert_eq!(js[1].outcome, Outcome::Pass, "the days half still answered: {:?}", js[1]);
    assert!(matches!(js[2].outcome, Outcome::Unevaluated(_)), "{:?}", js[2]);
    assert_eq!(rung(&js), Exit::Empty);
}

/// ⚠ A BREACH outranks an unevaluated criterion, in EITHER order — a real failure must never be
/// masked by a probe that could not answer elsewhere in the same run. Asserted both ways round,
/// because a `.max()` replaced by "the first non-pass wins" would pass one of them.
#[test]
fn a_breach_outranks_an_unevaluated_criterion_in_either_order() {
    // Probed and clean: its BREACH comes from the days criterion alone, so the ranking below
    // is genuinely breach-against-unevaluated rather than two unevaluated rows.
    let short = Candidate { gaps: Some(Vec::new()), ..bars(1) };
    let broken =
        Candidate { gaps: None, gaps_error: Some("cannot read manifest".to_string()), ..bars(400) };
    let g = gate_args(spec("binance:BTCUSDT"), 365, Some(MS_PER_DAY), &["bar"]);
    assert_eq!(rung(&judge(&g, &[short.clone(), broken.clone()])), Exit::Breach);
    assert_eq!(rung(&judge(&g, &[broken, short])), Exit::Breach);
}

/// An INVERTED span is not a breach and not a pass: the store's own numbers are impossible, so
/// nothing honest can be derived from them. The row names the sibling verb that reports it.
#[test]
fn an_impossible_span_goes_unevaluated_and_names_the_verb_that_reports_it() {
    let inverted = Candidate { first_ts: 10 * MS_PER_DAY, last_ts: 0, rows: 5, ..bars(1) };
    let g = gate_args(spec("binance:BTCUSDT:1h"), 1, None, &["bar"]);
    let js = judge(&g, &[inverted]);
    assert!(matches!(js[1].outcome, Outcome::Unevaluated(_)), "{:?}", js[1]);
    let Outcome::Unevaluated(why) = &js[1].outcome else { panic!("{:?}", js[1]) };
    assert!(why.contains("data hist health"), "{why}");
    assert_eq!(rung(&js), Exit::Empty);
}

/// A series the store folded to EMPTY spans nothing and BREACHES — it is absence, not an
/// impossibility, and a gate that passed over it would be green on a store holding no rows.
/// Its dates render as `-` rather than as `1970-01-01`, which would read as data.
#[test]
fn an_empty_series_breaches_and_renders_no_dates() {
    let empty = Candidate { first_ts: 0, last_ts: 0, rows: 0, ..bars(1) };
    let g = gate_args(spec("binance:BTCUSDT:1h"), 1, None, &["bar"]);
    let js = judge(&g, &[empty]);
    assert_eq!(js[1].outcome, Outcome::Breach, "{:?}", js[1]);
    assert!(js[1].observed.contains("0d (- .. -)"), "{:?}", js[1]);
}

/// An empty judgement set is not a pass. Unreachable through the verb — the parser defaults the
/// kind roster to one entry, so a presence criterion always exists — and pinned anyway, because
/// the one thing this rung may never do is answer `0` for "nothing happened".
#[test]
fn no_criteria_at_all_is_not_a_pass() {
    assert_eq!(rung(&[]), Exit::Empty);
}

/// The word and the number are ONE decision, and it is the COMPUTE plane's — this verb reaches
/// `crate::cmd::runs::failif`'s vocabulary through its own judgements rather than through a
/// copy of the mapping. A renderer that said `pass` while the process exited on a breach is the
/// drift `runs/gate.rs`'s split exists to prevent; two planes printing different words for one
/// outcome is the drift the shared declaration prevents.
///
/// The three rows are reached through real `judge` output rather than by naming `Exit`
/// variants, so a projection that read the wrong field would redden this even while
/// `failif`'s own pin stayed green.
#[test]
fn the_verdict_word_and_the_rung_are_the_same_decision() {
    let g = gate_args(spec("binance:BTCUSDT:1h"), 365, Some(MS_PER_DAY), &["bar"]);
    let pass = judge(&g, &[Candidate { gaps: Some(Vec::new()), ..bars(400) }]);
    assert_eq!(verdict_word(rung(&pass)), "pass");

    let breach = judge(&g, &[Candidate { gaps: Some(Vec::new()), ..bars(10) }]);
    assert_eq!(verdict_word(rung(&breach)), "breach");

    let unevaluated = judge(
        &g,
        &[Candidate {
            gaps: None,
            gaps_error: Some("cannot read manifest".to_string()),
            ..bars(400)
        }],
    );
    assert_eq!(verdict_word(rung(&unevaluated)), "unevaluated");
}

/// **The disclosure that keeps a half-checked gate from reading as a whole one.** A run with no
/// `--max-gap` says which half it did not check; a run WITH one does not carry that note —
/// the anti-vacuity control, since a note that fires on every run stops being read.
#[test]
fn a_gate_that_checked_no_holes_says_so_and_one_that_did_stays_quiet() {
    // ⚠ Each fixture carries the gap evidence its OWN run would have: unprobed where no
    // `--max-gap` was given, probed-and-clean where one was. `execute_gate` produces exactly
    // that pairing, and rendering the other combination would assert a table no run reaches.
    let none = gate_args(spec("binance:BTCUSDT:1h"), 365, None, &["bar"]);
    let js = judge(&none, &[bars(400)]);
    let text = lines(&none, &js, rung(&js), 1, 4).join("\n");
    assert!(text.contains("HOLES"), "{text}");
    assert!(text.contains("--max-gap"), "…and how to check them: {text}");

    let with = gate_args(spec("binance:BTCUSDT:1h"), 365, Some(7 * MS_PER_DAY), &["bar"]);
    let js = judge(&with, &[Candidate { gaps: Some(Vec::new()), ..bars(400) }]);
    let text = lines(&with, &js, rung(&js), 1, 4).join("\n");
    assert!(!text.contains("HOLES"), "the note must not fire when the holes WERE checked: {text}");
}

/// **The second disclosure, and it is a MEASUREMENT of the store rather than a style rule.** A
/// tolerance below a day cannot be satisfied by any gap at all, because the store derives its
/// holes from `date=` partitions. `--max-gap 1d` and wider carry no such note.
#[test]
fn a_sub_day_tolerance_says_the_store_cannot_answer_that_finely() {
    let clean = || Candidate { gaps: Some(Vec::new()), ..bars(400) };
    let fine = gate_args(spec("binance:BTCUSDT:1h"), 1, Some(4 * 3_600_000), &["bar"]);
    let js = judge(&fine, &[clean()]);
    let text = lines(&fine, &js, rung(&js), 1, 1).join("\n");
    assert!(text.contains("whole UTC day"), "{text}");
    assert!(text.contains("no missing day at all"), "{text}");

    let coarse = gate_args(spec("binance:BTCUSDT:1h"), 1, Some(MS_PER_DAY), &["bar"]);
    let js = judge(&coarse, &[clean()]);
    let text = lines(&coarse, &js, rung(&js), 1, 1).join("\n");
    assert!(!text.contains("whole UTC day"), "a day-or-wider tolerance is answerable: {text}");
}

/// The table names every criterion — the passing ones included — because §7.1 says the verdict
/// is a document rather than a number, and because a CI step that prints its gate on success is
/// the normal case.
#[test]
fn the_table_renders_every_criterion_and_its_own_summary() {
    let g = gate_args(spec("binance:BTCUSDT:1h"), 365, Some(MS_PER_DAY), &["bar"]);
    // PROBED with an empty answer — the shape `execute_gate` always produces when `--max-gap`
    // was given, so only the days criterion fails and the summary's count is the one a run
    // would print. A `gaps: None` here would go UNEVALUATED and make this case assert a
    // rendering no operator can reach.
    let js = judge(&g, &[Candidate { gaps: Some(Vec::new()), ..bars(200) }]);
    let exit = rung(&js);
    let text = lines(&g, &js, exit, 1, 9).join("\n");
    assert!(text.starts_with("gate binance:BTCUSDT:1h"), "{text}");
    assert!(text.contains("1 of 9 stored series match this spec"), "{text}");
    assert!(text.contains("CRITERION") && text.contains("VERDICT"), "{text}");
    assert!(text.contains(REQUIRE_DAYS) && text.contains(MAX_GAP), "{text}");
    assert!(text.contains(KIND_PRESENT), "the presence criterion is rendered too: {text}");
    assert!(text.contains("BREACH — 1 of 3 criteria"), "{text}");
}

/// A duration prints in the grammar `--max-gap` accepts, so a cell can be pasted straight back
/// into the flag. `1.5d` would name a spelling that grammar refuses.
#[test]
fn a_duration_cell_is_pasteable_back_into_the_flag() {
    assert_eq!(duration_cell(MS_PER_DAY), "1d");
    assert_eq!(duration_cell(7 * MS_PER_DAY), "7d");
    assert_eq!(duration_cell(4 * 3_600_000), "4h");
    assert_eq!(duration_cell(36 * 3_600_000), "36h");
    assert_eq!(duration_cell(1_500), "1500ms");
    for cell in [duration_cell(MS_PER_DAY), duration_cell(4 * 3_600_000)] {
        assert!(parse_max_gap(&cell).is_ok(), "{cell} must parse back");
    }
}

/// The document carries the numbers each verdict was derived FROM, so a consumer can re-derive
/// a judgement rather than trust it — and an unmade gap probe is `null` rather than an empty
/// array, which would say "no holes".
#[test]
fn the_document_carries_the_evidence_and_never_fakes_an_unmade_probe() {
    let probed = Candidate { gaps: Some(vec![(MS_PER_DAY, 2 * MS_PER_DAY - 1)]), ..bars(10) };
    let docs = json_series(&[probed, bars(10)]);
    assert_eq!(docs[0]["span_days"], 10);
    assert_eq!(docs[0]["span_ms"], 10 * MS_PER_DAY);
    assert_eq!(docs[0]["largest_gap_ms"], MS_PER_DAY);
    assert_eq!(docs[0]["gaps"][0]["from_ts"], MS_PER_DAY);
    assert_eq!(docs[0]["gaps"][0]["to_ts"], 2 * MS_PER_DAY - 1);
    assert!(docs[1]["gaps"].is_null(), "an unmade probe is null, never []: {}", docs[1]);
    assert!(docs[1]["largest_gap_ms"].is_null(), "{}", docs[1]);
}

/// An unevaluated criterion carries its reason into the document; everything else carries
/// `null` under the same key, so a reader's key set does not change shape between runs.
#[test]
fn the_document_explains_only_what_went_unevaluated() {
    let broken =
        Candidate { gaps: None, gaps_error: Some("cannot read manifest".to_string()), ..bars(400) };
    let g = gate_args(spec("binance:BTCUSDT:1h"), 365, Some(MS_PER_DAY), &["bar"]);
    let docs = json_criteria(&judge(&g, &[broken]));
    assert_eq!(docs[1]["verdict"], "pass");
    assert!(docs[1]["why"].is_null(), "{}", docs[1]);
    assert_eq!(docs[2]["verdict"], "unevaluated");
    assert!(
        docs[2]["why"].as_str().unwrap_or_default().contains("cannot read manifest"),
        "{}",
        docs[2]
    );
}
