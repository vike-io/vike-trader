use super::*;
use vike_secrets::SettingRow;

/// The whole reason the rows go back through the patch types: a typo'd ROW KEY is refused BY
/// NAME, and the message names the store rather than a path an operator would open and find
/// innocent.
#[test]
fn an_unknown_row_key_is_refused_by_name_on_the_read_path() {
    let store = StoredSettings {
        settings: vec![SettingRow {
            section: "policy".into(),
            key: "max_levrage".into(),
            value: "3.0".into(),
        }],
        ..Default::default()
    };
    let mut settings = Settings::default();
    apply_rows(&mut settings, &store, None);
    let msg = settings.seal_refusal.clone().expect("an illegal row must be marked");
    assert!(msg.contains("max_levrage"), "the refusal must NAME the key: {msg}");
    assert!(
        msg.contains("settings database"),
        "…and the store it came from, not a file on disk: {msg}"
    );
}

/// A TOMBSTONE keeps its own message through the row path — `max_total_exposure` was DELETED
/// from `Policy` for being unread, and an operator who writes it must be told where the concept
/// went rather than "unknown field".
#[test]
fn a_tombstoned_key_keeps_its_own_refusal_through_the_rows() {
    let store = StoredSettings {
        settings: vec![SettingRow {
            section: "policy".into(),
            key: "max_total_exposure".into(),
            value: "500.0".into(),
        }],
        ..Default::default()
    };
    let mut settings = Settings::default();
    apply_rows(&mut settings, &store, None);
    let msg = settings.seal_refusal.clone().expect("a tombstone must be marked");
    assert!(msg.contains("max_total_exposure"), "{msg}");
    assert!(
        !msg.contains("unknown field"),
        "a tombstone must not degrade to the generic unknown-key message: {msg}"
    );
}

/// A BOUND is imported from `vike-model`, and the row path gets it because it runs the same
/// `apply`. No `CHECK` constraint restates it in SQL — that split-brain is what the typed model
/// refuses.
#[test]
fn a_bound_still_bites_through_the_rows() {
    let store = StoredSettings {
        settings: vec![SettingRow {
            section: "policy".into(),
            key: "market_slippage".into(),
            value: "0.9".into(),
        }],
        ..Default::default()
    };
    let mut settings = Settings::default();
    apply_rows(&mut settings, &store, None);
    let msg = settings.seal_refusal.expect("an out-of-bound value must be marked");
    assert!(msg.contains("market_slippage"), "{msg}");
}

/// An unknown VENUE id in the arming rows is refused too — `Policy::apply`'s hand-written
/// roster check is the gate `deny_unknown_fields` cannot be, and it reaches the rows because
/// the rows become that same `[venues]` table.
#[test]
fn an_unknown_venue_in_the_arming_rows_is_refused() {
    let store = StoredSettings {
        arming: vec![ArmingRow {
            venue: "not-a-venue".into(),
            label: None,
            mode: "live".into(),
            max_exposure: None,
        }],
        ..Default::default()
    };
    let mut settings = Settings::default();
    apply_rows(&mut settings, &store, None);
    let msg = settings.seal_refusal.expect("an unknown venue must be marked");
    assert!(msg.contains("not-a-venue"), "{msg}");
}

/// An account LABEL round trips, and a label the alphabet refuses is still refused BY NAME.
#[test]
fn an_account_label_round_trips_and_a_refused_label_is_still_named() {
    let store = StoredSettings {
        arming: vec![ArmingRow {
            venue: "hyperliquid".into(),
            label: Some("ALT2".into()),
            mode: "paper".into(),
            max_exposure: None,
        }],
        ..Default::default()
    };
    let mut settings = Settings::default();
    apply_rows(&mut settings, &store, None);
    assert!(settings.seal_refusal.is_none(), "{:?}", settings.seal_refusal);
    assert_eq!(
        settings.policy.venues.accounts_by_venue().get("hyperliquid").unwrap().get("ALT2"),
        Some(&crate::venue_mode::VenueMode::Paper)
    );

    // ...and a label the LABEL alphabet refuses is refused through the rows too, by name — the
    // same gate a written key hits, reached through the store.
    let refused = StoredSettings {
        arming: vec![ArmingRow {
            venue: "hyperliquid".into(),
            label: Some("my account".into()),
            mode: "paper".into(),
            max_exposure: None,
        }],
        ..Default::default()
    };
    let mut settings = Settings::default();
    apply_rows(&mut settings, &refused, None);
    let msg = settings.seal_refusal.expect("a refused label must be marked");
    assert!(msg.contains("my account"), "{msg}");
}

