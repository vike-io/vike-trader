use super::*;

fn file() -> &'static Path {
    Path::new("preferences.toml")
}

#[test]
fn defaults_match_the_constants_they_replace() {
    let p = Preferences::default();
    assert_eq!(p.log_level, "info");
    assert_eq!(p.log_file_level, "trace");
}

#[test]
fn rust_log_wins_over_vike_log_matching_vike_log_init() {
    let mut p = Preferences::default();
    p.apply_env(&HashMap::from([
        (RUST_LOG_ENV.to_string(), "debug".to_string()),
        (LOG_LEVEL_ENV.to_string(), "warn".to_string()),
    ]))
    .unwrap();
    assert_eq!(p.log_level, "debug");
}

#[test]
fn vike_log_applies_when_rust_log_is_unset() {
    let mut p = Preferences::default();
    p.apply_env(&HashMap::from([(LOG_LEVEL_ENV.to_string(), "warn".to_string())])).unwrap();
    assert_eq!(p.log_level, "warn");
}

/// The tombstone REFUSES rather than ignoring, and every value is refused — including `0.40`,
/// which was this field's own DEFAULT and the value most likely to be sitting in a real
/// `preferences` row. An operator who wrote it believes they are pacing at 40 %; they were
/// already pacing at 40 % for an unrelated reason (the compiled-in constant), and would have
/// gone on believing the row did it.
#[test]
fn the_removed_rate_utilization_key_is_refused_and_names_where_the_number_lives_now() {
    for v in [0.40, 0.9, 1.5, 0.001] {
        let err = Preferences::default()
            .apply(PreferencesPatch { rate_utilization: Some(v), ..Default::default() }, file())
            .expect_err("a preference nothing reads must not load silently");
        let msg = err.to_string();
        assert!(msg.starts_with("preferences.toml: rate_utilization = "), "{msg}");
        assert!(msg.contains("NOTHING read it"), "says why it is gone: {msg}");
        assert!(
            msg.contains("DEFAULT_UTILIZATION"),
            "names where the number really comes from: {msg}"
        );
        assert!(
            msg.contains("policy.rate.max_utilization"),
            "names the ceiling half to remove too: {msg}"
        );
        assert!(
            msg.contains("no sanctioned way to remove this row yet")
                && msg.contains("no verb deletes a settings row")
                && msg.contains("`config unset`")
                && msg.contains("seal"),
            "a stale row has no sanctioned removal yet, and the refusal says why — a hand \
                 DELETE trips the adoption seal — instead of sending the operator to one: {msg}"
        );
        assert!(
            !msg.contains("Remove this row") && !msg.contains("settings database itself"),
            "the refusal must not advise the hand DELETE the seal refuses: {msg}"
        );
        assert!(!msg.contains("from policy.toml"), "no settings file is a remedy (0086): {msg}");
    }
}

#[test]
fn a_zero_sweep_pool_is_rejected() {
    let err = Preferences::default()
        .apply(PreferencesPatch { sweep_threads: Some(0), ..Default::default() }, file())
        .unwrap_err();
    assert!(err.to_string().contains("sweep_threads"), "{err}");
}

#[test]
fn a_non_numeric_sweep_thread_count_names_the_variable() {
    let env = HashMap::from([(SWEEP_THREADS_ENV.to_string(), "lots".to_string())]);
    let err = Preferences::default().apply_env(&env).unwrap_err();
    assert!(err.to_string().starts_with("VIKE_SWEEP_THREADS=lots: "), "{err}");
}

