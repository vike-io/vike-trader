use super::*;
use std::assert_matches;

fn sel<'a>(optimizer: Option<&'a str>, knob: Option<(&'a str, &'a str)>) -> SearchSelection<'a> {
    let mut s = SearchSelection { optimizer, ..SearchSelection::default() };
    match knob {
        Some(("--euler-depth", v)) => s.euler_depth = Some(v),
        Some(("--trials", v)) => s.trials = Some(v),
        Some(("--seed", v)) => s.seed = Some(v),
        Some((other, _)) => panic!("unknown knob in this fixture: {other}"),
        None => {}
    }
    s
}

/// ⚠ THE ONE-ROSTER PROPERTY, on the side that IMPLEMENTS it: every name the protocol says it
/// can carry builds a method whose `Optimizer::name` is that name. A fifth entry in
/// `SEARCH_METHODS` with no arm here reddens this — the §15.1 gate's engine leg.
#[test]
fn every_protocol_method_builds_the_method_it_names() {
    for name in SEARCH_METHODS {
        // genetic REQUIRES a seed by design, so the fixture supplies one; nothing else does.
        let knob = (name == "genetic").then_some(("--seed", "7"));
        let method =
            resolve(&sel(Some(name), knob)).unwrap_or_else(|e| panic!("{name} must resolve: {e}"));
        assert_eq!(optimizer_for(method).name(), name, "--optimizer {name} must build {name:?}");
        assert_eq!(identity_parts(&method).0, name, "the artifact identity names it too");
    }
}

/// No selector at all is the exhaustive grid — what this verb has always run, and what an
/// omitted wire field must mean.
#[test]
fn an_empty_selection_is_the_default_method() {
    let method = resolve(&SearchSelection::default()).expect("an empty selection resolves");
    assert_eq!(optimizer_for(method).name(), DEFAULT_SEARCH_METHOD);
}

/// Every knob is refused by every method that does not own it, and the refusal names EVERY
/// owner rather than the first — an operator who wrote `--optimizer grid --seed 7` must be told
/// about genetic as well as tpe.
#[test]
fn a_knob_is_refused_by_every_method_that_does_not_own_it() {
    for (flag, owners) in METHOD_KNOBS {
        for name in SEARCH_METHODS {
            if owners.iter().any(|o| name.eq_ignore_ascii_case(o)) {
                continue;
            }
            let err = resolve(&sel(Some(name), Some((flag, "1"))))
                .expect_err("a knob under a non-owner must be refused");
            assert!(err.contains(flag), "the refusal names the knob: {err}");
            assert!(err.contains(name), "the refusal names the method selected: {err}");
            for owner in *owners {
                assert!(err.contains(owner), "the refusal names EVERY owner ({owner}): {err}");
            }
        }
    }
}

/// ⚠ The seam's whole point: a VALUE is judged once, by this parser, so the message an operator
/// reads is the same whether they typed `--local` or `--addr`.
#[test]
fn a_bad_value_is_refused_by_the_flag_that_owns_it() {
    for (optimizer, flag, value, needle) in [
        ("tpe", "--trials", "abc", "invalid --trials"),
        ("tpe", "--trials", "0", "invalid --trials"),
        ("tpe", "--seed", "-1", "invalid --seed"),
        ("euler", "--euler-depth", "abc", "invalid --euler-depth"),
    ] {
        let err = resolve(&sel(Some(optimizer), Some((flag, value))))
            .expect_err("a bad value must be refused");
        assert!(err.contains(needle), "{flag} {value}: {err}");
    }
}

/// The euler cap is a RANGE CHECK, not a clamp — `EulerConfig::with_depth` clamps silently and
/// the budget line would then report a depth nobody typed. Moved verbatim with the parser.
#[test]
fn a_depth_past_the_cap_is_refused_rather_than_clamped() {
    let over = (EulerConfig::MAX_DEPTH_CAP + 1).to_string();
    let err = resolve(&sel(Some("euler"), Some(("--euler-depth", &over))))
        .expect_err("past the cap must refuse");
    assert!(err.contains("cap"), "{err}");
}

/// genetic refuses an absent seed and tpe does not, and the asymmetry is a property of the
/// METHOD rather than of the flag — `GeneticConfig::new` takes the seed as a required parameter
/// and its doc says why.
#[test]
fn genetic_requires_a_seed_and_tpe_does_not() {
    let err = resolve(&sel(Some("genetic"), None)).expect_err("genetic needs a seed");
    assert!(err.contains("--seed"), "{err}");
    assert!(resolve(&sel(Some("tpe"), None)).is_ok(), "tpe's absent seed is its documented 0");
}

