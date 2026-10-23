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
            msg.contains("`vike-cli config unset preferences.rate_utilization`")
                && msg.contains("seal"),
            "a stale row is removed with the verb that moves the adoption seal's count with it, \
             named with the exact key: {msg}"
        );
        assert!(
            !msg.contains("no sanctioned way")
                && !msg.contains("no verb deletes")
                && !msg.contains("it is owed")
                && !msg.contains("settings database itself"),
            "the refusal must not say the removal is owed, nor advise the hand DELETE the seal \
             refuses: {msg}"
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
/// letting `0` through. The zero-pool test survives that mutation (it passes `0`, refused either
/// way); this is the positive-direction assertion it does not make.
#[test]
fn a_valid_sweep_thread_count_is_accepted() {
    let mut p = Preferences::default();
    p.apply(PreferencesPatch { sweep_threads: Some(8), ..Default::default() }, file())
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

/// `preferences.export_dir` (decision 0111, phase P7): a directory, or no row at all — the desktop's
/// `<exe_dir>/exports`. A blank row is refused rather than read as the working directory.
#[test]
fn the_export_dir_row_is_a_directory_or_nothing() {
    let mut p = Preferences::default();
    assert_eq!(p.export_dir, None, "no row is the desktop's own default");
    p.apply(
        PreferencesPatch { export_dir: Some("/home/me/charts".into()), ..Default::default() },
        file(),
    )
    .unwrap();
    assert_eq!(p.export_dir, Some(std::path::PathBuf::from("/home/me/charts")));
    for blank in ["", "  "] {
        let err = Preferences::default()
            .apply(
                PreferencesPatch { export_dir: Some(blank.into()), ..Default::default() },
                file(),
            )
            .unwrap_err()
            .to_string();
        assert!(err.contains("export_dir"), "{blank:?}: {err}");
    }
}

/// The desktop's tuning rows and the Studio chat's carry TODAY's values as their defaults, so a box
/// with no row runs exactly what it ran before they existed.
#[test]
fn the_tuning_rows_default_to_the_values_they_replace() {
    let p = Preferences::default();
    assert_eq!(p.observe_poll_ms, 50, "the sleep the observe bridge always took");
    assert_eq!(p.chart_history_bars, 3_000, "the depth one chart read always asked for");
    assert_eq!(p.backfill_lookback_days, 30, "the window a bulk backfill always used");
    assert_eq!(p.ai_anthropic_model, "claude-opus-4-8");
    assert_eq!(p.ai_cerebras_model, "llama-3.3-70b");
    assert_eq!(p.ai_timeout_secs, 120, "the whole-request bound the chat always used");

    // …and every default is a value its own row accepts: a default the validator refused would be
    // a store that could not be written back to its starting point.
    let mut applied = Preferences::default();
    applied
        .apply(
            PreferencesPatch {
                observe_poll_ms: Some(p.observe_poll_ms),
                chart_history_bars: Some(p.chart_history_bars),
                backfill_lookback_days: Some(p.backfill_lookback_days),
                ai_anthropic_model: Some(p.ai_anthropic_model.clone()),
                ai_cerebras_model: Some(p.ai_cerebras_model.clone()),
                ai_timeout_secs: Some(p.ai_timeout_secs),
                ..Default::default()
            },
            file(),
        )
        .expect("every default is inside its own row's range");
    assert_eq!(applied, p);
}

/// One bounded whole-number tuning row: the patch that sets it, the field that reads it back, and
/// its closed range.
struct Bounded {
    key: &'static str,
    min: u32,
    max: u32,
    default: u32,
    patch: fn(u32) -> PreferencesPatch,
    read: fn(&Preferences) -> u32,
}

fn bounded_rows() -> [Bounded; 4] {
    [
        Bounded {
            key: "observe_poll_ms",
            min: MIN_OBSERVE_POLL_MS,
            max: MAX_OBSERVE_POLL_MS,
            default: DEFAULT_OBSERVE_POLL_MS,
            patch: |v| PreferencesPatch { observe_poll_ms: Some(v), ..Default::default() },
            read: |p| p.observe_poll_ms,
        },
        Bounded {
            key: "chart_history_bars",
            min: MIN_CHART_HISTORY_BARS,
            max: MAX_CHART_HISTORY_BARS,
            default: DEFAULT_CHART_HISTORY_BARS,
            patch: |v| PreferencesPatch { chart_history_bars: Some(v), ..Default::default() },
            read: |p| p.chart_history_bars,
        },
        Bounded {
            key: "backfill_lookback_days",
            min: MIN_BACKFILL_LOOKBACK_DAYS,
            max: MAX_BACKFILL_LOOKBACK_DAYS,
            default: DEFAULT_BACKFILL_LOOKBACK_DAYS,
            patch: |v| PreferencesPatch { backfill_lookback_days: Some(v), ..Default::default() },
            read: |p| p.backfill_lookback_days,
        },
        Bounded {
            key: "ai_timeout_secs",
            min: MIN_AI_TIMEOUT_SECS,
            max: MAX_AI_TIMEOUT_SECS,
            default: DEFAULT_AI_TIMEOUT_SECS,
            patch: |v| PreferencesPatch { ai_timeout_secs: Some(v), ..Default::default() },
            read: |p| p.ai_timeout_secs,
        },
    ]
}

/// The four ranges are the ones the owner ruled, spelled out so a bound that moves is a decision
/// made here and not a side effect of editing a constant.
#[test]
fn the_tuning_ranges_are_the_ruled_ones() {
    let ranges: Vec<(&str, u32, u32)> =
        bounded_rows().iter().map(|r| (r.key, r.min, r.max)).collect();
    assert_eq!(
        ranges,
        [
            ("observe_poll_ms", 10, 1_000),
            ("chart_history_bars", 300, 4_000),
            ("backfill_lookback_days", 1, 365),
            ("ai_timeout_secs", 30, 900),
        ]
    );
}

/// Every bounded tuning row takes its two bounds and its default, and REFUSES one step outside —
/// naming the key, the value and both bounds — without clamping: the refused field is still the
/// default afterwards.
#[test]
fn a_bounded_tuning_row_takes_its_bounds_and_refuses_one_step_outside() {
    for r in bounded_rows() {
        for ok in [r.min, r.default, r.max] {
            let mut p = Preferences::default();
            p.apply((r.patch)(ok), file()).unwrap_or_else(|e| panic!("{} = {ok}: {e}", r.key));
            assert_eq!((r.read)(&p), ok, "{}", r.key);
        }
        for bad in [r.min - 1, r.max + 1, u32::MAX] {
            let mut p = Preferences::default();
            let err = p.apply((r.patch)(bad), file()).unwrap_err().to_string();
            assert!(
                err.starts_with(&format!("preferences.toml: {} = {bad} is outside", r.key)),
                "the refusal names the file, the KEY and the value: {err}"
            );
            assert!(
                err.contains(&format!("{}..={}", r.min, r.max)),
                "{} = {bad}: it names both bounds: {err}",
                r.key
            );
            assert_eq!((r.read)(&p), r.default, "{} = {bad}: refused, never clamped", r.key);
        }
    }
}

/// The GUI bar store trims each series to a cap, and a chart read deeper than the cap would be
/// trimmed on the way in. The ceiling is therefore set by that cap, which lives above this crate:
/// `vike_app_core::data::data_sink`'s `DIRECT_BAR_CLOSED_CAP` is held above it by a `const`
/// assertion there. What this crate can pin is that the ceiling is the ruled number.
#[test]
fn the_chart_history_ceiling_is_the_ruled_one() {
    assert_eq!(MAX_CHART_HISTORY_BARS, 4_000);
    const { assert!(DEFAULT_CHART_HISTORY_BARS <= MAX_CHART_HISTORY_BARS) };
}

/// The patch that sets one provider's model-id row, by the row's key.
fn model_patch(key: &str, id: String) -> PreferencesPatch {
    match key {
        "ai_anthropic_model" => {
            PreferencesPatch { ai_anthropic_model: Some(id), ..Default::default() }
        }
        "ai_cerebras_model" => {
            PreferencesPatch { ai_cerebras_model: Some(id), ..Default::default() }
        }
        other => panic!("{other} is not a model-id row"),
    }
}

/// A model id is 1 to 100 characters of ASCII letters, digits and `._:/-`, for either provider's
/// row; the refusal names the key and never echoes the value.
#[test]
fn a_model_id_row_takes_a_well_shaped_id_and_refuses_everything_else() {
    let longest = "a".repeat(MAX_AI_MODEL_ID_CHARS);
    let too_long = "a".repeat(MAX_AI_MODEL_ID_CHARS + 1);
    for key in ["ai_anthropic_model", "ai_cerebras_model"] {
        for ok in ["claude-sonnet-4-5", "llama-3.3-70b", "meta/llama:70b", "a._:/-", "7"] {
            let mut p = Preferences::default();
            p.apply(model_patch(key, ok.to_string()), file())
                .unwrap_or_else(|e| panic!("{key} = {ok}: {e}"));
        }
        let mut p = Preferences::default();
        p.apply(model_patch(key, longest.clone()), file()).expect("a 100-character id is taken");

        for bad in [
            "",
            "   ",
            " claude-opus-4-8",
            "claude-opus-4-8 ",
            "claude opus",
            "claude\n",
            "claude;rm",
            "модель",
            "sk-secret value",
            too_long.as_str(),
        ] {
            let mut p = Preferences::default();
            let msg = p.apply(model_patch(key, bad.to_string()), file()).unwrap_err().to_string();
            assert!(msg.starts_with(&format!("preferences.toml: {key} = ")), "{bad:?}: {msg}");
            assert!(msg.contains("1 to 100 characters"), "{bad:?}: it names the rule: {msg}");
            assert!(!msg.contains("secret"), "{bad:?}: the refusal printed the value: {msg}");
            assert_eq!(
                (p.ai_anthropic_model.as_str(), p.ai_cerebras_model.as_str()),
                (DEFAULT_AI_ANTHROPIC_MODEL, DEFAULT_AI_CEREBRAS_MODEL),
                "{bad:?}: refused, never trimmed into shape"
            );
        }
    }
    // The two providers' rows are independent: setting one leaves the other at its default.
    let mut p = Preferences::default();
    p.apply(model_patch("ai_anthropic_model", "claude-x".into()), file()).unwrap();
    assert_eq!(p.ai_anthropic_model, "claude-x");
    assert_eq!(p.ai_cerebras_model, DEFAULT_AI_CEREBRAS_MODEL);
}

/// The two link-patience rows are RAISE-ONLY windows: the built-in value is the floor, and a row
/// below it is refused by key, at the write and at the load (both reach `apply`). The boundary
/// table is the same for both keys: below the floor, the floor, the ceiling, one over.
#[test]
fn the_link_patience_rows_are_raise_only_windows() {
    /// One row of the table: the key, its floor, its ceiling, how to build a patch setting it and
    /// how to read the applied value back.
    type PatienceCase =
        (&'static str, u32, u32, fn(u32) -> PreferencesPatch, fn(&Preferences) -> u32);
    let cases: [PatienceCase; 2] = [
        (
            "observe_stall_timeout_ms",
            DEFAULT_OBSERVE_STALL_TIMEOUT_MS,
            MAX_OBSERVE_STALL_TIMEOUT_MS,
            |v| PreferencesPatch { observe_stall_timeout_ms: Some(v), ..Default::default() },
            |p| p.observe_stall_timeout_ms,
        ),
        (
            "control_reply_timeout_ms",
            DEFAULT_CONTROL_REPLY_TIMEOUT_MS,
            MAX_CONTROL_REPLY_TIMEOUT_MS,
            |v| PreferencesPatch { control_reply_timeout_ms: Some(v), ..Default::default() },
            |p| p.control_reply_timeout_ms,
        ),
    ];
    for (key, floor, max, patch, read) in cases {
        for v in [floor, floor + 1, max] {
            let mut p = Preferences::default();
            p.apply(patch(v), file()).unwrap_or_else(|e| panic!("{key} = {v}: {e}"));
            assert_eq!(read(&p), v, "{key} = {v} must be applied as written, not clamped");
        }
        for v in [0, floor - 1] {
            let msg = Preferences::default().apply(patch(v), file()).expect_err(key).to_string();
            let head = format!("preferences.toml: {key} = {v} is below the minimum {floor}");
            assert!(msg.starts_with(&head), "{msg}");
            assert!(msg.contains("RAISED"), "{msg}");
        }
        let over = Preferences::default().apply(patch(max + 1), file()).expect_err(key).to_string();
        assert!(over.contains(key) && over.contains("exceeds the allowed maximum"), "{over}");
        assert!(over.contains(&max.to_string()), "{over}");
        // A refused row leaves the value alone (a half-applied patch would be a clamp by accident).
        let mut p = Preferences::default();
        assert!(p.apply(patch(floor - 1), file()).is_err());
        assert_eq!(read(&p), floor, "{key}: a refused row changed the value");
    }
}

/// A box with no link-patience row resolves today's windows, and the numbers are the shipped
/// ones: 45 s is three of the node's 15 s heartbeats, 30 s the control reply wait. The higher-layer
/// constants these restate are pinned in `vike-app-core`'s `observe_bridge_tests.rs`.
#[test]
fn the_link_patience_defaults_are_todays_windows() {
    let p = Preferences::default();
    assert_eq!(p.observe_stall_timeout_ms, 45_000);
    assert_eq!(p.control_reply_timeout_ms, 30_000);
    assert_eq!(p.observe_stall_timeout_ms, DEFAULT_OBSERVE_STALL_TIMEOUT_MS);
    assert_eq!(p.control_reply_timeout_ms, DEFAULT_CONTROL_REPLY_TIMEOUT_MS);
}

/// `preferences.vsync`: ON with no row, a boolean when there is one, and a patch that omits the key
/// leaves whatever is already resolved. It is the only expressible present-mode choice, so there is
/// no third value to test.
#[test]
fn the_vsync_row_defaults_on_and_a_boolean_row_moves_it_both_ways() {
    let mut p = Preferences::default();
    assert!(p.vsync, "no row is the library's own vsync");
    assert_eq!(p.vsync, DEFAULT_VSYNC);
    p.apply(PreferencesPatch { vsync: Some(false), ..Default::default() }, file()).unwrap();
    assert!(!p.vsync);
    p.apply(PreferencesPatch::default(), file()).unwrap();
    assert!(!p.vsync, "a patch without the key leaves it");
    p.apply(PreferencesPatch { vsync: Some(true), ..Default::default() }, file()).unwrap();
    assert!(p.vsync);
}

/// A row that is not a boolean is refused by the TYPE, so the same refusal stands at the write, at
/// the load and in a file: `PreferencesPatch` is the one parse shape all three share. A row is a
/// JSON scalar, so the values are JSON.
#[test]
fn a_non_boolean_vsync_value_does_not_parse() {
    for bad in ["\"yes\"", "1", "0", "\"false\"", "1.0", "null"] {
        let json = format!("{{\"vsync\": {bad}}}");
        let parsed = serde_json::from_str::<PreferencesPatch>(&json);
        // `null` is the one that parses (`Option`): it is refused one layer up, by the row loader
        // (`crates/vike-config/tests/mirror.rs`'s `a_null_row_is_refused_rather_than_resolving_to_the_default`),
        // so it must not be asserted as a type error HERE.
        if bad == "null" {
            assert_eq!(parsed.unwrap().vsync, None);
        } else {
            let err = parsed.expect_err(bad).to_string();
            assert!(err.contains("invalid type"), "{bad}: {err}");
        }
    }
    let ok: PreferencesPatch = serde_json::from_str("{\"vsync\": false}").unwrap();
    assert_eq!(ok.vsync, Some(false));
}
