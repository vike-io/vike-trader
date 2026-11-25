use super::*;
use std::assert_matches;

fn spec() -> NamedRunSpec {
    NamedRunSpec {
        strategy: "buy_hold".to_string(),
        params: vec![("size".to_string(), NamedRunParam::Num(1.0))],
        venue: "binance".to_string(),
        symbol: "BTCUSDT".to_string(),
        interval: "1h".to_string(),
        start: 0,
        end: 3_600_000 * 100,
    }
}

/// **THE STRUCTURAL PROPERTY, stated as a test because the whole classification rests on it.**
///
/// A named run's params carrier has no variant a script could occupy, so a request cannot carry
/// source even before anything refuses one. The proof is a serde round trip rather than an
/// inspection: a `NamedRunParam` encoded from a JSON STRING simply does not decode, which is the
/// same thing a hostile peer would discover.
#[test]
fn the_request_type_has_nowhere_to_put_a_script() {
    // Every legal shape decodes...
    for json in ["{\"Int\":4}", "{\"Num\":2.5}", "{\"Flag\":true}"] {
        serde_json::from_str::<NamedRunParam>(json)
            .unwrap_or_else(|e| panic!("{json} must decode: {e}"));
    }
    // ...and a script does not, under any tag this enum has.
    for json in [
        "{\"Text\":\"fn on_bar(strategy) { strategy.close_all(); }\"}",
        "{\"Str\":\"fn on_bar() {}\"}",
        "{\"Int\":\"fn on_bar() {}\"}",
        "{\"Num\":\"fn on_bar() {}\"}",
        "{\"Flag\":\"fn on_bar() {}\"}",
        "\"fn on_bar() {}\"",
    ] {
        assert!(
            serde_json::from_str::<NamedRunParam>(json).is_err(),
            "a NamedRunParam must have nowhere to put a script, yet {json} decoded"
        );
    }
    // ...and so does a whole spec whose params try to smuggle one.
    let hostile = "{\"strategy\":\"buy_hold\",\"params\":[[\"src\",\"fn on_bar() {}\"]],\
                       \"venue\":\"binance\",\"symbol\":\"BTCUSDT\",\"interval\":\"1h\",\
                       \"start\":0,\"end\":1}";
    assert!(
        serde_json::from_str::<NamedRunSpec>(hostile).is_err(),
        "a NamedRunSpec whose param value is a string must not decode at all"
    );
}

/// The BELT, and it is labelled one: a `src` key whose VALUE is a number is structurally inert
/// (nothing in the resolution closure reads it, and it is not a string anyway), and it is still
/// refused — so a caller who believes they are shipping a script is told they are not.
#[test]
fn the_reserved_source_key_is_refused_by_name_even_as_a_number() {
    let mut s = spec();
    s.params = vec![(vike_model::RESERVED_SOURCE_KEY.to_string(), NamedRunParam::Num(1.0))];
    let err = validate_named_run(&s).expect_err("the reserved key is refused");
    assert_eq!(
        err,
        "a named run may not carry a param called `src`. That key is a strategy's SOURCE rather \
         than one of its knobs, and a NAMED run carries no source: the request type has no field a \
         script could occupy, and the resolution closure holds no compiler to read one. This is \
         refused rather than ignored so a caller who believes they are shipping a script learns \
         they are not — put the strategy in a file under strategies/ and name it with \
         `[strategy] file` in the profile of `vike-cli backtest run`, a Control-scope verb for \
         exactly that reason (docs/decisions/0064-a-named-run-carries-no-source.md)."
    );
}

/// Every refusal names the CONSTANT it hit, which is what makes a bound actionable rather than
/// merely present. One case per bound.
#[test]
fn every_bound_names_itself_in_its_refusal() {
    let cases: Vec<(NamedRunSpec, &str)> = vec![
        (NamedRunSpec { strategy: "x".repeat(65), ..spec() }, "NAMED_RUN_MAX_STRATEGY_BYTES"),
        (
            NamedRunSpec {
                params: (0..33).map(|i| (format!("k{i}"), NamedRunParam::Int(1))).collect(),
                ..spec()
            },
            "NAMED_RUN_MAX_PARAMS",
        ),
        (
            NamedRunSpec { params: vec![("k".repeat(49), NamedRunParam::Int(1))], ..spec() },
            "NAMED_RUN_MAX_PARAM_KEY_BYTES",
        ),
        (NamedRunSpec { interval: "1w".to_string(), ..spec() }, "NAMED_RUN_INTERVALS"),
        (NamedRunSpec { start: 0, end: 3_600_000 * 60_000, ..spec() }, "NAMED_RUN_MAX_BARS"),
        (NamedRunSpec { symbol: "B".repeat(33), ..spec() }, "SEED_MAX_SYMBOL_BYTES"),
        (NamedRunSpec { venue: "v".repeat(17), ..spec() }, "CATALOG_MAX_VENUE_BYTES"),
    ];
    for (s, needle) in cases {
        let err = validate_named_run(&s).expect_err("must be refused");
        assert!(err.contains(needle), "the refusal must name {needle}, got: {err}");
    }
}