/// An unknown method names the roster it is not in, rendered from the const so the message and
/// the check cannot disagree.
#[test]
fn an_unknown_method_names_the_roster() {
    let err = resolve(&sel(Some("bogus"), None)).expect_err("bogus is not a method");
    for name in SEARCH_METHODS {
        assert!(err.contains(name), "the refusal lists {name}: {err}");
    }
}

/// The five ranking names, including the composite the WIRE could not carry before stage 7.
#[test]
fn every_rank_name_resolves_and_a_typo_does_not() {
    assert_eq!(resolve_rank(None).unwrap(), RankChoice::Metric(RankMetric::default()));
    assert_eq!(resolve_rank(Some("MULTI")).unwrap(), RankChoice::Multi);
    for name in ["sharpe", "return", "max_dd", "equity"] {
        assert_matches!(resolve_rank(Some(name)).unwrap(), RankChoice::Metric(_), "{name}");
    }
    let err = resolve_rank(Some("bogus")).expect_err("a typo is refused");
    assert!(err.contains("multi"), "the refusal advertises the composite too: {err}");
}

/// **THE GATE. It holds the shared VOCABULARY equal to what this module actually accepts, so
/// the two argv parsers on this surface cannot answer one spelling two ways.**
///
/// `vike_datahub_client::flag_vocab` is data below both parsers; this function is the only
/// implementation of what a `--rank-by` value MEANS. Nothing but this test connects them, and
/// without it the vocabulary is a fourth hand copy of the roster with better manners. Both
/// directions are checked, because both have a live failure mode: a roster member this module
/// refuses would make the client ACCEPT a value the engine then rejects after a spawn (or a
/// round trip), and a value this module accepts that the roster omits is the disagreement that
/// was MEASURED — the client refused `--rank-by SHARPE` while this side's own test pinned
/// `RankMetric::from_str_ci("SHARPE")` as `Some`.
///
/// ⚠ The CASE half is the part to read twice. `resolve_rank` is case-insensitive on both arms
/// (`eq_ignore_ascii_case` for `multi`, `from_str_ci` for the four), so the vocabulary's
/// predicate is too — and that direction was a deliberate choice to WIDEN the client rather
/// than narrow the engine, because `--rank-by SHARPE` works on a box today.
#[test]
fn the_rank_by_roster_is_exactly_what_resolve_rank_accepts() {
    let roster = flag_vocab::value_roster(RANK_BY_FLAG);
    assert!(!roster.is_empty(), "a --rank-by row with no values would make this test vacuous");
    for &name in roster {
        assert!(resolve_rank(Some(name)).is_ok(), "{name} is on the roster and must resolve");
        let shouted = name.to_ascii_uppercase();
        assert!(resolve_rank(Some(shouted.as_str())).is_ok(), "{shouted} resolves as {name}");
    }
    // …and membership is not widened by the case rule: a non-member is refused in every case,
    // and the refusal RENDERS the same roster this test read.
    for bad in ["bogus", "BOGUS", "sharp", ""] {
        let e = resolve_rank(Some(bad)).expect_err("a non-member must be refused");
        assert_eq!(e, flag_vocab::refuse_value(RANK_BY_FLAG, bad), "one sentence, both routes");
    }
}

/// The `--optimizer` twin of the gate above. This roster is ALREADY one const
/// ([`SEARCH_METHODS`], read by both sides), so what this adds is the CASE half — the second
/// measured disagreement, where `--optimizer TPE` resolved here and was refused by the client's
/// exact-match spelling check.
///
/// ⚠ It asserts the absence of the INVALID-METHOD refusal rather than `is_ok`, deliberately: a
/// roster name can legitimately fail for a reason that is not about its spelling — `genetic`
/// with no seed is refused by `require_seed`, and that refusal is correct. Asserting `is_ok`
/// would force this test to reproduce each method's own construction rules, which is the second
/// copy of the ownership table this module exists to prevent.
#[test]
fn the_optimizer_roster_is_exactly_what_resolve_accepts() {
    for &name in flag_vocab::value_roster("--optimizer") {
        for spelling in [name.to_string(), name.to_ascii_uppercase()] {
            if let Err(e) = resolve(&sel(Some(spelling.as_str()), None)) {
                assert!(!e.contains("invalid --optimizer"), "{spelling} is a method: {e}");
            }
        }
    }
    for bad in ["bayes", "BAYES", "gri", ""] {
        let e = resolve(&sel(Some(bad), None)).expect_err("a non-member must be refused");
        assert!(e.contains("invalid --optimizer"), "{bad:?} must be refused as a method: {e}");
    }
}