/// `check_non_blank` guards three keys and was pinned by nothing — a mutation sweep replaced
/// its whole body with `Ok(())` and every test here still passed, while its neighbour
/// (`sweep_threads == 0`) was pinned twice over.
///
/// The consequence is not cosmetic for `log_level`: a blank one reaches `vike_log::init` as
/// `EnvFilter::new("")`, which enables NOTHING and complains about nothing. A live daemon's
/// JSON audit file goes dark, silently, because somebody left a key with an empty value in a
/// `preferences` row — exactly the "configured something false" failure this crate's
/// consumption gate exists to prevent. `log_file_level` is the same story one layer down.
///
/// `chart_style` rides along because it shares the helper. It is deliberately NOT part of the
/// argument: it resolves as an INDEX, and a blank one is already inert.
#[test]
fn a_blank_preference_value_is_refused_by_key() {
    /// One row of the blank-value table: the key's name, and how to build a patch setting it.
    type BlankCase = (&'static str, fn(String) -> PreferencesPatch);

    let cases: &[BlankCase] = &[
        ("log_level", |v| PreferencesPatch { log_level: Some(v), ..Default::default() }),
        ("log_file_level", |v| PreferencesPatch { log_file_level: Some(v), ..Default::default() }),
        ("chart_style", |v| PreferencesPatch { chart_style: Some(v), ..Default::default() }),
    ];

    // Whitespace, not just "" — `check_non_blank` trims, and a key set to a space is the
    // shape somebody actually leaves behind.
    for (key, patch) in cases {
        for blank in ["", "   ", "\t"] {
            let err = Preferences::default()
                .apply(patch(blank.to_string()), file())
                .expect_err(&format!("{key} = {blank:?} must be refused, not accepted"));
            let msg = err.to_string();
            assert!(msg.contains(key), "the refusal must name the offending key: {msg}");
            assert!(
                msg.contains("blank — omit the key to keep the default"),
                "and must say what to do instead: {msg}"
            );
        }
    }
}

/// The zero guard was pinned; the `==` in it was not. A sweep flipped it to `!=`, which turns
/// every VALID worker count into a hard startup refusal — including for `vike-tradehub` — while
/// letting `0` through. Both existing sweep-thread tests survive that mutation: one passes `0`
/// (refused either way) and one passes `"lots"`, which dies in `parse_usize` before the guard
/// is ever reached. This is the positive-direction assertion neither of them makes.
#[test]
fn a_valid_sweep_thread_count_is_accepted() {
    let mut p = Preferences::default();
    p.apply_env(&HashMap::from([(SWEEP_THREADS_ENV.to_string(), "8".to_string())]))
        .expect("a valid worker count must load");
    assert_eq!(p.sweep_threads, Some(8));
}

#[test]
fn the_appearance_defaults_are_the_ruled_ones() {
    let p = Preferences::default();
    assert_eq!(
        (p.theme.as_str(), p.market_colors.as_str(), p.header_gradient),
        ("graphite", "classic", false)
    );
    assert_eq!((p.density.as_str(), p.text_size.as_str()), ("normal", "standard"));
}

#[test]
fn every_listed_appearance_word_is_accepted() {
    type Case = (&'static [&'static str], fn(String) -> PreferencesPatch, fn(&Preferences) -> &str);
    let cases: [Case; 4] = [
        (
            &THEMES,
            |v| PreferencesPatch { theme: Some(v), ..Default::default() },
            |p| p.theme.as_str(),
        ),
        (
            &MARKET_COLOR_SETS,
            |v| PreferencesPatch { market_colors: Some(v), ..Default::default() },
            |p| p.market_colors.as_str(),
        ),
        (
            &DENSITIES,
            |v| PreferencesPatch { density: Some(v), ..Default::default() },
            |p| p.density.as_str(),
        ),
        (
            &TEXT_SIZES,
            |v| PreferencesPatch { text_size: Some(v), ..Default::default() },
            |p| p.text_size.as_str(),
        ),
    ];
    for (words, patch, read) in cases {
        for w in words {
            let mut p = Preferences::default();
            p.apply(patch(w.to_string()), file()).unwrap_or_else(|e| panic!("{w}: {e}"));
            assert_eq!(read(&p), *w);
        }
    }
    let mut p = Preferences::default();
    p.apply(PreferencesPatch { header_gradient: Some(true), ..Default::default() }, file())
        .unwrap();
    assert!(p.header_gradient);
}

/// A word outside the list is refused BY KEY, naming the list, and never echoes the value: a
/// credential pasted into the wrong key must not be printed by the refusal.
#[test]
fn a_foreign_appearance_word_is_refused_by_key_without_echoing_it() {
    let secret = || "sk-live-SECRET".to_string();
    let cases = [
        ("theme", PreferencesPatch { theme: Some(secret()), ..Default::default() }),
        ("market_colors", PreferencesPatch { market_colors: Some(secret()), ..Default::default() }),
        ("density", PreferencesPatch { density: Some(secret()), ..Default::default() }),
        ("text_size", PreferencesPatch { text_size: Some(secret()), ..Default::default() }),
    ];
    for (key, patch) in cases {
        let msg = Preferences::default().apply(patch, file()).expect_err(key).to_string();
        assert!(msg.contains(key), "{msg}");
        assert!(msg.contains("must be one of:"), "{msg}");
        assert!(!msg.contains("SECRET"), "the refusal printed the value: {msg}");
    }
}

/// `preferences.max_order_qty` (decision 0111, phase P3): a finite cap above 0, or no row at all.
#[test]
fn the_max_order_qty_row_is_a_positive_finite_cap() {
    let mut p = Preferences::default();
    assert_eq!(p.max_order_qty, None, "no row is no cap");
    p.apply(PreferencesPatch { max_order_qty: Some(2.5), ..Default::default() }, file()).unwrap();
    assert_eq!(p.max_order_qty, Some(2.5));
    for bad in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        let err = Preferences::default()
            .apply(PreferencesPatch { max_order_qty: Some(bad), ..Default::default() }, file())
            .unwrap_err()
            .to_string();
        assert!(err.contains("max_order_qty"), "{bad}: {err}");
    }
}
