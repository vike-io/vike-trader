use super::*;
use std::assert_matches;

const SAMPLE: &str = r#"
store = "/data/tape"

[[subscribe]]
venue = "polymarket"
family = "btc-5m"
backfill = "archive"

[[subscribe]]
venue = "binance"
family = "*USDT-PERP"

[[subscribe]]
venue = "binance"
symbols = ["BTCUSDT", "ETHUSDT"]
backfill = "off"
"#;

/// The shape both venues share — spec §8.2's "families are general" decision, in config form.
/// Polymarket's family rotates its members every 5 minutes and Binance's is a static filter, but
/// the customer writes the same key for both.
#[test]
fn parses_families_and_explicit_symbols_with_one_shape() {
    let p = RecorderProfile::from_toml(SAMPLE).unwrap();
    assert_eq!(p.store, PathBuf::from("/data/tape"));
    assert_eq!(
        p.families(),
        vec![("polymarket".into(), "btc-5m".into()), ("binance".into(), "*USDT-PERP".into())]
    );
    assert_eq!(
        p.explicit_symbols(),
        vec![("binance".into(), "BTCUSDT".into()), ("binance".into(), "ETHUSDT".into())]
    );
}

/// Backfill is PER SUBSCRIPTION (spec §8.3), not a daemon-wide switch.
#[test]
fn backfill_is_per_subscription_and_defaults_to_venue() {
    let p = RecorderProfile::from_toml(SAMPLE).unwrap();
    assert_eq!(p.backfill_for("polymarket", "btc-5m"), Some(Backfill::Archive));
    assert_eq!(
        p.backfill_for("binance", "*USDT-PERP"),
        Some(Backfill::Venue),
        "omitted ⇒ the free venue-REST path"
    );
    assert_eq!(p.backfill_for("binance", "nope"), None);
}

/// A subscription naming nothing records nothing — rejected rather than silently ignored, which
/// is how a customer would otherwise find out days later that a venue has no tape.
#[test]
fn a_subscription_that_records_nothing_is_rejected() {
    let err = RecorderProfile::from_toml("store = \"/x\"\n\n[[subscribe]]\nvenue = \"okx\"\n")
        .unwrap_err();
    assert_matches!(err, ProfileError::Empty { index: 0, .. }, "{err}");
    assert!(format!("{err}").contains("record nothing"), "{err}");
}

/// Family AND symbols in one entry would store the same instrument twice — grouped by the
/// family and per-symbol by the list — so a read would return it from both layouts.
#[test]
fn family_plus_symbols_in_one_subscription_is_rejected() {
    let toml = "store = \"/x\"\n\n[[subscribe]]\nvenue = \"binance\"\nfamily = \"f\"\n\
                    symbols = [\"BTCUSDT\"]\n";
    let err = RecorderProfile::from_toml(toml).unwrap_err();
    assert_matches!(err, ProfileError::FamilyAndSymbols { index: 0, .. }, "{err}");
}

#[test]
fn the_same_family_twice_is_rejected() {
    let toml = "store = \"/x\"\n\n[[subscribe]]\nvenue = \"p\"\nfamily = \"f\"\n\n\
                    [[subscribe]]\nvenue = \"p\"\nfamily = \"f\"\nbackfill = \"archive\"\n";
    let err = RecorderProfile::from_toml(toml).unwrap_err();
    assert_matches!(err, ProfileError::DuplicateFamily { .. }, "{err}");
}

/// The same family name under DIFFERENT venues is legal — families are venue-scoped.
#[test]
fn the_same_family_name_under_two_venues_is_fine() {
    let toml = "store = \"/x\"\n\n[[subscribe]]\nvenue = \"a\"\nfamily = \"f\"\n\n\
                    [[subscribe]]\nvenue = \"b\"\nfamily = \"f\"\n";
    assert!(RecorderProfile::from_toml(toml).is_ok());
}

/// An empty profile is valid — a daemon with nothing subscribed yet is a legitimate state (the
/// customer has not picked anything in the Data Manager), not a misconfiguration.
#[test]
fn an_empty_profile_is_valid() {
    let p = RecorderProfile::from_toml("store = \"/x\"\n").unwrap();
    assert!(p.subscribe.is_empty());
    assert!(p.families().is_empty());
}

/// **An absent `[maintenance]` table means DEFAULTS, not OFF.** A recorder commits once per
/// buffer flush, so a busy series writes a part every few seconds — measured live, 23 parts in
/// 150 s for one family's book. Defaulting to "no compaction" would leave every customer who did
/// not know to ask for it with ~13,000 files a day per family per kind.
#[test]
fn an_absent_maintenance_table_means_defaults_not_off() {
    let p = RecorderProfile::from_toml("store = \"/x\"\n").unwrap();
    assert_eq!(p.maintenance, Maintenance::default());
    let (cfg, interval) = p.maintenance.scheduler_args().expect("on by default");
    assert_eq!(interval, Duration::from_secs(300));
    assert_eq!(cfg.compaction.min_parts, 4);
    assert!(cfg.retention.is_none(), "a recorder ACCUMULATES; pruning is always explicit");
}