/// The vocabulary's ARITY rows are about flags this module has no opinion on, with ONE
/// exception that matters: every method KNOB it owns must be a row the vocabulary calls
/// `Valued`. A knob declared `Bare` there would tell a triage to refuse `--trials=8`, which
/// both parsers accept and which `parse_trials` reads.
#[test]
fn every_method_knob_is_a_valued_row_in_the_shared_vocabulary() {
    for &(flag, _) in METHOD_KNOBS {
        let row = flag_vocab::spec(flag)
            .unwrap_or_else(|| panic!("{flag} is a method knob and needs a vocabulary row"));
        assert_eq!(row.arity, flag_vocab::Arity::Valued, "{flag} takes a value");
        assert_eq!(row.route, flag_vocab::Route::Both, "{flag} is forwarded by the client");
    }
}

/// ⚠ THE PRESERVATION RULE, asserted rather than left in a comment: grid + a classic metric is
/// the ONE combination that keeps `RankBy::Metric` (which clears every row's `score`), and
/// everything else stamps an objective label. Getting this wrong changes a shipped JSON
/// document — `crates/vike-backtest/tests/compute_profile_roundtrip.rs`'s
/// `profile_sweep_is_byte_identical_local_and_remote` is what goes red.
#[test]
fn only_grid_with_a_classic_metric_takes_the_classic_evaluator() {
    assert!(uses_classic_evaluator(SearchMethod::Grid, RankChoice::Metric(RankMetric::Sharpe)));
    assert!(!uses_classic_evaluator(SearchMethod::Grid, RankChoice::Multi));
    let tpe = resolve(&sel(Some("tpe"), None)).expect("tpe resolves");
    assert!(!uses_classic_evaluator(tpe, RankChoice::Metric(RankMetric::Sharpe)));
}

// ── `--min-trades`, `--progress`, and the optimizer listing ───────────────────────────────
//
// ⚠ This header read "and the door-less optimizer listing". All three have argv doors now
// (`crates/vike-backtest/src/backtest_cli/search_flags.rs`'s `parse_search_flags` for the two
// observers, its `--list-optimizers` arm for the listing), and the gates below hold the shared
// vocabulary to what these resolvers accept — the property that only became checkable once a
// parser read them.

/// The floor's three dispositions: absent is disarmed, `0` is disarmed, and a count arms it.
///
/// ⚠ `0` being ACCEPTED is the load-bearing half. An operator scripting a matrix writes
/// `--min-trades $FLOOR` with `FLOOR=0` for the control arm; refusing it would force a
/// conditional argv, which is where a flag gets dropped from the arm that needed it.
#[test]
fn the_trade_floor_accepts_zero_as_disarmed() {
    assert_eq!(resolve_min_trades(None), Ok(TradeFloor::DISARMED));
    assert_eq!(resolve_min_trades(Some("0")), Ok(TradeFloor::DISARMED));
    let armed = resolve_min_trades(Some("50")).expect("a count arms it");
    assert!(armed.is_armed());
    assert_eq!(armed.min_trades(), 50);
}

/// The refusal names what `0` means, because `usize` already rejects a negative and the operator
/// who typed `-1` needs to be told which end of the range they wanted rather than merely that
/// their value is not a number.
#[test]
fn a_non_integer_trade_floor_is_refused_by_name() {
    for bad in ["-1", "50.0", "fifty", ""] {
        let e = resolve_min_trades(Some(bad))
            .expect_err("a value that is not a non-negative integer must be refused");
        assert!(e.contains("--min-trades"), "the refusal names the flag ({bad:?}): {e}");
        assert!(e.contains("0 disarms the floor"), "…and what 0 means ({bad:?}): {e}");
    }
}