/// The window ceiling REFUSES; it never clamps. Stated as its own test because "clamp it" is the
/// obvious kindness and it is the one `docs/decisions/0062`'s decision 5 argues is a lie.
#[test]
fn an_over_wide_window_is_refused_rather_than_narrowed() {
    let s = NamedRunSpec { interval: "1m".to_string(), start: 0, end: 60_000 * 60_000, ..spec() };
    let err = validate_named_run(&s).expect_err("must be refused");
    assert!(err.contains("REFUSED rather than clamped"), "{err}");
    // …and the boundary itself is admitted, so the bound is a ceiling rather than an off-by-one.
    let exact = NamedRunSpec {
        interval: "1m".to_string(),
        start: 0,
        end: 60_000 * i64::from(NAMED_RUN_MAX_BARS - 1),
        ..spec()
    };
    validate_named_run(&exact).expect("exactly NAMED_RUN_MAX_BARS bars is admitted");
}

/// Rule 1 of [`NAMED_RUN_INTERVALS`]' derivation, asserted rather than trusted: an entry the
/// store vocabulary cannot price would have no window, and [`named_run_bars`] would answer
/// `None` for an interval [`validate_named_run_interval`] had just accepted.
#[test]
fn every_permitted_interval_has_a_bar_width_the_store_can_price() {
    for iv in NAMED_RUN_INTERVALS {
        let ms = vike_marketdata::interval::interval_ms(iv);
        assert_matches!(ms, Some(n) if n > 0, "{iv} has no positive bar width: {ms:?}");
        assert!(named_run_bars(iv, 0, 1_000_000).is_some(), "{iv} has no window");
    }
}

/// The set is strictly increasing in bar width and free of duplicates — not cosmetic: it is
/// rendered into a refusal an operator reads, and a duplicate would mean two rows of the
/// derivation collapsed without anybody noticing.
#[test]
fn the_permitted_set_is_sorted_by_bar_width_and_free_of_duplicates() {
    let widths: Vec<i64> = NAMED_RUN_INTERVALS
        .iter()
        .map(|iv| vike_marketdata::interval::interval_ms(iv).unwrap())
        .collect();
    assert!(widths.windows(2).all(|w| w[0] < w[1]), "not strictly increasing: {widths:?}");
}

/// The one entry where this set and the SEED set visibly disagree, pinned so a future "align
/// them" pass has to read both derivations first.
#[test]
fn one_second_is_readable_here_and_not_seedable() {
    assert!(NAMED_RUN_INTERVALS.contains(&"1s"), "the store can hold 1s bars");
    // ...and the seed lane still refuses it, because bybit and okx refuse it from their own code
    // tables: `crates/vike-datahub-client/src/seed.rs`'s
    // `the_intervals_no_venue_or_no_store_can_serve_are_refused_by_name` holds that half. The two sets
    // answer different questions; do not unify them.
}

/// A non-finite numeric param is refused at the door rather than becoming a decode failure on
/// the far side.
#[test]
fn a_non_finite_param_is_refused_before_it_becomes_a_desync() {
    for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let s = NamedRunSpec { params: vec![("g".to_string(), NamedRunParam::Num(bad))], ..spec() };
        let err = validate_named_run(&s).expect_err("must be refused");
        assert!(err.contains("non-finite"), "{err}");
    }
}

/// The unarmed sentence names the switch and the box — the teaching-refusal rule, held rather
/// than trusted, because an empty roster and an unarmed lane look identical from a picker.
#[test]
fn the_unarmed_note_names_the_flag_and_the_box() {
    let note = NamedRunRoster::unarmed_note();
    assert!(note.contains("vike-backend backtesthub --addr --named-run"), "{note}");
    // ...and it says the roster is WITHHELD rather than absent, which is the half a picker
    // needs: an unarmed daemon answers an empty `strategies` list (0064's decision 8 leg 3),
    // and without this sentence that is indistinguishable from a daemon holding none.
    assert!(note.contains("names no strategies"), "{note}");
}

/// The client's encode and the server's decode of a knob are inverses for the three kinds, and a
/// string is refused BY NAME — the reserved source key with its own sentence.
#[test]
fn a_knob_round_trips_and_a_string_is_refused() {
    for v in [ParamValue::Int(4), ParamValue::Float(2.5), ParamValue::Bool(true)] {
        assert_eq!(NamedRunParam::from_param("k", &v).expect("a number or flag").to_param(), v);
    }
    let text = ParamValue::Str("BTCUSDT".to_string());
    let err = NamedRunParam::from_param("symbol", &text).expect_err("a string has no carrier");
    assert!(err.contains("\"symbol\"") && err.contains("no text variant"), "{err}");
    let src =
        NamedRunParam::from_param(vike_model::RESERVED_SOURCE_KEY, &text).expect_err("a script");
    assert_eq!(
        src,
        "this strategy carries a `src` param, which is a SCRIPT rather than a knob. A named run \
         carries no source: put the strategy in a file under strategies/ and name it with \
         `[strategy] file` in the profile of `vike-cli backtest run`, which is Control-scope for \
         exactly this reason."
    );
}

/// A well-formed request passes every bound — so the tests above cannot be passing because the
/// validator refuses everything.
#[test]
fn an_ordinary_request_is_admitted() {
    validate_named_run(&spec()).expect("an ordinary named run is admitted");
}