#[test]
fn maintenance_knobs_parse_and_convert() {
    let toml = "store = \"/x\"\n\n[maintenance]\ninterval_secs = 60\nmin_parts = 8\n\
                    target_mb = 64\nretention_days = 7\n";
    let p = RecorderProfile::from_toml(toml).unwrap();
    let (cfg, interval) = p.maintenance.scheduler_args().unwrap();
    assert_eq!(interval, Duration::from_secs(60));
    assert_eq!(cfg.compaction.min_parts, 8);
    assert_eq!(cfg.compaction.target_bytes, 64 * 1024 * 1024);
    assert_eq!(cfg.retention.unwrap().max_age_ms, Some(7 * 86_400_000));
}

/// A partial table keeps the other defaults — turning one knob must not silently disable the
/// rest.
#[test]
fn a_partial_maintenance_table_keeps_the_other_defaults() {
    let p =
        RecorderProfile::from_toml("store = \"/x\"\n\n[maintenance]\nmin_parts = 16\n").unwrap();
    assert_eq!(p.maintenance.min_parts, 16);
    assert_eq!(p.maintenance.interval_secs, 300);
    assert_eq!(p.maintenance.target_mb, 384);
}

#[test]
fn interval_zero_disables_maintenance() {
    let p =
        RecorderProfile::from_toml("store = \"/x\"\n\n[maintenance]\ninterval_secs = 0\n").unwrap();
    assert!(p.maintenance.scheduler_args().is_none());
}

/// `retention_days = 0` prunes everything older than NOW — the tape being written, as it is
/// written. Almost certainly a typo for "keep forever", which is what omitting the key means.
#[test]
fn zero_retention_is_rejected_because_it_would_delete_the_tape_continuously() {
    let err = RecorderProfile::from_toml("store = \"/x\"\n\n[maintenance]\nretention_days = 0\n")
        .unwrap_err();
    assert_matches!(err, ProfileError::ZeroRetention, "{err}");
    assert!(err.to_string().contains("keep data forever"), "{err}");
}

#[test]
fn min_parts_below_two_is_rejected_as_pure_churn() {
    let err =
        RecorderProfile::from_toml("store = \"/x\"\n\n[maintenance]\nmin_parts = 1\n").unwrap_err();
    assert_matches!(err, ProfileError::MinPartsTooSmall { got: 1 }, "{err}");
}

/// ...but not while maintenance is off: validating a knob nothing will read would reject a
/// perfectly coherent "disabled, and I left the old numbers in place" profile.
#[test]
fn min_parts_is_not_validated_when_maintenance_is_disabled() {
    let toml = "store = \"/x\"\n\n[maintenance]\ninterval_secs = 0\nmin_parts = 1\n";
    assert!(RecorderProfile::from_toml(toml).is_ok());
}

/// `target_mb` bounds a merge's memory AND selects what to merge, so zero means "nothing is
/// ever compacted" — silently. An operator who wants that says `interval_secs = 0`.
#[test]
fn a_zero_target_size_is_rejected_rather_than_silently_disabling_compaction() {
    let err =
        RecorderProfile::from_toml("store = \"/x\"\n\n[maintenance]\ntarget_mb = 0\n").unwrap_err();
    assert_matches!(err, ProfileError::ZeroTargetSize, "{err}");
    let off = "store = \"/x\"\n\n[maintenance]\ninterval_secs = 0\ntarget_mb = 0\n";
    assert!(RecorderProfile::from_toml(off).is_ok(), "not validated while maintenance is off");
}

/// The memory knob reaches the same silent never-compacts through a different door.
#[test]
fn a_zero_row_budget_is_rejected_too() {
    let err = RecorderProfile::from_toml("store = \"/x\"\n\n[maintenance]\nmax_merge_rows = 0\n")
        .unwrap_err();
    assert_matches!(err, ProfileError::ZeroMaxMergeRows, "{err}");
}

/// **An absent `[alerting]` table means DEFAULTS, not OFF** — same argument as `[maintenance]`
/// above, and the sharper one: the condition the watchdog catches raises no error anywhere, so
/// a recorder that had to be TOLD to watch would be the one nobody told.
#[test]
fn an_absent_alerting_table_means_defaults_not_off() {
    let p = RecorderProfile::from_toml("store = \"/x\"\n").unwrap();
    assert_eq!(p.alerting, Alerting::default());
    assert_eq!(p.alerting.repeat_secs, 3600);
    assert!(p.alerting.webhooks.is_empty(), "log-only until a target is named");
    assert_eq!(p.alerting.series_prefix, None, "unscoped: every subscribed series");
}