/// ⚠ `--min-trades` is deliberately NOT a [`METHOD_KNOBS`] row, so it is accepted under EVERY
/// method. A row would refuse it under three of the four for no reason at all: the floor
/// configures the EVALUATOR, which every method drives.
#[test]
fn the_trade_floor_is_owned_by_no_method() {
    assert!(
        !METHOD_KNOBS.iter().any(|(flag, _)| *flag == "--min-trades"),
        "a row here would refuse a method-agnostic knob under every method but one"
    );
    assert!(
        !METHOD_KNOBS.iter().any(|(flag, _)| *flag == "--progress"),
        "and the same for the progress stream, which is not a search knob at all"
    );
}

/// **THE `--progress` TWIN OF [`the_rank_by_roster_is_exactly_what_resolve_rank_accepts`], and
/// it holds a COPY against its SOURCE.**
///
/// `vike_datahub_client::flag_vocab`'s `PROGRESS_MODES` is a hand copy and cannot be anything
/// else: that crate sits BELOW this one and `crates/vike-ops/tests/architecture/layer_gate.rs` fails the
/// edge, so [`ProgressMode::NAMES`] — the ONE roster [`resolve_progress`] walks and its refusal
/// renders — is not nameable there. This crate is the only one that can see both, which is why
/// the gate lives here and not beside the copy.
///
/// Both directions are checked, because both have a live failure mode. A member the copy omits
/// makes a triage refuse a spelling this resolver accepts; a member the copy adds makes it
/// ADVERTISE a mode `from_str_ci` answers `None` for. The ORDER is asserted too, because
/// `flag_vocab::refuse_value` joins the copy with `|` while [`resolve_progress`]'s own refusal
/// joins `NAMES`, and two differently-ordered sentences for one flag is the disagreement class
/// that module exists to end.
///
/// ⚠ The mutation this fails on, in PRODUCTION: add a fourth `ProgressMode` variant with its
/// `NAMES` row and `from_str_ci` arm, and leave `PROGRESS_MODES` at three — which is exactly
/// the edit `optimize.rs`'s own completeness test cannot see, because that test never looks
/// outside its crate. Deleting `"json"` from `PROGRESS_MODES` reddens it from the other side.
#[test]
fn the_progress_roster_is_exactly_what_resolve_progress_accepts() {
    let roster = flag_vocab::value_roster(PROGRESS_FLAG);
    assert!(!roster.is_empty(), "an empty --progress roster would make this test vacuous");
    assert_eq!(
        roster,
        &ProgressMode::NAMES[..],
        "left = `vike_datahub_client::flag_vocab`'s hand copy, right = the roster this \
             module's resolver walks and its refusal renders. Same members, SAME ORDER — the two \
             are joined into two refusal sentences for one flag."
    );
    for &name in roster {
        assert!(resolve_progress(Some(name)).is_ok(), "{name} is on the roster and resolves");
        let shouted = name.to_ascii_uppercase();
        assert!(resolve_progress(Some(shouted.as_str())).is_ok(), "{shouted} resolves");
    }
    // …and membership is not widened by the case rule. Both sides refuse a non-member, which is
    // what makes the copy safe to spelling-check against.
    for bad in ["verbose", "VERBOSE", "aut", ""] {
        assert!(resolve_progress(Some(bad)).is_err(), "{bad:?} is not a mode");
        assert!(!flag_vocab::accepts_value(PROGRESS_FLAG, bad), "{bad:?}");
    }
}

