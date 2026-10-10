use super::*;
use std::assert_matches;

fn spec() -> NamedRunSpec {
    NamedRunSpec {
        strategy: "buy_hold".to_string(),
        params: vec![("size".to_string(), NamedParam::Num(1.0))],
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
/// inspection: a `NamedParam` encoded from a JSON STRING simply does not decode, which is the
/// same thing a hostile peer would discover.
#[test]
fn the_request_type_has_nowhere_to_put_a_script() {
    // Every legal shape decodes...
    for json in ["{\"Int\":4}", "{\"Num\":2.5}", "{\"Flag\":true}"] {
        serde_json::from_str::<NamedParam>(json)
            .unwrap_or_else(|e| panic!("{json} must decode: {e}"));
    }
    // ...and a script does not, under any tag this enum has.
    for json in [
        "{\"Text\":\"fn on_bar() { buy(1.0); }\"}",
        "{\"Str\":\"fn on_bar() {}\"}",
        "{\"Int\":\"fn on_bar() {}\"}",
        "{\"Num\":\"fn on_bar() {}\"}",
        "{\"Flag\":\"fn on_bar() {}\"}",
        "\"fn on_bar() {}\"",
    ] {
        assert!(
            serde_json::from_str::<NamedParam>(json).is_err(),
            "a NamedParam must have nowhere to put a script, yet {json} decoded"
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
    s.params = vec![(vike_model::RESERVED_SRC_KEY.to_string(), NamedParam::Num(1.0))];
    let err = validate_named_run(&s).expect_err("the reserved key is refused");
    assert!(err.contains("carries no source"), "{err}");
    assert!(err.contains("--script"), "it names the verb that DOES ship source: {err}");
}

/// Every refusal names the CONSTANT it hit, which is what makes a bound actionable rather than
/// merely present. One case per bound.
#[test]
fn every_bound_names_itself_in_its_refusal() {
    let cases: Vec<(NamedRunSpec, &str)> = vec![
        (NamedRunSpec { strategy: "x".repeat(65), ..spec() }, "NAMED_RUN_MAX_STRATEGY_BYTES"),
        (
            NamedRunSpec {
                params: (0..33).map(|i| (format!("k{i}"), NamedParam::Int(1))).collect(),
                ..spec()
            },
            "NAMED_RUN_MAX_PARAMS",
        ),
        (
            NamedRunSpec { params: vec![("k".repeat(49), NamedParam::Int(1))], ..spec() },
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
        let ms = vike_model::time::interval_ms(iv);
        assert_matches!(ms, Some(n) if n > 0, "{iv} has no positive bar width: {ms:?}");
        assert!(named_run_bars(iv, 0, 1_000_000).is_some(), "{iv} has no window");
    }
}

/// The set is strictly increasing in bar width and free of duplicates — not cosmetic: it is
/// rendered into a refusal an operator reads, and a duplicate would mean two rows of the
/// derivation collapsed without anybody noticing.
#[test]
fn the_permitted_set_is_sorted_by_bar_width_and_free_of_duplicates() {
    let widths: Vec<i64> =
        NAMED_RUN_INTERVALS.iter().map(|iv| vike_model::time::interval_ms(iv).unwrap()).collect();
    assert!(widths.windows(2).all(|w| w[0] < w[1]), "not strictly increasing: {widths:?}");
}

/// The one entry where this set and the SEED set visibly disagree, pinned so a future "align
/// them" pass has to read both derivations first.
#[test]
fn one_second_is_readable_here_and_not_seedable() {
    assert!(NAMED_RUN_INTERVALS.contains(&"1s"), "the store can hold 1s bars");
    assert!(
        !crate::seed::SEED_INTERVALS.contains(&"1s"),
        "…and the seed lane still refuses it, because bybit and okx refuse it from their own \
             code tables. The two sets answer different questions; do not unify them."
    );
}

/// A non-finite numeric param is refused at the door rather than becoming a decode failure on
/// the far side.
#[test]
fn a_non_finite_param_is_refused_before_it_becomes_a_desync() {
    for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let s = NamedRunSpec { params: vec![("g".to_string(), NamedParam::Num(bad))], ..spec() };
        let err = validate_named_run(&s).expect_err("must be refused");
        assert!(err.contains("non-finite"), "{err}");
    }
}

/// The unarmed sentence names the switch and the box — the teaching-refusal rule, held rather
/// than trusted, because an empty roster and an unarmed lane look identical from a picker.
#[test]
fn the_unarmed_note_names_the_variable_and_the_box() {
    let note = NamedRoster::unarmed_note();
    assert!(note.contains("VIKE_BACKTEST_NAMED_RUN=1"), "{note}");
    assert!(note.contains("vike-backend backtest --addr"), "{note}");
    // ...and it says the roster is WITHHELD rather than absent, which is the half a picker
    // needs: an unarmed daemon answers an empty `strategies` list (0064's decision 8 leg 3),
    // and without this sentence that is indistinguishable from a daemon holding none.
    assert!(note.contains("names no strategies"), "{note}");
}

/// A well-formed request passes every bound — so the tests above cannot be passing because the
/// validator refuses everything.
#[test]
fn an_ordinary_request_is_admitted() {
    validate_named_run(&spec()).expect("an ordinary named run is admitted");
}