#[test]
fn alerting_knobs_parse_and_a_partial_table_keeps_the_other_defaults() {
    let toml = "store = \"/x\"\n\n[alerting]\nwebhooks = [\"telegram\"]\n\
                    series_prefix = \"book/polymarket/\"\n";
    let p = RecorderProfile::from_toml(toml).unwrap();
    assert_eq!(p.alerting.webhooks, vec!["telegram".to_string()]);
    assert_eq!(p.alerting.series_prefix.as_deref(), Some("book/polymarket/"));
    assert_eq!(p.alerting.repeat_secs, 3600, "the untouched knob keeps its default");
}

/// `repeat_secs = 0` is legal and MEANS something (once per episode) — unlike the maintenance
/// zeros above, which silently disable the thing they configure. Nothing to reject here.
#[test]
fn a_zero_repeat_is_a_legal_value_not_a_disabled_one() {
    let p = RecorderProfile::from_toml("store = \"/x\"\n\n[alerting]\nrepeat_secs = 0\n").unwrap();
    assert_eq!(p.alerting.repeat_secs, 0);
}

/// **A MISSPELLED KEY IS REFUSED BY NAME, in every table.** Serde's default is to DROP an
/// unrecognised key, which on this file meant a one-character typo silently reconfigured the
/// daemon: each case below was accepted, and started, before `deny_unknown_fields`.
///
/// Driven per-table rather than once, because each table's `#[serde(default)]` fields make the
/// drop invisible in a DIFFERENT way — see each row's comment for what used to happen.
#[test]
fn a_misspelled_key_is_refused_by_name_in_every_table() {
    // (profile body, the typo'd key the message must name)
    let cases = [
        // Top level: `subscribe` dropped ⇒ a daemon that recorded NOTHING and said so nowhere
        // (an empty profile is deliberately valid, so validation could not catch it either).
        ("store = \"/x\"\n\n[[subscription]]\nvenue = \"binance\"\n", "subscription"),
        // Subscription: silently fell back to `Backfill::Venue` — which on Polymarket cannot
        // restore the book, the lie `Backfill`'s own doc says nothing may tell.
        (
            "store = \"/x\"\n\n[[subscribe]]\nvenue = \"p\"\nfamily = \"f\"\n\
                 backfil = \"archive\"\n",
            "backfil",
        ),
        // Maintenance: the operator asked for 30 days and kept the tape forever.
        ("store = \"/x\"\n\n[maintenance]\nretention_day = 30\n", "retention_day"),
        // Alerting: the operator believed a PAGER was armed; delivery stayed log-only.
        ("store = \"/x\"\n\n[alerting]\nwebhook = [\"telegram\"]\n", "webhook"),
    ];
    for (body, key) in cases {
        let err = RecorderProfile::from_toml(body)
            .expect_err("an unknown key must be refused, not dropped");
        assert_matches!(err, ProfileError::Parse(_), "{key}: {err:?}");
        let msg = err.to_string();
        assert!(msg.contains("unknown field"), "{key}: must say what is wrong — {msg}");
        assert!(msg.contains(key), "{key}: the refusal must NAME the offending key — {msg}");
    }
}

/// …and the refusal is ACTIONABLE, not merely correct: `toml` lists the keys it WOULD have
/// accepted, so the operator does not have to go and find this file to learn the spelling.
#[test]
fn the_refusal_lists_the_keys_that_would_have_been_accepted() {
    let err =
        RecorderProfile::from_toml("store = \"/x\"\n\n[alerting]\nwebhook = [\"telegram\"]\n")
            .unwrap_err()
            .to_string();
    for expected in ["webhooks", "repeat_secs", "series_prefix"] {
        assert!(err.contains(expected), "the message must offer `{expected}` — {err}");
    }
}

/// ⚠ THE GUARD ON THE ABOVE: the shipped example must still parse. A refusal that rejects the
/// file this repo tells operators to copy would be a worse bug than the one it fixes, and the
/// example is not otherwise compiled by anything.
#[test]
fn the_shipped_example_profile_still_parses() {
    let example = include_str!("../recorder.example.toml");
    let p = RecorderProfile::from_toml(example).expect("the shipped example must parse");
    assert_eq!(p.families(), vec![("polymarket".into(), "btc-updown-5m".into())]);
}

/// The knob reaches `CompactionConfig` — the whole point of the row bound is that the scheduler
/// actually receives it. (`target_mb` was plumbed and ignored for as long as it existed.)
#[test]
fn the_row_budget_reaches_the_compaction_config() {
    let p =
        RecorderProfile::from_toml("store = \"/x\"\n\n[maintenance]\nmax_merge_rows = 250000\n")
            .unwrap();
    let (cfg, _) = p.maintenance.scheduler_args().expect("maintenance enabled");
    assert_eq!(cfg.compaction.max_merge_rows, 250_000);
    assert_eq!(
        Maintenance::default().max_merge_rows,
        1_000_000,
        "the default is the documented one"
    );
}