/// The three ENGINE-ONLY rows that landed with their doors, held against the facts a triage
/// would read off them: both observers take a VALUE (a `Bare` row would tell one to refuse
/// `--progress=json`, which both parsers accept), the listing takes none, and none of the three
/// is `Both` — the wire carries no field for the floor and no stream for the progress sink.
///
/// ⚠ It is the `every_method_knob_is_a_valued_row_in_the_shared_vocabulary` shape, applied to
/// the flags that are deliberately NOT method knobs. That test asserts `Route::Both` because a
/// knob is forwarded by the client; this one asserts the opposite for the same reason read
/// backwards — a `Both` row here would say the client sends what it cannot send.
///
/// ⚠ The mutation this fails on, in PRODUCTION: promote either observer row to `Route::Both`
/// without adding a `WireSearch` field. That is the edit that would make the client accept
/// `--min-trades`, forward it on `--local` and DROP it on `--addr` — the silent downgrade
/// `crate::proto`'s `FEATURE_SEARCH_METHOD` negotiation exists to refuse.
#[test]
fn the_observer_flags_are_engine_only_rows_in_the_shared_vocabulary() {
    for flag in [MIN_TRADES_FLAG, PROGRESS_FLAG] {
        let row = flag_vocab::spec(flag)
            .unwrap_or_else(|| panic!("{flag} has an argv door and needs a vocabulary row"));
        assert_eq!(row.arity, flag_vocab::Arity::Valued, "{flag} takes a value");
        assert_eq!(row.route, flag_vocab::Route::EngineOnly, "{flag} reaches no client route");
        assert!(!row.why.is_empty(), "{flag}: a row without its reason is a row nobody edits");
    }
    // The floor declares no roster: a count is free-form and the ENGINE owns its range, exactly
    // as `--trials` and `--euler-depth` are forwarded unvalidated by the client.
    assert!(flag_vocab::value_roster(MIN_TRADES_FLAG).is_empty());
    for n in ["0", "1", "50"] {
        assert!(resolve_min_trades(Some(n)).is_ok(), "{n} is a count");
        assert!(flag_vocab::accepts_value(MIN_TRADES_FLAG, n), "{n}");
    }
    let listing = flag_vocab::spec("--list-optimizers")
        .expect("--list-optimizers has an argv door and needs a vocabulary row");
    assert_eq!(listing.arity, flag_vocab::Arity::Bare, "a listing names no value");
    assert_eq!(listing.route, flag_vocab::Route::EngineOnly, "the client publishes an asset");
}

/// Every `--progress` spelling resolves, and the refusal RENDERS the roster rather than
/// restating it — so a fourth mode cannot be accepted by the parser and missing from the
/// message.
#[test]
fn the_progress_refusal_renders_its_own_roster() {
    assert_eq!(resolve_progress(None), Ok(ProgressMode::Auto), "unset is auto");
    for name in ProgressMode::NAMES {
        assert!(resolve_progress(Some(name)).is_ok(), "{name} must resolve");
    }
    let e = resolve_progress(Some("verbose")).expect_err("not a mode");
    assert!(e.contains("--progress"), "{e}");
    for name in ProgressMode::NAMES {
        assert!(e.contains(name), "the refusal must offer {name}: {e}");
    }
}

/// The optimizer LISTING renders the PROTOCOL const and can render nothing else — one name per
/// line and nothing else, the shape `vike-cli backtest strategies` already prints, so a script
/// that piped one can pipe the other.
///
/// ⚠ This test was named `the_roster_door_renders_the_one_roster` and its doc opened "The
/// `--list-optimizers` door", which was the strongest claim in this file that the listing had
/// SHIPPED — and it had not: no flag, no FLAGS row, no arm. The renderer is what this covers,
/// and [`optimizer_roster_lines`] carries the deferral. The properties asserted below are
/// unchanged; only the name and the first sentence were false.
#[test]
fn the_roster_renderer_renders_the_one_roster() {
    let rendered = optimizer_roster_lines();
    let lines: Vec<&str> = rendered.lines().collect();
    assert_eq!(lines, SEARCH_METHODS.to_vec(), "the const, in its own order");
    assert!(
        !rendered.contains(':'),
        "no header, no count, no default marker — a pipeline must not have to strip anything"
    );
    // Every listed name is a name [`resolve`] actually accepts: a listing that advertises a
    // method the selector refuses is worse than no listing at all.
    for &name in &lines {
        // ⚠ genetic's `--seed` is REQUIRED, so its selection needs one to be COMPLETE. The
        // listing is still right — the method exists and the flag is its own documented input.
        let seed = (name == "genetic").then_some("7");
        let built = resolve(&SearchSelection { optimizer: Some(name), seed, ..Default::default() });
        assert!(built.is_ok(), "{name} is listed and must select: {built:?}");
    }
}

/// The JSON listing carries the two facts the plain one omits — the count and the DEFAULT — and
/// stays one parseable object on one line.
#[test]
fn the_roster_json_names_the_default() {
    let doc = optimizer_roster_json();
    assert!(doc.starts_with('{') && doc.ends_with('}'), "one object: {doc}");
    assert!(doc.contains(&format!("\"count\":{}", SEARCH_METHODS.len())), "{doc}");
    assert!(doc.contains(&format!("\"default\":\"{DEFAULT_SEARCH_METHOD}\"")), "{doc}");
    for name in SEARCH_METHODS {
        assert!(doc.contains(&format!("\"{name}\"")), "{name} missing from {doc}");
    }
    assert!(!doc.contains('\n'), "a caller owns the line ending: {doc}");
}