/// **A malformed stored value is named AT ITS OWN ROW, by key** — now MARKED rather than
/// returned, since there is no file underneath for the degrade to lean on any more.
#[test]
fn a_stale_row_is_marked_by_name_rather_than_stopping_the_boot() {
    for (section, key, value) in [
        ("config", "state_dir", r"'C:\vike\state'"),
        ("preferences", "log_file_level", "\"\"\"warn\"\"\""),
        ("policy", "max_leverage", "inf"),
        // Trailing input after a complete scalar: the injection a text splice also allowed, and
        // the reason each value is parsed as ONE document rather than spliced into an object.
        ("policy", "max_leverage", "3.0, \"max_notional_per_order\": 9e9"),
    ] {
        let store = StoredSettings {
            settings: vec![SettingRow {
                section: section.into(),
                key: key.into(),
                value: value.into(),
            }],
            ..Default::default()
        };
        let mut settings = Settings::default();
        apply_rows(&mut settings, &store, None);
        let msg = settings.seal_refusal.clone().unwrap_or_else(|| {
            panic!("an unreadable row must be MARKED; warnings: {:?}", settings.warnings)
        });
        assert!(msg.contains(key), "the warning must NAME the row's key: {msg}");
        assert!(msg.contains("settings database"), "…and the store it came from: {msg}");
        assert!(!msg.contains(r"C:\vike"), "the warning must not echo the value: {msg}");
        assert!(msg.contains("nightly backup"), "…and the ONE repair 0086 leaves: {msg}");

        // …and the LAYER is dropped whole rather than half-applied: this store's one row was
        // the only thing it had to say, so nothing of it reached the resolved settings.
        assert_eq!(
            settings.policy.max_leverage,
            Settings::default().policy.max_leverage,
            "an unreadable store must contribute NOTHING, not its parseable half"
        );
    }
}

/// **An ILLEGAL row still refuses (via the mark), and that half is what keeps the stale-row
/// disposition above honest.**
#[test]
fn an_integer_too_large_to_be_one_is_refused_rather_than_read_as_a_float() {
    let store = StoredSettings {
        settings: vec![SettingRow {
            section: "policy".into(),
            key: "max_notional_per_order".into(),
            value: "18446744073709551616".into(),
        }],
        ..Default::default()
    };
    let mut settings = Settings::default();
    apply_rows(&mut settings, &store, None);
    let msg = settings.seal_refusal.expect("an integer outside the store's range is ILLEGAL");
    assert!(msg.contains("max_notional_per_order"), "the refusal must NAME the key: {msg}");
    assert!(
        msg.contains("INTEGER outside the range"),
        "…and say what is wrong with it rather than wearing the stale-format hint: {msg}"
    );
}

/// **A `null` row is refused rather than read as "unset".** JSON's one expressive advantage
/// over TOML is the one thing this column must not accept: `deny_unknown_fields` sees a KNOWN
/// field name, and every patch field is an `Option`, so `null` would deserialize as absence and
/// a ceiling an operator filed would resolve to its default in silence.
#[test]
fn a_null_row_value_is_refused_rather_than_read_as_unset() {
    let store = StoredSettings {
        settings: vec![SettingRow {
            section: "policy".into(),
            key: "max_notional_per_order".into(),
            value: "null".into(),
        }],
        ..Default::default()
    };
    let mut settings = Settings::default();
    apply_rows(&mut settings, &store, None);
    let msg = settings.seal_refusal.expect("a null row must be marked");
    assert!(msg.contains("max_notional_per_order"), "{msg}");
    assert!(msg.contains("null"), "{msg}");

    // ...and the refusal is doing real work: serde itself accepts it, silently, as `None`.
    let object = serde_json::json!({ "max_notional_per_order": null });
    let patch: PolicyPatch = serde_json::from_value(object).unwrap();
    assert!(patch.max_notional_per_order.is_none(), "which is exactly the silent read above");
}

/// A key and a path THROUGH that key cannot both be set. A TOML document would have gotten the
/// refusal from its own parser; `UNIQUE (section, key)` cannot see it, so it is checked by hand.
#[test]
fn two_rows_claiming_one_place_are_refused_rather_than_one_winning() {
    let store = StoredSettings {
        settings: vec![
            SettingRow { section: "policy".into(), key: "rate".into(), value: "1".into() },
            SettingRow {
                section: "policy".into(),
                key: "rate.max_utilization".into(),
                value: "0.5".into(),
            },
        ],
        ..Default::default()
    };
    let mut settings = Settings::default();
    apply_rows(&mut settings, &store, None);
    let msg = settings.seal_refusal.expect("a collision must be marked");
    assert!(msg.contains("rate"), "{msg}");
}

/// A dotted row still becomes a NESTED object — the shape `PolicyPatch::rate` expects — so the
/// tombstone it is guarding reaches its own message rather than "unknown field".
#[test]
fn a_dotted_row_key_becomes_a_nested_object() {
    let store = StoredSettings {
        settings: vec![SettingRow {
            section: "policy".into(),
            key: "rate.max_utilization".into(),
            value: "0.5".into(),
        }],
        ..Default::default()
    };
    let values = section_values(&store).unwrap();
    assert_eq!(values["policy"], serde_json::json!({ "rate": { "max_utilization": 0.5 } }));
}

/// **A finite `f64` survives the JSON round trip BIT-EXACTLY, and so does the rendering this
/// replaced** — the two halves a ceiling depends on, swept rather than sampled.
///
/// 20 000 random bit patterns through a deterministic LCG, so the sweep is the same every run
/// and a failure is reproducible from the seed.
#[test]
fn a_finite_f64_survives_the_json_round_trip_bit_exactly() {
    let mut state: u64 = 0x2026_0918_0057;
    let mut swept = 0usize;
    for _ in 0..20_000 {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let x = f64::from_bits(state);
        if !x.is_finite() {
            continue;
        }
        swept += 1;

        let rendered = json_scalar(&toml::Value::Float(x)).expect("a finite float renders");
        let back: f64 = serde_json::from_str(&rendered).expect("…and parses back");
        assert_eq!(back.to_bits(), x.to_bits(), "{x:?} rendered as {rendered}");

        // The migration half: the bytes the column held BEFORE, read by the parser it holds now.
        let old = toml::Value::Float(x).to_string();
        let from_old: f64 = serde_json::from_str(&old)
            .unwrap_or_else(|e| panic!("the old rendering {old} is not JSON: {e}"));
        assert_eq!(from_old.to_bits(), x.to_bits(), "{x:?} stored as {old}");
    }
    assert!(swept > 15_000, "the sweep must reach real values, not mostly NaN: {swept}");
}

/// The arming rows become the two MAPS `PolicyPatch` expects, placed whole.
#[test]
fn the_arming_rows_become_the_two_policy_maps() {
    let store = StoredSettings {
        arming: vec![
            ArmingRow {
                venue: "binance".into(),
                label: None,
                mode: "demo".into(),
                max_exposure: None,
            },
            ArmingRow {
                venue: "hyperliquid".into(),
                label: Some("ALT".into()),
                mode: "paper".into(),
                max_exposure: None,
            },
        ],
        ..Default::default()
    };
    let values = section_values(&store).unwrap();
    assert_eq!(
        values["policy"],
        serde_json::json!({
            "venues": { "binance": "demo" },
            "accounts": { "hyperliquid": { "ALT": "paper" } },
        })
    );
}

/// **The seal's own integrity check runs whenever a seal exists, and marks rather than
/// returns.**
#[test]
fn a_seal_count_mismatch_is_marked_not_returned() {
    let store = StoredSettings {
        settings: vec![SettingRow {
            section: "policy".into(),
            key: "max_leverage".into(),
            value: "2.0".into(),
        }],
        ..Default::default()
    };
    let adoption = Adoption {
        adopted_at: String::new(),
        tool_version: String::new(),
        files_present: String::new(),
        venues_declared: false,
        setting_rows: 0, // sealed on ZERO, but the store now carries one
        arming_rows: 0,
    };
    let mut settings = Settings::default();
    apply_rows(&mut settings, &store, Some(&adoption));
    let msg = settings.seal_refusal.expect("a count mismatch must be marked");
    assert!(msg.contains("CHANGED"), "{msg}");
    assert!(msg.contains("nightly backup"), "{msg}");
    // The value must not have applied either — a seal refusal drops the whole layer.
    assert_eq!(settings.policy.max_leverage, Settings::default().policy.max_leverage);
}

/// A store with NO seal at all (never written to) runs no integrity check and applies cleanly —
/// the ordinary state of a fresh box.
#[test]
fn a_never_written_store_applies_with_no_seal_to_check() {
    let store = StoredSettings {
        settings: vec![SettingRow {
            section: "policy".into(),
            key: "max_leverage".into(),
            value: "2.0".into(),
        }],
        ..Default::default()
    };
    let mut settings = Settings::default();
    apply_rows(&mut settings, &store, None);
    assert!(settings.seal_refusal.is_none(), "{:?}", settings.seal_refusal);
    assert_eq!(settings.policy.max_leverage, 2.0);
}
